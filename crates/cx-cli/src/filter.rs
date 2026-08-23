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

/// Words that name a test anywhere in a path.
///
/// Absent on purpose: `specs`. Plural "specs" is design documentation
/// more often than tests — 40 documents and zero tests on a real repo.
const TEST_WORDS: [&str; 3] = ["test", "tests", "spec"];

/// Words that name a test *directory* only. In a filename they describe
/// the subject rather than the kind: `web/e2e/login.js` is a test,
/// `docs/2026-04-27-web-e2e-design.md` is a document about one.
const TEST_DIR_WORDS: [&str; 3] = ["e2e", "mocks", "testdata"];

/// A test word means the same thing whichever separator surrounds it.
const SEPARATORS: [char; 4] = ['/', '_', '-', '.'];

/// Whether a path names a test, by convention alone — no language, build
/// system, or parser. Words count only as whole segments, so `foo_test.go`,
/// `foo-test.js`, `foo.test.ts`, `test_foo.py`, `tests.rs`, `__mocks__/`
/// and `e2e/` are tests while `latest.rs` and `contest_view.rs` are not.
///
/// The price of staying language-agnostic: a name only one toolchain
/// recognizes carries no test word, so pytest's `conftest.py` and JUnit's
/// `FooTest.java` are missed. Teaching cx to see them means teaching it
/// one ecosystem at a time.
fn is_test_path(path: &str) -> bool {
    let (dirs, file) = path.rsplit_once('/').unwrap_or(("", path));
    let names_a_test = |part: &str, words: &[&str]| {
        part.split(SEPARATORS)
            .any(|segment| words.iter().any(|w| segment.eq_ignore_ascii_case(w)))
    };
    names_a_test(dirs, &TEST_WORDS)
        || names_a_test(dirs, &TEST_DIR_WORDS)
        || names_a_test(file, &TEST_WORDS)
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

    /// The whole convention, in one table: what counts as a test, what
    /// does not, and the two costs of consulting no language at all —
    /// documents *about* tests are kept, and names only one toolchain
    /// knows (`conftest.py`, `FooTest.java`) go undetected.
    #[test]
    fn detects_tests_by_naming_convention() {
        let f = Filter::new(Path::new("/nonexistent"), HashMap::new(), true).unwrap();
        for (path, is_test) in [
            // Test directories, at any depth, any separator, any case.
            ("tests/end_to_end.rs", true),
            ("crates/cx-cli/tests/e2e.rs", true),
            ("test/helper.js", true),
            ("web/e2e/login.spec.js", true),
            ("spec/models/user_spec.rb", true),
            ("src/__tests__/button.tsx", true),
            ("src/__mocks__/fs.js", true),
            ("pkg/testdata/sample.json", true),
            ("Tests/Legacy.cs", true),
            // Test words as whole filename segments.
            ("src/parser_test.go", true),
            ("src/parser-test.js", true),
            ("ui/Button.test.tsx", true),
            ("api/test_client.py", true),
            ("src/tests.rs", true),
            // Test support exists only for tests, so it counts as one.
            ("crates/store/src/test_helpers.rs", true),
            // A test word inside a longer word is just a word.
            ("src/main.rs", false),
            ("src/testing.rs", false),
            ("src/latest.rs", false),
            ("src/contest_view.rs", false),
            ("src/attestation.go", false),
            // Documents about tests are not tests.
            ("docs/specs/2026-05-01-relay-design.md", false),
            ("docs/plans/2026-04-27-web-e2e.md", false),
            // The price of knowing no language.
            ("api/conftest.py", false),
            ("src/main/java/FooTest.java", false),
        ] {
            assert_eq!(f.exclusion(path, b"code").is_some(), is_test, "{path}");
        }
    }
}
