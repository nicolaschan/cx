//! The file-filtering stack (plan §3, "order matters"):
//! 1. `.gitattributes` linguist-generated / -vendored / -documentation
//! 2. binary detection on content (UTF-16/32 aware)
//! 3. ported linguist generated/vendored patterns
//! 4. prose, unless `--prose` asks to keep it
//! 5. data files, unless `--data` asks to keep them
//! 6. test files, unless `--include-tests` asks to keep them
//! 7. `.cxignore`
//!
//! The density backstop (in the report, not here) flags, it doesn't drop.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use content_inspector::{ContentType, inspect};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use tokei::LanguageType;

use crate::git::LinguistAttrs;
use crate::language;

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

/// Languages linguist types as prose, among those tokei recognises. The
/// twelve it lacks (Textile, Pod, RDoc, Creole, Wikitext, RMarkdown, …)
/// are rare enough to leave undetected rather than keep a second table.
const PROSE_LANGUAGES: [LanguageType; 6] = [
    LanguageType::AsciiDoc,
    LanguageType::Markdown,
    LanguageType::Mdx,
    LanguageType::Org,
    LanguageType::ReStructuredText,
    LanguageType::Text,
];

/// Conventional documents named without an extension, so tokei has no
/// language for them. Matched on the basename up to its first `.`, `-`
/// or `_`, so `COPYING.LESSER`, `LICENSE-MIT` and `README_zh` count.
const PROSE_FILENAMES: [&str; 8] = [
    "LICENSE",
    "LICENCE",
    "COPYING",
    "NOTICE",
    "README",
    "CHANGELOG",
    "AUTHORS",
    "CONTRIBUTORS",
];

/// Data formats, by extension — the sole route to them: tokei's
/// serialized-data languages (JSON, XML, SVG) have no filename or
/// shebang entries, and the tabular and line-delimited formats have no
/// tokei language at all. Config (YAML, TOML) and markup (HTML, CSS)
/// stay code — they are authored, not emitted — as does a language that
/// merely compiles *to* data (Jsonnet). SVG is an image that happens to
/// be text.
const DATA_EXTENSIONS: [&str; 8] = [
    "json", "xml", "svg", "csv", "tsv", "jsonl", "ndjson", "geojson",
];

/// Whether a blob is a prose document: a prose language by tokei's
/// table, or — only when no language is found at all, so `LICENSE.py`
/// is Python — a conventional extensionless document.
fn is_prose(path: &str, content: &[u8]) -> bool {
    match language::of(path, content) {
        Some(lang) => PROSE_LANGUAGES.contains(&lang),
        None => {
            let file = path.rsplit('/').next().unwrap_or(path);
            let stem = file.split(SEPARATORS).next().unwrap_or(file);
            PROSE_FILENAMES.iter().any(|n| stem.eq_ignore_ascii_case(n))
        }
    }
}

/// Whether a blob is serialized data rather than authored logic, by the
/// extension after the path's last dot. No data extension contains `/`,
/// so a dot inside a directory name can never match.
fn is_data(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| DATA_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

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
    include_tests: bool,
    prose: bool,
    data: bool,
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
        include_tests: bool,
        prose: bool,
        data: bool,
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
            include_tests,
            prose,
            data,
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
        if !self.prose && is_prose(path, content) {
            return Some("prose");
        }
        if !self.data && is_data(path) {
            return Some("data");
        }
        if !self.include_tests && is_test_path(path) {
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

    /// The default filter: tests, prose, and data excluded.
    fn filter() -> Filter {
        Filter::new(
            Path::new("/nonexistent"),
            HashMap::new(),
            false,
            false,
            false,
        )
        .unwrap()
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
        // UTF-16LE "hello" with BOM: full of NULs, but text. Named
        // `blob.bin` rather than `readme.txt` so this stays a
        // binary-detection test and doesn't also exercise the prose layer.
        let utf16 = b"\xff\xfeh\0e\0l\0l\0o\0";
        assert_eq!(f.exclusion("blob.bin", utf16), None);
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
        let f = Filter::new(Path::new("/nonexistent"), attrs, false, false, false).unwrap();
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
    fn drops_tests_unless_asked_to_include_them() {
        let including = Filter::new(
            Path::new("/nonexistent"),
            HashMap::new(),
            true,
            false,
            false,
        )
        .unwrap();
        for path in ["tests/e2e.rs", "src/parser_test.go", "web/app.spec.ts"] {
            assert_eq!(filter().exclusion(path, b"code"), Some("test"), "{path}");
            assert_eq!(including.exclusion(path, b"code"), None, "{path} included");
        }
    }

    /// The whole convention, in one table: what counts as a test, what
    /// does not, and the two costs of consulting no language at all —
    /// documents *about* tests are kept, and names only one toolchain
    /// knows (`conftest.py`, `FooTest.java`) go undetected.
    #[test]
    fn detects_tests_by_naming_convention() {
        let f = filter();
        for (path, is_test) in [
            // Test directories, at any depth, any separator, any case.
            ("tests/end_to_end.rs", true),
            ("crates/cx-cli/tests/e2e.rs", true),
            ("test/helper.js", true),
            ("web/e2e/login.spec.js", true),
            ("spec/models/user_spec.rb", true),
            ("src/__tests__/button.tsx", true),
            ("src/__mocks__/fs.js", true),
            ("pkg/testdata/fixture.rs", true),
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
            // Compare against the specific reason, not `is_some`: the
            // documents-about-tests rows are `.md`, which the earlier prose
            // layer catches, so `is_some` would conflate prose with test.
            assert_eq!(
                f.exclusion(path, b"code") == Some("test"),
                is_test,
                "{path}"
            );
        }
    }

    /// Prose is what tokei types as such plus the conventional
    /// extensionless documents; config and markup are code, and data
    /// has its own layer.
    #[test]
    fn skips_prose_unless_asked_to_keep_it() {
        let f = filter();
        for path in [
            "README.md",
            "docs/guide.markdown",
            "docs/page.mdx",
            "CHANGES.rst",
            "notes.txt",
            "book/ch1.adoc",
            "todo.org",
            "LICENSE",
            "COPYING",
            "COPYING.LESSER",
            "LICENSE-MIT",
            "README",
            "sub/NOTICE",
            "AUTHORS",
            "CONTRIBUTORS",
            "CHANGELOG",
            "ChangeLog",
        ] {
            assert_eq!(f.exclusion(path, b"words"), Some("prose"), "{path}");
        }
        for path in [
            "ci.yaml",
            "index.html",
            "Cargo.toml",
            "style.css",
            "LICENSE.py",
            "README.rs",
            "src/main.rs",
            "Makefile",
        ] {
            assert_eq!(f.exclusion(path, b"code"), None, "{path}");
        }

        let keep = Filter::new(
            Path::new("/nonexistent"),
            HashMap::new(),
            false,
            true,
            false,
        )
        .unwrap();
        assert_eq!(keep.exclusion("README.md", b"words"), None);
        assert_eq!(keep.exclusion("LICENSE", b"words"), None);
    }

    /// Data files — serialized data, not authored logic — leave the
    /// universe like prose does: JSON, XML, SVG by tokei's table, plus
    /// the tabular and line-delimited formats tokei has no language for.
    #[test]
    fn skips_data_files_unless_asked_to_keep_them() {
        let f = filter();
        for path in [
            "package.json",
            "tsconfig.json",
            "rows.csv",
            "data/rows.tsv",
            "events.jsonl",
            "events.ndjson",
            "map.geojson",
            "pom.xml",
            "logo.svg",
        ] {
            assert_eq!(f.exclusion(path, b"data"), Some("data"), "{path}");
        }
        // The extension is case-insensitive and must be the file's own:
        // a dot inside a directory name never matches, because no data
        // extension contains a slash.
        assert_eq!(f.exclusion("export/ROWS.CSV", b"data"), Some("data"));
        assert_eq!(f.exclusion("dir.v1/rows.csv", b"data"), Some("data"));
        assert_eq!(f.exclusion("data.json/notes.rs", b"code"), None);
        assert_eq!(f.exclusion("data.json/Makefile", b"code"), None);
        // Config and markup stay code, and so does a language that
        // merely compiles *to* data.
        for path in [
            "ci.yaml",
            "Cargo.toml",
            "index.html",
            "style.css",
            "conf.jsonnet",
        ] {
            assert_eq!(f.exclusion(path, b"code"), None, "{path}");
        }
        let keep = Filter::new(
            Path::new("/nonexistent"),
            HashMap::new(),
            false,
            false,
            true,
        )
        .unwrap();
        assert_eq!(keep.exclusion("package.json", b"data"), None);
        assert_eq!(keep.exclusion("rows.csv", b"data"), None);
    }

    /// A data file in a test directory is data first, mirroring prose:
    /// what a file is precedes where it lives.
    #[test]
    fn data_is_recognised_before_tests() {
        assert_eq!(
            filter().exclusion("pkg/testdata/sample.json", b"{}"),
            Some("data")
        );
    }

    /// A prose document about tests is prose first (prose precedes the
    /// test layer), whether or not tests are being scored.
    #[test]
    fn prose_is_recognised_before_tests() {
        let f = filter();
        assert_eq!(f.exclusion("docs/tests/plan.md", b"words"), Some("prose"));
    }
}
