//! Which paths cx is asked to look at — the `--glob` selection, applied
//! to path lists before a single blob is fetched.
//!
//! Distinct from [`crate::filter`], which decides whether a file cx *is*
//! looking at holds scorable code and says why when it doesn't. A path
//! outside the scope is absent rather than skipped: in no reference, no
//! scoring pass, and no skipped list.

use std::path::Path;

use anyhow::Result;
use ignore::overrides::{Override, OverrideBuilder};

pub struct Scope(Override);

impl Scope {
    /// Gitignore glob syntax, with `!` excluding and the last matching
    /// glob winning — `ignore`'s override matcher, which is ripgrep's
    /// `-g`. No globs admits every path.
    pub fn new(root: &Path, globs: &[String]) -> Result<Self> {
        let mut builder = OverrideBuilder::new(root);
        for glob in globs {
            builder.add(glob)?;
        }
        Ok(Scope(builder.build()?))
    }

    /// Whether a repo-relative path is in scope.
    ///
    /// Each enclosing directory is consulted before the file, outermost
    /// first, exactly as a walk of the tree would reach them: an exclude
    /// prunes the subtree under it — `!target` needs no `target/**`, and
    /// as in gitignore nothing inside a pruned directory can be let back
    /// in — while an include merely opens the directory, leaving the file
    /// free to be excluded deeper down. A scope holding any include
    /// rejects whatever none of them opened.
    ///
    /// Every candidate, the file included, is offered as a directory:
    /// that is what keeps `Override` from reporting its
    /// no-include-matched verdict as an exclude of its own.
    pub fn allows(&self, path: &str) -> bool {
        let dirs = path.match_indices('/').map(|(end, _)| &path[..end]);
        let mut opened = self.0.num_whitelists() == 0;
        for candidate in dirs.chain([path]) {
            let verdict = self.0.matched(candidate, true);
            if verdict.is_ignore() {
                return false;
            }
            opened |= verdict.is_whitelist();
        }
        opened
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(globs: &[&str]) -> Scope {
        let globs: Vec<String> = globs.iter().map(|g| (*g).to_owned()).collect();
        Scope::new(Path::new("/repo"), &globs).unwrap()
    }

    /// Each row is a scope and the verdict it gives every path — the
    /// whole rule in one table.
    #[test]
    fn selects_paths_the_way_gitignore_globs_read() {
        for (globs, path, allowed) in [
            // No globs: the whole repo.
            (&[][..], "src/main.rs", true),
            // An include admits its matches and nothing else.
            (&["src/**"][..], "src/main.rs", true),
            (&["src/**"][..], "src/deep/nested.rs", true),
            (&["src/**"][..], "docs/guide.md", false),
            // A glob with a `/` is anchored at the repo root; one
            // without matches at any depth.
            (&["src/**"][..], "crates/a/src/main.rs", false),
            (&["*.rs"][..], "crates/a/src/main.rs", true),
            (&["*.rs"][..], "crates/a/src/main.py", false),
            // An exclude drops its matches and keeps the rest.
            (&["!docs/**"][..], "src/main.rs", true),
            (&["!docs/**"][..], "docs/guide.md", false),
            // A bare directory name covers its whole subtree, either way.
            (&["crates/cx-cli"][..], "crates/cx-cli/src/main.rs", true),
            (&["crates/cx-cli"][..], "crates/cx-core/src/lib.rs", false),
            (&["!target"][..], "target/debug/build.rs", false),
            (&["!target"][..], "src/main.rs", true),
            // A carve-out inside an include, named as a glob or as a
            // directory.
            (&["src/**", "!src/legacy/**"][..], "src/main.rs", true),
            (
                &["src/**", "!src/legacy/**"][..],
                "src/legacy/old.rs",
                false,
            ),
            (&["src/**", "!src/legacy"][..], "src/legacy/old.rs", false),
            // The gitignore rule cuts the other way too: an excluded
            // directory is final, so a later include cannot reach inside
            // one. Scope the exclude more narrowly instead.
            (
                &["!vendor", "vendor/ours/**"][..],
                "vendor/ours/a.rs",
                false,
            ),
            (&["!vendor/lib/**"][..], "vendor/ours/a.rs", true),
            (&["!vendor/lib/**"][..], "vendor/lib/b.rs", false),
            // Several includes union.
            (&["src/**", "docs/**"][..], "docs/guide.md", true),
            (&["src/**", "docs/**"][..], "Cargo.toml", false),
            // A file at the root, with and without includes present.
            (&["!*.md"][..], "README.md", false),
            (&["!*.md"][..], "Cargo.toml", true),
        ] {
            assert_eq!(scope(globs).allows(path), allowed, "{globs:?} on {path}");
        }
    }

    #[test]
    fn rejects_an_unparsable_glob() {
        assert!(Scope::new(Path::new("/repo"), &["src/[".to_owned()]).is_err());
    }
}
