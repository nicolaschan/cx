//! Thin wrappers over the five git plumbing commands the tool needs.
//! Shelling out is deliberate (no libgit2/gix): the surface is tiny and
//! `git` is always present where cx runs.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub struct Git {
    root: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Added,
    Modified,
    Deleted,
    Renamed { from: String },
}

#[derive(Clone, Debug)]
pub struct Change {
    pub path: String,
    pub status: Status,
}

impl Git {
    /// The repository containing the current directory.
    pub fn discover() -> Result<Self> {
        Self::discover_at(Path::new("."))
    }

    /// The repository containing `dir`.
    pub fn discover_at(dir: &Path) -> Result<Self> {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("running git")?;
        if !out.status.success() {
            bail!("not inside a git repository");
        }
        let root = String::from_utf8(out.stdout)?.trim().to_owned();
        Ok(Git {
            root: PathBuf::from(root),
        })
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    /// Resolve a revision, or None if it doesn't exist.
    pub fn resolve(&self, rev: &str) -> Result<Option<String>> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ])
            .output()?;
        if out.status.success() {
            Ok(Some(String::from_utf8(out.stdout)?.trim().to_owned()))
        } else {
            Ok(None)
        }
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        let out = self.run(&["merge-base", a, b])?;
        Ok(String::from_utf8(out)?.trim().to_owned())
    }

    /// All file paths in a tree.
    pub fn ls_tree(&self, rev: &str) -> Result<Vec<String>> {
        let out = self.run(&["ls-tree", "-r", "-z", "--name-only", rev])?;
        Ok(split_nul(&out))
    }

    /// Shared so both listings below describe the same diff, renames
    /// included.
    fn diff(&self, format: &str, from: &str, staged: bool) -> Result<Vec<String>> {
        let mut args = vec!["diff", format, "-z", "--find-renames"];
        if staged {
            args.push("--cached");
        }
        args.push(from);
        if !staged {
            args.push("HEAD");
        }
        Ok(split_nul(&self.run(&args)?))
    }

    /// Changed files between `from` and either HEAD or the index.
    pub fn changes(&self, from: &str, staged: bool) -> Result<Vec<Change>> {
        let fields = self.diff("--name-status", from, staged)?;
        let mut changes = Vec::new();
        let mut it = fields.into_iter();
        while let Some(status) = it.next() {
            let path = it.next().context("truncated --name-status output")?;
            match status.chars().next().context("empty status")? {
                'A' => changes.push(Change {
                    path,
                    status: Status::Added,
                }),
                // A copy carries two paths like a rename; the source stays
                // in the tree, so only the destination is a change.
                'C' => {
                    let to = it.next().context("copy without target path")?;
                    changes.push(Change {
                        path: to,
                        status: Status::Added,
                    });
                }
                'M' | 'T' => changes.push(Change {
                    path,
                    status: Status::Modified,
                }),
                'D' => changes.push(Change {
                    path,
                    status: Status::Deleted,
                }),
                'R' => {
                    let to = it.next().context("rename without target path")?;
                    changes.push(Change {
                        path: to,
                        status: Status::Renamed { from: path },
                    });
                }
                _ => {} // unmerged/unknown: nothing scorable
            }
        }
        Ok(changes)
    }

    /// Lines (added, deleted) per path, keyed as [`Git::changes`] keys them.
    pub fn line_counts(&self, from: &str, staged: bool) -> Result<HashMap<String, (u64, u64)>> {
        // `-` for a binary file, which the filter drops before summing.
        let count = |s: &str| s.parse().unwrap_or(0);
        let mut counts = HashMap::new();
        let mut fields = self.diff("--numstat", from, staged)?.into_iter();
        while let Some(record) = fields.next() {
            let (added, rest) = record.split_once('\t').context("numstat without a tab")?;
            let (deleted, path) = rest.split_once('\t').context("numstat without a path")?;
            // A rename leaves that path empty and follows with its
            // source and then its destination.
            let path = if path.is_empty() {
                fields.nth(1).context("rename without paths")?
            } else {
                path.to_owned()
            };
            counts.insert(path, (count(added), count(deleted)));
        }
        Ok(counts)
    }

    /// Bulk-fetch blob contents. Specs are object names (`<rev>:<path>`,
    /// `:0:<path>`, …); returns None per spec that doesn't resolve or
    /// names a non-blob (e.g. a submodule's commit object).
    pub fn blobs(&self, specs: &[String]) -> Result<Vec<Option<Vec<u8>>>> {
        // The batch protocol delimits requests with newlines, so a path
        // containing one would silently resolve wrong objects.
        if let Some(bad) = specs.iter().find(|s| s.contains('\n')) {
            bail!("path contains a newline, refusing to score: {bad:?}");
        }
        let mut child = Command::new("git")
            .current_dir(&self.root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("spawning git cat-file --batch")?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input: Vec<u8> = specs
            .iter()
            .flat_map(|s| s.bytes().chain(std::iter::once(b'\n')))
            .collect();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let out = child.wait_with_output()?;
        writer
            .join()
            .expect("writer thread")
            .context("writing to cat-file")?;
        if !out.status.success() {
            bail!("git cat-file --batch failed");
        }

        let mut blobs = Vec::with_capacity(specs.len());
        let mut rest = out.stdout.as_slice();
        while !rest.is_empty() {
            let nl = rest
                .iter()
                .position(|&b| b == b'\n')
                .context("truncated batch header")?;
            let header = std::str::from_utf8(&rest[..nl])?;
            rest = &rest[nl + 1..];
            if header.ends_with(" missing") || header.ends_with(" ambiguous") {
                blobs.push(None);
                continue;
            }
            let (kind, size) = {
                let mut parts = header.rsplit(' ');
                let size: usize = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .with_context(|| format!("unparsable batch header: {header}"))?;
                (parts.next().unwrap_or_default().to_owned(), size)
            };
            let content = rest
                .get(..size)
                .with_context(|| format!("truncated batch content for: {header}"))?;
            // Only blobs are file content; a gitlink resolves to a commit
            // object whose text must not leak into references.
            blobs.push((kind == "blob").then(|| content.to_vec()));
            rest = rest
                .get(size + 1..) // content + trailing newline
                .context("truncated batch trailer")?;
        }
        if blobs.len() != specs.len() {
            bail!(
                "cat-file returned {} objects for {} specs",
                blobs.len(),
                specs.len()
            );
        }
        Ok(blobs)
    }

    /// linguist-* attributes for the given paths, via `git check-attr`
    /// (reads .gitattributes the same way GitHub's linguist does).
    pub fn linguist_attrs(&self, paths: &[String]) -> Result<HashMap<String, LinguistAttrs>> {
        const ATTRS: [&str; 3] = [
            "linguist-generated",
            "linguist-vendored",
            "linguist-documentation",
        ];
        let mut child = Command::new("git")
            .current_dir(&self.root)
            .args(["check-attr", "-z", "--stdin"])
            .args(ATTRS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("spawning git check-attr")?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input: Vec<u8> = paths
            .iter()
            .flat_map(|p| p.bytes().chain(std::iter::once(b'\0')))
            .collect();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let out = child.wait_with_output()?;
        writer
            .join()
            .expect("writer thread")
            .context("writing to check-attr")?;
        if !out.status.success() {
            bail!("git check-attr failed");
        }

        let mut attrs: HashMap<String, LinguistAttrs> = HashMap::new();
        let fields = split_nul(&out.stdout);
        for triple in fields.as_chunks::<3>().0 {
            let (path, attr, value) = (&triple[0], &triple[1], &triple[2]);
            let set = matches!(value.as_str(), "set" | "true");
            let entry = attrs.entry(path.clone()).or_default();
            match attr.as_str() {
                "linguist-generated" => entry.generated = set,
                "linguist-vendored" => entry.vendored = set,
                "linguist-documentation" => entry.documentation = set,
                _ => {}
            }
        }
        Ok(attrs)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LinguistAttrs {
    pub generated: bool,
    pub vendored: bool,
    pub documentation: bool,
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}
