//! The code-only view of a file: comments removed, string literals
//! emptied to their delimiters, blank lines dropped. Which bytes are
//! which comes from tokei's per-language table (line and block
//! delimiters, nesting, string quotes), so a `//` inside a string
//! literal opens no comment and `r#"…"#` still closes at `"#`.

use crate::language;

/// Which stripped-by-default byte classes a run scores anyway. `Default`
/// is the code-only view: comments out, string contents out.
#[derive(Clone, Copy, Default)]
pub struct Keep {
    pub comments: bool,
    pub strings: bool,
}

/// `content` with its comments removed (unless kept), each string
/// literal reduced to its delimiters (unless kept — the string counts,
/// its contents do not), and whitespace-only lines dropped. With
/// everything kept, or when `path` and `content` name no language tokei
/// knows, `content` passes through untouched.
pub fn code_only(path: &str, content: Vec<u8>, keep: Keep) -> Vec<u8> {
    match Syntax::of(path, &content) {
        Some(syntax) if !(keep.comments && keep.strings) => syntax.strip(&content, keep),
        _ => content,
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

/// A comment or string that swallows bytes until a closing token (or, for
/// `Line`, a newline). `Block` and `Str` also serve as the token an
/// opener at the start of `Code` resolves to; `Block`'s `depth` starts at
/// 1 there and only changes once inside the comment.
#[derive(Clone, Copy)]
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

impl State {
    /// Just inside a block comment's opener: depth 1, deepened by `open`
    /// when the comment nests.
    fn block(open: Option<&'static str>, close: &'static str) -> Self {
        State::Block {
            open,
            close,
            depth: 1,
        }
    }

    fn string(close: &'static str, verbatim: bool) -> Self {
        State::Str { close, verbatim }
    }
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

    /// The state entered by the longest opener starting at `rest`, so
    /// `"""` beats `"` and `r#"` beats `"`. A doc-quote is a comment only
    /// at the start of a statement; elsewhere it's just a (longer)
    /// string quote. On a tie, a comment beats a string quote (checked
    /// first, kept on equal length) — swallowing one line wrong is
    /// cheaper than swallowing the rest of the file.
    fn opener_at(&self, rest: &[u8], statement_start: bool) -> Option<(usize, State)> {
        let mut best: Option<(usize, State)> = None;
        let mut consider = |token: &'static str, state: State| {
            if rest.starts_with(token.as_bytes()) && best.is_none_or(|(len, _)| token.len() > len) {
                best = Some((token.len(), state));
            }
        };
        for &t in self.line {
            consider(t, State::Line);
        }
        for &(o, c) in self.block {
            consider(o, State::block(self.nested.then_some(o), c));
        }
        for &(o, c) in self.nested_block {
            consider(o, State::block(Some(o), c));
        }
        for &(o, c) in self.quotes {
            consider(o, State::string(c, false));
        }
        for &(o, c) in self.verbatim {
            consider(o, State::string(c, true));
        }
        for &(o, c) in self.doc {
            let state = if statement_start {
                State::block(None, c)
            } else {
                State::string(c, false)
            };
            consider(o, state);
        }
        best
    }

    fn strip(&self, src: &[u8], keep: Keep) -> Vec<u8> {
        let mut out = Out::with_capacity(src.len());
        let mut state = State::Code;
        let mut i = 0;
        while i < src.len() {
            let rest = &src[i..];
            let (len, kept) = match state {
                State::Code => match self.opener_at(rest, out.statement_start) {
                    // A string's delimiter is code, so it's always kept;
                    // a comment's belongs to the comment.
                    Some((len, entered)) => {
                        state = entered;
                        (len, matches!(entered, State::Str { .. }) || keep.comments)
                    }
                    None => (1, true),
                },
                State::Line => {
                    if rest[0] == b'\n' {
                        state = State::Code;
                        (1, true)
                    } else {
                        (1, keep.comments)
                    }
                }
                State::Block { open, close, depth } => {
                    if rest.starts_with(close.as_bytes()) {
                        state = if depth == 1 {
                            State::Code
                        } else {
                            State::Block {
                                open,
                                close,
                                depth: depth - 1,
                            }
                        };
                        (close.len(), keep.comments)
                    } else if let Some(o) = open.filter(|o| rest.starts_with(o.as_bytes())) {
                        state = State::Block {
                            open,
                            close,
                            depth: depth + 1,
                        };
                        (o.len(), keep.comments)
                    } else {
                        (1, rest[0] == b'\n' || keep.comments)
                    }
                }
                State::Str { close, verbatim } => {
                    if !verbatim && rest[0] == b'\\' {
                        (rest.len().min(2), keep.strings)
                    } else if rest.starts_with(close.as_bytes()) {
                        state = State::Code;
                        (close.len(), true)
                    } else {
                        (1, keep.strings)
                    }
                }
            };
            if kept {
                out.push(&rest[..len]);
            }
            i += len;
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
        if let Some(end) = line.iter().rposition(|b| !b.is_ascii_whitespace()) {
            out.extend_from_slice(&line[..=end]);
            out.push(b'\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(path: &str, src: &str) -> String {
        strip_keeping(path, src, Keep::default())
    }

    fn strip_keeping(path: &str, src: &str, keep: Keep) -> String {
        String::from_utf8(code_only(path, src.as_bytes().to_vec(), keep)).unwrap()
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
            // A non-nesting block comment closes at the first terminator.
            ("a.c", "/* x /* y */ int z;\n", " int z;\n"),
        ] {
            assert_eq!(strip(path, src), want, "{path}");
        }
    }

    /// The default view of a string literal: the delimiters count, the
    /// contents do not.
    #[test]
    fn string_contents_are_stripped_delimiters_kept() {
        for (path, src, want) in [
            ("a.rs", "let s = \"secret contents\";\n", "let s = \"\";\n"),
            ("a.py", "s = 'it # counts not'  # gone\n", "s = ''\n"),
            // Escapes are contents too, and an escaped quote does not close.
            ("a.rs", "let s = \"a\\\"b\\n\";\n", "let s = \"\";\n"),
            // A multiline string collapses to its delimiters.
            (
                "a.py",
                "x = \"\"\"a\nb\"\"\" + y\n",
                "x = \"\"\"\"\"\" + y\n",
            ),
            // Comment markers inside a string open no comment.
            ("a.rs", "let u = \"http://x\"; // real\n", "let u = \"\";\n"),
            (
                "a.rs",
                "let s = r#\"/* ignored */ \\\"#; /* gone */\n",
                "let s = r#\"\"#;\n",
            ),
            ("a.py", "s = 'it\\'s # not'  # is\n", "s = ''\n"),
        ] {
            assert_eq!(strip(path, src), want, "{src}");
        }
    }

    #[test]
    fn doc_quote_off_statement_is_a_longer_string_quote() {
        assert_eq!(
            strip("a.py", "x = \"\"\"a\" # c\"\"\"\ny = 1\n"),
            "x = \"\"\"\"\"\"\ny = 1\n"
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
            "x = \"\"\"\"\"\"\n"
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
            "let s = \"\n"
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
        let src = "// looks like a comment\n\n";
        assert_eq!(strip("notes.unknownext", src), src);
        assert_eq!(strip("Makefile", "# c\nall:\n"), "all:\n");
    }

    #[test]
    fn shebang_decides_the_language_when_the_path_does_not() {
        assert_eq!(strip("bin/run", "#!/bin/sh\n# c\necho hi\n"), "echo hi\n");
    }

    #[test]
    fn line_comment_beats_string_quote_on_a_tie() {
        assert_eq!(
            strip("a.vim", "let s = \"x\" | echo s\n\" comment\nlet t = 1\n"),
            "let s =\nlet t = 1\n"
        );
    }

    /// Each stripped-by-default class comes back on request, restoring
    /// exactly its bytes.
    #[test]
    fn keeping_a_class_restores_its_bytes() {
        for (path, src, keep, want) in [
            // Keeping comments still empties strings.
            (
                "a.rs",
                "let s = \"x\"; // note\n\n/* block */\nfn g() {}\n",
                Keep {
                    comments: true,
                    strings: false,
                },
                "let s = \"\"; // note\n/* block */\nfn g() {}\n",
            ),
            // A docstring is kept as a comment, not emptied as a string.
            (
                "a.py",
                "def f():\n    \"\"\"Doc\"\"\"\n    return \"x\"\n",
                Keep {
                    comments: true,
                    strings: false,
                },
                "def f():\n    \"\"\"Doc\"\"\"\n    return \"\"\n",
            ),
            // Keeping strings restores their contents.
            (
                "a.rs",
                "let u = \"http://x\"; // real\n",
                Keep {
                    comments: false,
                    strings: true,
                },
                "let u = \"http://x\";\n",
            ),
            // Keeping both is the raw file: blank lines survive too.
            (
                "a.rs",
                "// c\n\nlet s = \"x\";\n",
                Keep {
                    comments: true,
                    strings: true,
                },
                "// c\n\nlet s = \"x\";\n",
            ),
        ] {
            assert_eq!(strip_keeping(path, src, keep), want, "{src}");
        }
    }

    #[test]
    fn a_lone_backslash_at_the_end_cannot_escape_past_it() {
        assert_eq!(strip("a.rs", "let s = \"abc\\"), "let s = \"\n");
        let keep = Keep {
            comments: false,
            strings: true,
        };
        assert_eq!(
            strip_keeping("a.rs", "let s = \"abc\\", keep),
            "let s = \"abc\\\n"
        );
    }
}
