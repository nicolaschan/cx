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

/// Directories that a test runner owns wholesale, by name alone.
///
/// Deliberately absent: `specs`. Plural "specs" is design documentation
/// more often than it is tests — it matched 40 design documents and zero
/// tests on a real repo — while spec-named *files* are still caught by
/// [`TEST_WORDS`] wherever they live.
const TEST_DIRS: [&str; 7] = [
    "test",
    "tests",
    "spec",
    "e2e",
    "__tests__",
    "__mocks__",
    "testdata",
];

/// Words that name a test file. Matched only as whole segments of the
/// filename, split on the separators every ecosystem builds these names
/// from — never as substrings, so `latest.rs` and `contest_view.rs` are
/// production code.
const TEST_WORDS: [&str; 4] = ["test", "tests", "spec", "specs"];

const SEGMENT_SEPARATORS: [char; 3] = ['_', '-', '.'];

/// Whether a path names a test, by naming convention only — no language,
/// build system, or parser is consulted. This covers `foo_test.go`,
/// `foo-test.js`, `foo.test.ts`, `foo.spec.js`, `test_foo.py`, a bare
/// `tests.rs`, and anything under a test directory, because they are all
/// the same convention wearing different separators.
///
/// The deliberate cost of staying language-agnostic: names that only a
/// specific toolchain recognizes are *not* tests here — pytest's
/// `conftest.py` and JUnit's camelCase `FooTest.java` have no test word
/// to find, and identifying them would mean teaching cx one ecosystem at
/// a time.
fn is_test_path(path: &str) -> bool {
    let mut components = path.split('/').collect::<Vec<_>>();
    let file = components.pop().unwrap_or_default().to_ascii_lowercase();
    let in_test_dir = components
        .iter()
        .any(|c| TEST_DIRS.contains(&c.to_ascii_lowercase().as_str()));
    in_test_dir
        || file
            .split(SEGMENT_SEPARATORS)
            .any(|segment| TEST_WORDS.contains(&segment))
}

pub struct Filter {
    attrs: HashMap<String, LinguistAttrs>,
    patterns: GlobSet,
    ignore_tests: bool,
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
            ignore_tests,
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
        if self.ignore_tests && is_test_path(path) {
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

    fn ignoring_tests() -> Filter {
        Filter::new(Path::new("/nonexistent"), HashMap::new(), true).unwrap()
    }

    /// One convention in many separators — no language is consulted, so
    /// a name cx has never seen still classifies correctly.
    #[test]
    fn ignores_test_naming_conventions() {
        let f = ignoring_tests();
        for path in [
            // Test directories, at any depth.
            "tests/end_to_end.rs",
            "crates/cx-cli/tests/e2e.rs",
            "test/helper.js",
            "web/e2e/login.spec.js",
            "spec/models/user_spec.rb",
            "src/__tests__/button.tsx",
            "src/__mocks__/fs.js",
            "pkg/testdata/sample.json",
            "Tests/Legacy.cs", // case-insensitive
            // Test words as whole filename segments, any separator.
            "src/parser_test.go",
            "app/views_test.gleam",
            "src/parser-test.js",
            "ui/Button.test.tsx",
            "web/app.spec.ts",
            "api/test_client.py",
            "src/tests.rs", // bare module name
            "src/spec.lua",
            // Test *support* counts as test: it exists only for tests,
            // and the skipped list makes the call visible.
            "crates/store/src/test_helpers.rs",
        ] {
            assert_eq!(f.exclusion(path, b"code"), Some("test"), "{path}");
        }
    }

    #[test]
    fn ignoring_tests_spares_production_code() {
        let f = ignoring_tests();
        for path in [
            "src/main.rs",
            // Test words only count as whole segments.
            "src/testing.rs",
            "src/latest.rs",
            "src/contest_view.rs",
            "src/protest.py",
            "src/attestation.go",
            // Plural "specs" is design documentation far more often than
            // it is tests — a real repo had 40 of these and zero tests.
            "docs/specs/2026-05-01-relay-design.md",
        ] {
            assert_eq!(f.exclusion(path, b"code"), None, "{path}");
        }
    }

    /// The price of refusing language-specific knowledge: names that only
    /// one toolchain recognizes carry no test word, so cx keeps them.
    /// Documented rather than special-cased — the alternative is teaching
    /// cx one ecosystem at a time.
    #[test]
    fn toolchain_specific_names_are_not_detected() {
        let f = ignoring_tests();
        for path in ["api/conftest.py", "src/main/java/FooTest.java"] {
            assert_eq!(f.exclusion(path, b"code"), None, "{path}");
        }
    }
}
