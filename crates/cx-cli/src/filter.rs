//! The file-filtering stack (plan §3, "order matters"):
//! 1. `.gitattributes` linguist-generated / -vendored / -documentation
//! 2. binary detection on content (UTF-16/32 aware)
//! 3. ported linguist generated/vendored patterns
//! 4. test files, when `--ignore-tests` asks for it
//! 5. `.cxignore`
//!
//! The density backstop (layer 6) lives in the report, not here — it
//! flags, it doesn't drop.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use content_inspector::{ContentType, inspect};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::git::LinguistAttrs;

/// Patterns ported from linguist's generated.rb / vendor.yml — the ~15
/// that cover 95% of real repos.
const LINGUIST_PATTERNS: [&str; 15] = [
    "*.lock",
    "package-lock.json",
    "go.sum",
    "*.min.js",
    "*.min.css",
    "*.map",
    "*_pb2.py",
    "*.pb.go",
    "*_generated.*",
    "*.generated.*",
    "*.snap",
    "__snapshots__/**",
    "dist/**",
    "vendor/**",
    "node_modules/**",
];

/// Test-file conventions across the languages cx is likely to meet.
/// Directory names match at any depth; filename patterns are the ones
/// each ecosystem's runner actually keys on, so they are as reliable as
/// the runner's own discovery.
/// Deliberately absent: `specs/**`. Plural "specs" is more often design
/// documentation than RSpec-style tests (it swallowed 40 design docs and
/// zero tests on a real repo); spec-named *files* are caught by the
/// filename patterns wherever they live.
const TEST_PATTERNS: [&str; 15] = [
    // Directories a test runner owns wholesale.
    "test/**",
    "tests/**",
    "spec/**",
    "e2e/**",
    "__tests__/**",
    "__mocks__/**",
    "testdata/**",
    // Filenames: Go/Rust/Gleam, Python, JS/TS, Ruby, JUnit-style.
    "*_test.*",
    "*_tests.*",
    "test_*.*",
    "*.test.*",
    "*.spec.*",
    "*_spec.*",
    "conftest.py",
    "*Test.java",
];

pub struct Filter {
    attrs: HashMap<String, LinguistAttrs>,
    patterns: GlobSet,
    tests: Option<GlobSet>,
    cxignore: Option<Gitignore>,
}

/// Build a set matching each pattern at the repo root and at any depth.
fn glob_set(patterns: &[&str]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
        builder.add(Glob::new(&format!("**/{pattern}"))?);
    }
    Ok(builder.build()?)
}

impl Filter {
    pub fn new(
        root: &Path,
        attrs: HashMap<String, LinguistAttrs>,
        ignore_tests: bool,
    ) -> Result<Self> {
        let cxignore_path = root.join(".cxignore");
        let cxignore = cxignore_path.exists().then(|| {
            let mut b = GitignoreBuilder::new(root);
            b.add(cxignore_path);
            b.build()
        });
        Ok(Filter {
            attrs,
            patterns: glob_set(&LINGUIST_PATTERNS)?,
            tests: ignore_tests.then(|| glob_set(&TEST_PATTERNS)).transpose()?,
            cxignore: cxignore.transpose()?,
        })
    }

    /// Why a file is excluded from scoring and references, or None to keep.
    pub fn exclusion(&self, path: &str, content: &[u8]) -> Option<&'static str> {
        if let Some(a) = self.attrs.get(path) {
            if a.generated {
                return Some("linguist-generated");
            }
            if a.vendored {
                return Some("linguist-vendored");
            }
            if a.documentation {
                return Some("linguist-documentation");
            }
        }
        let head = &content[..content.len().min(8192)];
        if inspect(head) == ContentType::BINARY {
            return Some("binary");
        }
        if self.patterns.is_match(path) {
            return Some("generated/vendored pattern");
        }
        if self.tests.as_ref().is_some_and(|t| t.is_match(path)) {
            return Some("test");
        }
        if let Some(ig) = &self.cxignore
            && ig.matched(path, false).is_ignore()
        {
            return Some(".cxignore");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> Filter {
        Filter::new(Path::new("/nonexistent"), HashMap::new(), false).unwrap()
    }

    #[test]
    fn drops_lockfiles_and_vendored_anywhere_in_tree() {
        let f = filter();
        assert_eq!(
            f.exclusion("Cargo.lock", b"x"),
            Some("generated/vendored pattern")
        );
        assert_eq!(
            f.exclusion("web/package-lock.json", b"x"),
            Some("generated/vendored pattern")
        );
        assert_eq!(
            f.exclusion("a/b/vendor/lib.js", b"x"),
            Some("generated/vendored pattern")
        );
        assert_eq!(
            f.exclusion("assets/app.min.js", b"x"),
            Some("generated/vendored pattern")
        );
    }

    #[test]
    fn drops_binary_but_not_utf16_text_as_binary() {
        let f = filter();
        assert_eq!(
            f.exclusion("img.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some("binary")
        );
        // UTF-16LE "hello" with BOM: full of NULs, but text.
        let utf16 = b"\xff\xfeh\0e\0l\0l\0o\0";
        assert_eq!(f.exclusion("readme.txt", utf16), None);
    }

    #[test]
    fn respects_linguist_attributes() {
        let mut attrs = HashMap::new();
        attrs.insert(
            "src/schema.rs".to_owned(),
            LinguistAttrs {
                generated: true,
                ..Default::default()
            },
        );
        let f = Filter::new(Path::new("/nonexistent"), attrs, false).unwrap();
        assert_eq!(
            f.exclusion("src/schema.rs", b"code"),
            Some("linguist-generated")
        );
        assert_eq!(f.exclusion("src/other.rs", b"code"), None);
    }

    #[test]
    fn keeps_ordinary_code() {
        let f = filter();
        assert_eq!(f.exclusion("src/main.rs", b"fn main() {}"), None);
    }

    #[test]
    fn keeps_tests_unless_asked_to_ignore_them() {
        let f = filter();
        for path in ["tests/e2e.rs", "src/parser_test.go", "web/app.spec.ts"] {
            assert_eq!(f.exclusion(path, b"code"), None, "{path} kept by default");
        }
    }

    #[test]
    fn ignores_test_conventions_across_ecosystems() {
        let f = Filter::new(Path::new("/nonexistent"), HashMap::new(), true).unwrap();
        for path in [
            "tests/end_to_end.rs",
            "crates/cx-cli/tests/e2e.rs",
            "test/helper.js",
            "web/e2e/login.spec.js",
            "spec/models/user_spec.rb",
            "src/__tests__/button.tsx",
            "src/parser_test.go",
            "app/views_test.gleam",
            "api/test_client.py",
            "api/conftest.py",
            "ui/Button.test.tsx",
            // Test *support* counts as test: it exists only for tests,
            // and the skipped list makes the call visible.
            "crates/store/src/test_helpers.rs",
            "pkg/testdata/sample.json",
            "src/main/java/FooTest.java",
        ] {
            assert_eq!(f.exclusion(path, b"code"), Some("test"), "{path}");
        }
    }

    #[test]
    fn ignoring_tests_spares_production_code() {
        let f = Filter::new(Path::new("/nonexistent"), HashMap::new(), true).unwrap();
        for path in [
            "src/main.rs",
            "src/testing.rs",      // not a test: no separator
            "src/latest.rs",       // substring "test" mid-word
            "src/contest_view.rs", // ditto
            "src/protest.py",
            // Plural "specs" is design documentation far more often than
            // it is tests — a real repo had 40 of these and zero tests.
            "docs/specs/2026-05-01-relay-design.md",
        ] {
            assert_eq!(f.exclusion(path, b"code"), None, "{path}");
        }
    }
}
