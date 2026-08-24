//! The code-only view of a file: comments removed, blank lines dropped.
//! Which bytes are comments comes from tokei's per-language table (line
//! and block delimiters, nesting, string quotes), so a `//` inside a
//! string literal is code and `r#"/* … */"#` survives intact.

use crate::language;

/// `content` with its comments removed and whitespace-only lines dropped,
/// or `content` itself when `path` and `content` name no language tokei
/// knows.
pub fn code_only(path: &str, content: Vec<u8>) -> Vec<u8> {
    match Syntax::of(path, &content) {
        Some(syntax) => syntax.strip(&content),
        None => content,
    }
}

/// As much of a language's lexical shape as telling comments from code
/// needs. Every slice borrows tokei's static table.
struct Syntax {
    line: &'static [&'static str],
    block: &'static [(&'static str, &'static str)],
    /// Whether `block` comments nest.
    nested: bool,
    /// Block comments that always nest (D's `/+ +/`).
    nested_block: &'static [(&'static str, &'static str)],
    quotes: &'static [(&'static str, &'static str)],
    verbatim: &'static [(&'static str, &'static str)],
    /// Docstrings: strings that are documentation when they stand alone
    /// as a statement.
    doc: &'static [(&'static str, &'static str)],
}

/// What a token at the current position opens.
#[derive(Clone, Copy)]
enum Opener {
    Line,
    Block {
        open: Option<&'static str>,
        close: &'static str,
    },
    Str {
        close: &'static str,
        verbatim: bool,
    },
}

enum State {
    Code,
    Line,
    /// `open` is the token that deepens the nesting, when the comment nests.
    Block {
        open: Option<&'static str>,
        close: &'static str,
        depth: usize,
    },
    Str {
        close: &'static str,
        verbatim: bool,
    },
}

impl Syntax {
    fn of(path: &str, content: &[u8]) -> Option<Self> {
        let lang = language::of(path, content)?;
        Some(Syntax {
            line: lang.line_comments(),
            block: lang.multi_line_comments(),
            nested: lang.allows_nested(),
            nested_block: lang.nested_comments(),
            quotes: lang.quotes(),
            verbatim: lang.verbatim_quotes(),
            doc: lang.doc_quotes(),
        })
    }

    /// The longest opener starting at `rest`, so `"""` beats `"` and
    /// `r#"` beats `"`. A doc-quote is a comment only at the start of a
    /// statement; elsewhere it's just a (longer) string quote. On a tie,
    /// a comment beats a string quote (checked first, kept on equal
    /// length) — swallowing one line wrong is cheaper than swallowing
    /// the rest of the file.
    fn opener_at(&self, rest: &[u8], statement_start: bool) -> Option<(usize, Opener)> {
        let mut best: Option<(usize, Opener)> = None;
        let mut consider = |token: &'static str, opener: Opener| {
            if rest.starts_with(token.as_bytes()) && best.is_none_or(|(len, _)| token.len() > len) {
                best = Some((token.len(), opener));
            }
        };
        for &t in self.line {
            consider(t, Opener::Line);
        }
        for &(o, c) in self.block {
            consider(
                o,
                Opener::Block {
                    open: self.nested.then_some(o),
                    close: c,
                },
            );
        }
        for &(o, c) in self.nested_block {
            consider(
                o,
                Opener::Block {
                    open: Some(o),
                    close: c,
                },
            );
        }
        for &(o, c) in self.quotes {
            consider(
                o,
                Opener::Str {
                    close: c,
                    verbatim: false,
                },
            );
        }
        for &(o, c) in self.verbatim {
            consider(
                o,
                Opener::Str {
                    close: c,
                    verbatim: true,
                },
            );
        }
        for &(o, c) in self.doc {
            let opener = if statement_start {
                Opener::Block {
                    open: None,
                    close: c,
                }
            } else {
                Opener::Str {
                    close: c,
                    verbatim: false,
                }
            };
            consider(o, opener);
        }
        best
    }

    fn strip(&self, src: &[u8]) -> Vec<u8> {
        let mut out = Out::with_capacity(src.len());
        let mut state = State::Code;
        let mut i = 0;
        while i < src.len() {
            let rest = &src[i..];
            match state {
                State::Code => match self.opener_at(rest, out.statement_start) {
                    Some((len, Opener::Line)) => {
                        state = State::Line;
                        i += len;
                    }
                    Some((len, Opener::Block { open, close })) => {
                        state = State::Block {
                            open,
                            close,
                            depth: 1,
                        };
                        i += len;
                    }
                    Some((len, Opener::Str { close, verbatim })) => {
                        out.push(&rest[..len]);
                        state = State::Str { close, verbatim };
                        i += len;
                    }
                    None => {
                        out.push(&rest[..1]);
                        i += 1;
                    }
                },
                State::Line => {
                    if rest[0] == b'\n' {
                        out.push(b"\n");
                        state = State::Code;
                    }
                    i += 1;
                }
                State::Block { open, close, depth } => {
                    if rest.starts_with(close.as_bytes()) {
                        i += close.len();
                        state = if depth == 1 {
                            State::Code
                        } else {
                            State::Block {
                                open,
                                close,
                                depth: depth - 1,
                            }
                        };
                    } else if let Some(open) = open.filter(|o| rest.starts_with(o.as_bytes())) {
                        i += open.len();
                        state = State::Block {
                            open: Some(open),
                            close,
                            depth: depth + 1,
                        };
                    } else {
                        if rest[0] == b'\n' {
                            out.push(b"\n");
                        }
                        i += 1;
                    }
                }
                State::Str { close, verbatim } => {
                    if !verbatim && rest[0] == b'\\' {
                        let n = rest.len().min(2);
                        out.push(&rest[..n]);
                        i += n;
                    } else if rest.starts_with(close.as_bytes()) {
                        out.push(close.as_bytes());
                        state = State::Code;
                        i += close.len();
                    } else {
                        out.push(&rest[..1]);
                        i += 1;
                    }
                }
            }
        }
        without_blank_lines(&out.bytes)
    }
}

/// Output under construction, tracking whether the current line so far
/// holds nothing but whitespace — where a docstring is a statement.
struct Out {
    bytes: Vec<u8>,
    statement_start: bool,
}

impl Out {
    fn with_capacity(capacity: usize) -> Self {
        Out {
            bytes: Vec::with_capacity(capacity),
            statement_start: true,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.statement_start = b == b'\n' || (self.statement_start && b.is_ascii_whitespace());
        }
        self.bytes.extend_from_slice(bytes);
    }
}

/// Each line right-trimmed, whitespace-only lines gone, every line
/// newline-terminated (so CRLF and a missing final newline normalize too).
fn without_blank_lines(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for line in text.split(|&b| b == b'\n') {
        let end = line
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |p| p + 1);
        if end > 0 {
            out.extend_from_slice(&line[..end]);
            out.push(b'\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(path: &str, src: &str) -> String {
        String::from_utf8(code_only(path, src.as_bytes().to_vec())).unwrap()
    }

    #[test]
    fn strips_line_and_block_comments_across_syntax_families() {
        for (path, src, want) in [
            (
                "a.rs",
                "// top\nfn f() {} // trailing\n/* block\n   more */\nfn g() {}\n",
                "fn f() {}\nfn g() {}\n",
            ),
            (
                "a.py",
                "# top\nx = 1  # trailing\ny = 2\n",
                "x = 1\ny = 2\n",
            ),
            (
                "a.lua",
                "-- line\nlocal a = 1 --[[ block\nstill ]] local b = 2\n",
                "local a = 1\n local b = 2\n",
            ),
            (
                "a.hs",
                "{- outer {- inner -} still outer -}\nmain = x\n",
                "main = x\n",
            ),
            ("a.ml", "(* c *)\nlet x = 1\n", "let x = 1\n"),
            ("a.html", "<!-- c -->\n<p>hi</p>\n", "<p>hi</p>\n"),
            ("a.d", "/+ nests /+ here +/ +/\nint x;\n", "int x;\n"),
        ] {
            assert_eq!(strip(path, src), want, "{path}");
        }
    }

    #[test]
    fn comment_markers_inside_strings_are_code() {
        assert_eq!(
            strip("a.rs", "let u = \"http://x\"; // real\n"),
            "let u = \"http://x\";\n"
        );
        assert_eq!(
            strip("a.rs", "let s = r#\"/* kept */ \\\"#; /* gone */\n"),
            "let s = r#\"/* kept */ \\\"#;\n"
        );
        assert_eq!(
            strip("a.py", "s = 'it\\'s # not'  # is\n"),
            "s = 'it\\'s # not'\n"
        );
    }

    #[test]
    fn doc_quote_off_statement_is_a_longer_string_quote() {
        assert_eq!(
            strip("a.py", "x = \"\"\"a\" # c\"\"\"\ny = 1\n"),
            "x = \"\"\"a\" # c\"\"\"\ny = 1\n"
        );
    }

    #[test]
    fn docstrings_are_comments_only_in_statement_position() {
        assert_eq!(
            strip(
                "a.py",
                "def f():\n    \"\"\"Doc\n    more\"\"\"\n    return 1\n"
            ),
            "def f():\n    return 1\n"
        );
        assert_eq!(
            strip("a.py", "x = \"\"\"kept\nboth\"\"\"\n"),
            "x = \"\"\"kept\nboth\"\"\"\n"
        );
    }

    #[test]
    fn unterminated_comment_or_string_runs_to_the_end() {
        assert_eq!(
            strip("a.rs", "fn f() {}\n/* never closed\nfn g() {}\n"),
            "fn f() {}\n"
        );
        assert_eq!(
            strip("a.rs", "let s = \"open // not\nfn g() {}\n"),
            "let s = \"open // not\nfn g() {}\n"
        );
    }

    #[test]
    fn blank_and_whitespace_only_lines_are_dropped() {
        assert_eq!(
            strip("a.rs", "\n\nfn f() {}\n   \n\t\nfn g() {}   \n\n"),
            "fn f() {}\nfn g() {}\n"
        );
        assert_eq!(
            strip("a.rs", "fn f() {}\r\nfn g() {}"),
            "fn f() {}\nfn g() {}\n"
        );
    }

    #[test]
    fn unknown_language_is_returned_untouched() {
        let src = b"// looks like a comment\n\n".to_vec();
        assert_eq!(code_only("notes.unknownext", src.clone()), src);
        assert_eq!(code_only("Makefile", b"# c\nall:\n".to_vec()), b"all:\n");
    }

    #[test]
    fn shebang_decides_the_language_when_the_path_does_not() {
        assert_eq!(
            code_only("bin/run", b"#!/bin/sh\n# c\necho hi\n".to_vec()),
            b"echo hi\n"
        );
    }

    #[test]
    fn line_comment_beats_string_quote_on_a_tie() {
        assert_eq!(
            strip("a.vim", "let s = \"x\" | echo s\n\" comment\nlet t = 1\n"),
            "let s =\nlet t = 1\n"
        );
    }
}
