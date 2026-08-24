//! Which language a blob is written in, decided from its path and its
//! bytes alone — never from the working tree, which may hold a different
//! version of the file or none at all.

use std::path::Path;

use tokei::{Config, LanguageType};

/// A directory that cannot exist. tokei's lookup by name is a pure table
/// walk except for one step: a name with no extension makes it open the
/// file to read a shebang. Anchoring the name here keeps the filename and
/// extension tables and turns that read into a miss, so the shebang is
/// taken from `content` instead. (An absolute path whose first component
/// contains a NUL byte can never name a real file: opening it fails at
/// the CString step, before any lookup touches disk.)
const NOWHERE: &str = "/\0";

/// Interpreters a shebang may name, by the name they go by there. Not an
/// extension lookup: `m4` is not ObjectiveC's `.m`, `v` is not Coq's `.v`.
const INTERPRETERS: &[(&str, LanguageType)] = &[
    ("sh", LanguageType::Sh),
    ("bash", LanguageType::Bash),
    ("zsh", LanguageType::Zsh),
    ("ksh", LanguageType::Ksh),
    ("fish", LanguageType::Fish),
    ("csh", LanguageType::CShell),
    ("tcsh", LanguageType::CShell),
    ("dash", LanguageType::Sh),
    ("ash", LanguageType::Sh),
    ("python", LanguageType::Python),
    ("perl", LanguageType::Perl),
    ("ruby", LanguageType::Ruby),
    ("node", LanguageType::JavaScript),
    ("lua", LanguageType::Lua),
    ("php", LanguageType::Php),
    ("awk", LanguageType::AWK),
    ("raku", LanguageType::Raku),
    ("perl6", LanguageType::Raku),
];

pub fn of(path: &str, content: &[u8]) -> Option<LanguageType> {
    let name = path.rsplit('/').next().unwrap_or(path);
    LanguageType::from_path(Path::new(NOWHERE).join(name), &Config::default())
        .or_else(|| from_shebang(content))
}

/// `#!/usr/bin/env python3` and `#!/usr/bin/python3` both name python.
fn from_shebang(content: &[u8]) -> Option<LanguageType> {
    let first_line = content.strip_prefix(b"#!")?.split(|&b| b == b'\n').next()?;
    let mut words = std::str::from_utf8(first_line).ok()?.split_whitespace();
    let program = words.next()?;
    let program = match program.rsplit('/').next()? {
        "env" => words.find(|w| !w.starts_with('-'))?,
        program => program,
    };
    // Try the exact name first, so "perl6" (Raku's old name) isn't mistaken
    // for a version-numbered "perl". Only then trim a trailing version like
    // the "3" in "python3" or "3.11".
    by_name(program)
        .or_else(|| by_name(program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.')))
}

fn by_name(interpreter: &str) -> Option<LanguageType> {
    INTERPRETERS
        .iter()
        .find(|(name, _)| *name == interpreter)
        .map(|(_, lang)| *lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_extension_and_filename() {
        assert_eq!(of("src/main.rs", b""), Some(LanguageType::Rust));
        assert_eq!(of("Makefile", b""), Some(LanguageType::Makefile));
        assert_eq!(of("a/b/Dockerfile", b""), Some(LanguageType::Dockerfile));
        assert_eq!(of("README", b"words"), None);
    }

    #[test]
    fn by_shebang_when_the_name_says_nothing() {
        for (path, content, want) in [
            ("bin/run", "#!/bin/sh\n", Some(LanguageType::Sh)),
            ("bin/run", "#!/usr/bin/env bash\n", Some(LanguageType::Bash)),
            (
                "bin/run",
                "#!/usr/bin/env python3\n",
                Some(LanguageType::Python),
            ),
            (
                "bin/run",
                "#!/usr/bin/python3.11\n",
                Some(LanguageType::Python),
            ),
            (
                "bin/run",
                "#!/usr/bin/env node\n",
                Some(LanguageType::JavaScript),
            ),
            ("bin/run", "#!/usr/bin/perl\n", Some(LanguageType::Perl)),
            ("bin/run", "#!/usr/bin/env ruby\n", Some(LanguageType::Ruby)),
            ("bin/run", "#!/usr/bin/perl6\n", Some(LanguageType::Raku)),
            (
                "bin/run",
                "#!/usr/bin/env -S python3 -u\n",
                Some(LanguageType::Python),
            ),
            // An interpreter tokei has no shebang mapping for is a miss, not
            // a guess from its first letter as a file extension (`m4` is not
            // ObjectiveC's `.m`).
            ("bin/run", "#!/usr/bin/env m4\n", None),
        ] {
            assert_eq!(of(path, content.as_bytes()), want, "{path} {content:?}");
        }
    }

    #[test]
    fn never_reads_the_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("run");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();

        let path = file.to_str().unwrap();
        assert_eq!(
            of(path, b"#!/usr/bin/env python3\n"),
            Some(LanguageType::Python)
        );
    }
}
