//! Which paths cx is asked to look at — where the run is rooted and the
//! `--glob` selection, applied to path lists before a single blob is
//! fetched.
//!
//! Distinct from [`crate::filter`], which decides whether a file cx *is*
//! looking at holds scorable code and says why when it doesn't. A path
//! outside the scope is absent rather than skipped: in no reference, no
//! scoring pass, and no skipped list.

use std::path::Path;

use anyhow::Result;
use ignore::overrides::{Override, OverrideBuilder};

pub struct Scope {
    /// Where the run is rooted, relative to the repository root: empty
    /// at the root, otherwise a directory with its trailing slash, so
    /// that stripping it from a path both tests and performs the move
    /// into the run's own frame.
    base: String,
    globs: Override,
}

impl Scope {
    /// A run rooted at `base` — the repo-relative directory cx was run
    /// from — selecting within it by gitignore glob syntax, with `!`
    /// excluding and the last matching glob winning: `ignore`'s override
    /// matcher, which is ripgrep's `-g`. Globs read from `base`, like
    /// every other path a run names. No globs admits the whole subtree.
    pub fn new(root: &Path, base: &str, globs: &[String]) -> Result<Self> {
        let mut builder = OverrideBuilder::new(root.join(base));
        for glob in globs {
            builder.add(glob)?;
        }
        Ok(Scope {
            base: base.to_owned(),
            globs: builder.build()?,
        })
    }

    /// Whether a repo-relative path is part of this run: inside the
    /// directory it is rooted at, and selected by the globs there.
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
        let Some(path) = path.strip_prefix(&self.base) else {
            return false;
        };
        let dirs = path.match_indices('/').map(|(end, _)| &path[..end]);
        let mut opened = self.globs.num_whitelists() == 0;
        for candidate in dirs.chain([path]) {
            let verdict = self.globs.matched(candidate, true);
            if verdict.is_ignore() {
                return false;
            }
            opened |= verdict.is_whitelist();
        }
        opened
    }

    /// The name this run gives a repo-relative path: relative to the
    /// directory the run is rooted at, which is how every path it
    /// reports is written. Paths it never looked at keep the name they
    /// have in the repository.
    pub fn name(&self, path: &str) -> String {
        path.strip_prefix(&self.base).unwrap_or(path).to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooted(base: &str, globs: &[&str]) -> Scope {
        let globs: Vec<String> = globs.iter().map(|g| (*g).to_owned()).collect();
        Scope::new(Path::new("/repo"), base, &globs).unwrap()
    }

    fn scope(globs: &[&str]) -> Scope {
        rooted("", globs)
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

    /// A run rooted below the repository root sees that subtree and
    /// nothing else, and reads its globs from where it stands.
    #[test]
    fn a_run_below_the_root_sees_only_its_own_subtree() {
        for (base, globs, path, allowed) in [
            // The base alone selects, as `-g 'sub/**'` would.
            ("sub/", &[][..], "sub/main.rs", true),
            ("sub/", &[][..], "other/lib.rs", false),
            ("sub/", &[][..], "README.md", false),
            // A sibling that merely starts the same way is not inside,
            // however deep the directory the run was started in.
            ("sub/", &[][..], "subtle/x.rs", false),
            ("crates/cli/", &[][..], "crates/cli/main.rs", true),
            ("crates/cli/", &[][..], "crates/climate/x.rs", false),
            // Globs read from where the run stands, not from the root.
            ("sub/", &["src/**"][..], "sub/src/main.rs", true),
            ("sub/", &["src/**"][..], "sub/build.rs", false),
            ("sub/", &["*.rs"][..], "sub/src/main.rs", true),
            ("sub/", &["!src/**"][..], "sub/src/main.rs", false),
            ("sub/", &["!src/**"][..], "sub/build.rs", true),
            // Which is why a glob anchored at the repository root names
            // nothing: the path it describes is not inside this run.
            ("sub/", &["sub/**"][..], "sub/src/main.rs", false),
        ] {
            assert_eq!(
                rooted(base, globs).allows(path),
                allowed,
                "{base:?} {globs:?} on {path}"
            );
        }
    }

    /// What the run calls a path: relative to where it is rooted, so a
    /// run at the repo root names paths exactly as git does.
    #[test]
    fn names_paths_relative_to_where_the_run_is_rooted() {
        let cli = rooted("crates/cx-cli/", &[]);
        assert_eq!(cli.name("crates/cx-cli/src/main.rs"), "src/main.rs");
        assert_eq!(
            scope(&[]).name("crates/cx-cli/src/main.rs"),
            "crates/cx-cli/src/main.rs"
        );
    }

    #[test]
    fn rejects_an_unparsable_glob() {
        assert!(Scope::new(Path::new("/repo"), "", &["src/[".to_owned()]).is_err());
    }
}
