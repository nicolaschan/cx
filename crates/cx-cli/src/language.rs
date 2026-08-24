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

/// How an interpreter names itself in a shebang versus the extension of
/// the files it runs, where the two differ.
const INTERPRETERS: [(&str, &str); 4] = [
    ("python", "py"),
    ("perl", "pl"),
    ("ruby", "rb"),
    ("node", "js"),
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
        "env" => words.next()?,
        program => program,
    };
    let interpreter = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    let extension = INTERPRETERS
        .iter()
        .find(|(name, _)| *name == interpreter)
        .map_or(interpreter, |(_, ext)| ext);
    LanguageType::from_file_extension(extension)
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
            ("bin/run", "#!/bin/sh\n", LanguageType::Sh),
            ("bin/run", "#!/usr/bin/env bash\n", LanguageType::Bash),
            ("bin/run", "#!/usr/bin/env python3\n", LanguageType::Python),
            ("bin/run", "#!/usr/bin/python3.11\n", LanguageType::Python),
            ("bin/run", "#!/usr/bin/env node\n", LanguageType::JavaScript),
            ("bin/run", "#!/usr/bin/perl\n", LanguageType::Perl),
            ("bin/run", "#!/usr/bin/env ruby\n", LanguageType::Ruby),
        ] {
            assert_eq!(
                of(path, content.as_bytes()),
                Some(want),
                "{path} {content:?}"
            );
        }
    }

    #[test]
    fn never_reads_the_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("run");
        std::fs::write(&file, "#!/usr/bin/env node\n").unwrap();

        let path = file.to_str().unwrap();
        assert_eq!(of(path, b"#!/bin/sh\n"), Some(LanguageType::Sh));
    }
}
