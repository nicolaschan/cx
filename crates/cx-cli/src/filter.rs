//! The file-filtering stack (plan §3, "order matters"):
//! 1. `.gitattributes` linguist-generated / linguist-vendored
//! 2. binary detection on content (UTF-16/32 aware)
//! 3. ported linguist generated/vendored patterns
//! 4. `.cxignore`
//!
//! The density backstop (layer 5) lives in the report, not here — it
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

pub struct Filter {
    attrs: HashMap<String, LinguistAttrs>,
    patterns: GlobSet,
    cxignore: Option<Gitignore>,
}

impl Filter {
    pub fn new(root: &Path, attrs: HashMap<String, LinguistAttrs>) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pattern in LINGUIST_PATTERNS {
            builder.add(Glob::new(pattern)?);
            builder.add(Glob::new(&format!("**/{pattern}"))?);
        }
        let cxignore_path = root.join(".cxignore");
        let cxignore = cxignore_path.exists().then(|| {
            let mut b = GitignoreBuilder::new(root);
            b.add(cxignore_path);
            b.build()
        });
        Ok(Filter {
            attrs,
            patterns: builder.build()?,
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
        }
        let head = &content[..content.len().min(8192)];
        if inspect(head) == ContentType::BINARY {
            return Some("binary");
        }
        if self.patterns.is_match(path) {
            return Some("generated/vendored pattern");
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
        Filter::new(Path::new("/nonexistent"), HashMap::new()).unwrap()
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
        let f = Filter::new(Path::new("/nonexistent"), attrs).unwrap();
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
}
