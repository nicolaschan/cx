# Code-Only Scoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** cx scores code by default — comments stripped from every blob before compression, prose files skipped — with `--comments` / `--prose` to opt back in.

**Architecture:** `cx-cli` gains `strip.rs` (a byte-level scanner driven by tokei's per-language syntax table) and a prose layer in `filter.rs`. The three "what is scored" knobs plus the snapshot side move into one `pipeline::Scope` shared by diff and abs. Blobs go raw → filter → strip at the single point they enter scoring, so references, items, line counts and density all see code only. `cx-core` is untouched.

**Tech Stack:** Rust 2024, `tokei = 14` (default-features off) for the language table, existing `clap` boolish-flag pattern, `cargo test` end-to-end tests against real git repos.

Spec: `docs/superpowers/specs/2026-08-23-code-only-scoring-design.md`. Work in the worktree `.worktrees/code-only` on branch `feature/code-only-scoring`.

---

## File map

| File | Responsibility |
|---|---|
| `crates/cx-cli/Cargo.toml` | add the `tokei` dependency |
| `crates/cx-cli/src/language.rs` (new) | `of(path, bytes)`: which tokei language a blob is, from its name or shebang — never from disk |
| `crates/cx-cli/src/strip.rs` (new) | `code_only(path, bytes)`: comments out, blank lines dropped, via a `Syntax` built from tokei |
| `crates/cx-cli/src/lib.rs` | declare `strip` |
| `crates/cx-cli/src/pipeline.rs` | `Scope`; `prepare` (filter → strip) at the one blob entry point; diff/abs use it |
| `crates/cx-cli/src/filter.rs` | prose layer; `Filter::new` takes `&Scope` |
| `crates/cx-cli/src/main.rs` | `--comments`, `--prose`; `CommonArgs::scope()` |
| `crates/cx-cli/tests/end_to_end.rs` | `Scope` in fixtures; prose + comment behaviour through library and binary |
| `README.md` | document the default and the two flags |

Run all commands from `.worktrees/code-only`. Every commit message ends with:

```
Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0188gZzeXAahAyiMEmYqNYrq
```

---

### Task 1: The comment stripper

**Files:**
- Modify: `crates/cx-cli/Cargo.toml`
- Create: `crates/cx-cli/src/strip.rs`
- Modify: `crates/cx-cli/src/lib.rs`

- [ ] **Step 1: Add the dependency**

In `crates/cx-cli/Cargo.toml`, under `[dependencies]`, after `serde_json = "1"`:

```toml
tokei = { version = "14", default-features = false }
```

Run: `cargo build -p cx-cli`
Expected: builds (tokei compiles its table with tera at build time — ~5 s extra the first time).

- [ ] **Step 2: Declare the module and write the failing tests**

`crates/cx-cli/src/lib.rs` — add `pub mod strip;` after `pub mod report;` (keep the list alphabetical: breakdown, filter, git, pipeline, report, strip).

Create `crates/cx-cli/src/strip.rs` with only the tests for now:

```rust
//! The code-only view of a file: comments removed, blank lines dropped.
//! Which bytes are comments comes from tokei's per-language table (line
//! and block delimiters, nesting, string quotes), so a `//` inside a
//! string literal is code and `r#"/* … */"#` survives intact.

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(path: &str, src: &str) -> String {
        String::from_utf8(code_only(path, src.as_bytes().to_vec())).unwrap()
    }

    #[test]
    fn strips_line_and_block_comments_across_syntax_families() {
        for (path, src, want) in [
            ("a.rs", "// top\nfn f() {} // trailing\n/* block\n   more */\nfn g() {}\n", "fn f() {}\nfn g() {}\n"),
            ("a.py", "# top\nx = 1  # trailing\ny = 2\n", "x = 1\ny = 2\n"),
            ("a.lua", "-- line\nlocal a = 1 --[[ block\nstill ]] local b = 2\n", "local a = 1\n local b = 2\n"),
            ("a.hs", "{- outer {- inner -} still outer -}\nmain = x\n", "main = x\n"),
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
    fn docstrings_are_comments_only_in_statement_position() {
        assert_eq!(
            strip("a.py", "def f():\n    \"\"\"Doc\n    more\"\"\"\n    return 1\n"),
            "def f():\n    return 1\n"
        );
        assert_eq!(
            strip("a.py", "x = \"\"\"kept\nboth\"\"\"\n"),
            "x = \"\"\"kept\nboth\"\"\"\n"
        );
    }

    #[test]
    fn unterminated_comment_or_string_runs_to_the_end() {
        assert_eq!(strip("a.rs", "fn f() {}\n/* never closed\nfn g() {}\n"), "fn f() {}\n");
        assert_eq!(strip("a.rs", "let s = \"open // not\nfn g() {}\n"), "let s = \"open // not\nfn g() {}\n");
    }

    #[test]
    fn blank_and_whitespace_only_lines_are_dropped() {
        assert_eq!(strip("a.rs", "\n\nfn f() {}\n   \n\t\nfn g() {}   \n\n"), "fn f() {}\nfn g() {}\n");
        assert_eq!(strip("a.rs", "fn f() {}\r\nfn g() {}"), "fn f() {}\nfn g() {}\n");
    }

    #[test]
    fn unknown_language_is_returned_untouched() {
        let src = b"// looks like a comment\n\n".to_vec();
        assert_eq!(code_only("notes.unknownext", src.clone()), src);
        assert_eq!(code_only("Makefile", b"# c\nall:\n".to_vec()), b"all:\n");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p cx-cli strip::`
Expected: compile error — `code_only` not found.

- [ ] **Step 4: Implement the scanner**

Insert above the `#[cfg(test)]` block in `crates/cx-cli/src/strip.rs`:

```rust
use tokei::{Config, LanguageType};

/// `content` with its comments removed and whitespace-only lines dropped,
/// or `content` itself when `path` names no language tokei knows.
pub fn code_only(path: &str, content: Vec<u8>) -> Vec<u8> {
    match Syntax::of(path) {
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
    Block { open: Option<&'static str>, close: &'static str },
    Str { close: &'static str, verbatim: bool },
}

enum State {
    Code,
    Line,
    /// `open` is the token that deepens the nesting, when the comment nests.
    Block { open: Option<&'static str>, close: &'static str, depth: usize },
    Str { close: &'static str, verbatim: bool },
}

impl Syntax {
    fn of(path: &str) -> Option<Self> {
        let lang = LanguageType::from_path(path, &Config::default())?;
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
    /// `r#"` beats `"`. Docstrings only count at the start of a statement.
    fn opener_at(&self, rest: &[u8], statement_start: bool) -> Option<(usize, Opener)> {
        let mut best: Option<(usize, Opener)> = None;
        let mut consider = |token: &'static str, opener: Opener| {
            if rest.starts_with(token.as_bytes())
                && best.is_none_or(|(len, _)| token.len() > len)
            {
                best = Some((token.len(), opener));
            }
        };
        for &t in self.line {
            consider(t, Opener::Line);
        }
        for &(o, c) in self.block {
            consider(o, Opener::Block { open: self.nested.then_some(o), close: c });
        }
        for &(o, c) in self.nested_block {
            consider(o, Opener::Block { open: Some(o), close: c });
        }
        for &(o, c) in self.quotes {
            consider(o, Opener::Str { close: c, verbatim: false });
        }
        for &(o, c) in self.verbatim {
            consider(o, Opener::Str { close: c, verbatim: true });
        }
        if statement_start {
            for &(o, c) in self.doc {
                consider(o, Opener::Block { open: None, close: c });
            }
        }
        best
    }

    fn strip(&self, src: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(src.len());
        let mut state = State::Code;
        // Nothing but whitespace since the last newline.
        let mut statement_start = true;
        let mut emit = |out: &mut Vec<u8>, bytes: &[u8]| {
            for &b in bytes {
                statement_start = b == b'\n' || (statement_start && b.is_ascii_whitespace());
            }
            out.extend_from_slice(bytes);
        };
        let mut i = 0;
        while i < src.len() {
            let rest = &src[i..];
            match state {
                State::Code => match self.opener_at(rest, statement_start) {
                    Some((len, Opener::Line)) => {
                        state = State::Line;
                        i += len;
                    }
                    Some((len, Opener::Block { open, close })) => {
                        state = State::Block { open, close, depth: 1 };
                        i += len;
                    }
                    Some((len, Opener::Str { close, verbatim })) => {
                        emit(&mut out, &rest[..len]);
                        state = State::Str { close, verbatim };
                        i += len;
                    }
                    None => {
                        emit(&mut out, &rest[..1]);
                        i += 1;
                    }
                },
                State::Line => {
                    if rest[0] == b'\n' {
                        state = State::Code;
                    } else {
                        i += 1;
                    }
                }
                State::Block { open, close, depth } => {
                    if rest.starts_with(close.as_bytes()) {
                        i += close.len();
                        state = if depth == 1 {
                            State::Code
                        } else {
                            State::Block { open, close, depth: depth - 1 }
                        };
                    } else if let Some(open) = open.filter(|o| rest.starts_with(o.as_bytes())) {
                        i += open.len();
                        state = State::Block { open: Some(open), close, depth: depth + 1 };
                    } else {
                        if rest[0] == b'\n' {
                            emit(&mut out, b"\n");
                        }
                        i += 1;
                    }
                }
                State::Str { close, verbatim } => {
                    if !verbatim && rest[0] == b'\\' {
                        let n = rest.len().min(2);
                        emit(&mut out, &rest[..n]);
                        i += n;
                    } else if rest.starts_with(close.as_bytes()) {
                        emit(&mut out, close.as_bytes());
                        state = State::Code;
                        i += close.len();
                    } else {
                        emit(&mut out, &rest[..1]);
                        i += 1;
                    }
                }
            }
        }
        without_blank_lines(&out)
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
```

Notes for the implementer:
- `Option::is_none_or` is stable since Rust 1.82; the pinned toolchain (`rust-toolchain.toml`, stable) has it.
- The Lua expectation `" local b = 2\n"` keeps the leading space: only *whitespace-only* lines are dropped and only trailing whitespace is trimmed. That is deliberate — indentation is information.
- `Makefile` in the last test is a language tokei knows (`#` comments), which is why it *is* stripped; the point of the test is the pair.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p cx-cli strip::`
Expected: 6 passed.

If `docstrings_are_comments_only_in_statement_position` fails on the second case, check `opener_at`: the docstring opener must be offered only when `statement_start` is true, and after `x = ` it is false because `x` was emitted.

- [ ] **Step 6: Commit**

```bash
git add crates/cx-cli/Cargo.toml Cargo.lock crates/cx-cli/src/lib.rs crates/cx-cli/src/strip.rs
git commit -m "Add strip::code_only: comments out via tokei's syntax table"
```

---

### Task 2: One `Scope` for what is scored (pure refactor)

**Files:**
- Modify: `crates/cx-cli/src/pipeline.rs` (`DiffOptions`, `AbsOptions`, both uses of `opts.side` / `opts.ignore_tests`)
- Modify: `crates/cx-cli/src/main.rs:66-93`
- Modify: `crates/cx-cli/tests/end_to_end.rs` (every `DiffOptions { … }` / `AbsOptions { … }` literal)

No behaviour changes; the existing tests are the receipt.

- [ ] **Step 1: Define `Scope` and move the knobs into it**

In `crates/cx-cli/src/pipeline.rs`, replace the `DiffOptions` definition and the `AbsOptions` definition with:

```rust
/// What is scored: which snapshot, and which kinds of content count.
#[derive(Clone, Copy, Default)]
pub struct Scope {
    pub side: Side,
    /// Exclude test files from the universe entirely — they are then in
    /// no reference and no scoring pass, and appear as skipped.
    pub ignore_tests: bool,
    /// Score comments too. Otherwise every blob is reduced to code before
    /// it enters any reference or scoring pass.
    pub comments: bool,
    /// Score prose files (Markdown, reStructuredText, …) too. Otherwise
    /// they appear as skipped.
    pub prose: bool,
}

#[derive(Default)]
pub struct DiffOptions {
    pub base: Option<String>,
    pub scope: Scope,
}
```

and

```rust
#[derive(Default)]
pub struct AbsOptions {
    /// Skip per-file contributions, leaving one joint compression —
    /// much faster on big trees.
    pub no_files: bool,
    pub scope: Scope,
}
```

Then in `diff()`: `opts.side` → `opts.scope.side` (two places: `git.changes`, `git.contents`), `opts.ignore_tests` → `opts.scope.ignore_tests`, and `churn = git.line_counts(&merge_base, opts.side)` → `opts.scope.side`. In `abs()`: `opts.side` → `opts.scope.side` (three places: `git.list`, `git.contents`, `snapshot: opts.side.label()`), `opts.ignore_tests` → `opts.scope.ignore_tests`.

`comments` and `prose` are declared now and wired in Tasks 3 and 4; until then they are read by nothing, which is fine for a field.

- [ ] **Step 2: Build `Scope` once in `main.rs`**

Replace `DiffArgs::options` and `CommonArgs::side` / `abs_options` with:

```rust
impl DiffArgs {
    fn options(self, common: &CommonArgs) -> DiffOptions {
        DiffOptions {
            base: self.base,
            scope: common.scope(),
        }
    }
}

impl CommonArgs {
    fn scope(&self) -> Scope {
        let side = if self.committed {
            Side::Head
        } else if self.staged {
            Side::Index
        } else {
            Side::Worktree
        };
        Scope {
            side,
            ignore_tests: self.ignore_tests,
            comments: false,
            prose: false,
        }
    }

    fn abs_options(&self) -> AbsOptions {
        AbsOptions {
            no_files: self.no_files,
            scope: self.scope(),
        }
    }
```

(keep `report_options` as is). Update the import: `use cx_cli::pipeline::{self, AbsOptions, DiffOptions, Scope};`.

- [ ] **Step 3: Update the end-to-end fixtures**

In `crates/cx-cli/tests/end_to_end.rs`, change the import to
`use cx_cli::pipeline::{self, AbsOptions, AbsReport, DiffOptions, DiffReport, Scope};`
and add two helpers after `setup()`:

```rust
fn diff_at(git: &Git, side: Side) -> DiffReport {
    pipeline::diff(
        git,
        &DiffOptions {
            scope: Scope { side, ..Default::default() },
            ..Default::default()
        },
    )
    .unwrap()
}

fn abs_at(git: &Git, side: Side) -> AbsReport {
    pipeline::abs(
        git,
        &AbsOptions {
            scope: Scope { side, ..Default::default() },
            ..Default::default()
        },
    )
    .unwrap()
}
```

Then replace every literal of the shape

```rust
pipeline::diff(&X, &DiffOptions { side: Side::S, ..Default::default() }).unwrap()
```

with `diff_at(&X, Side::S)` (in `scores_a_realistic_branch`, `staged_mode_scores_the_index`, `worktree_side_scores_the_whole_working_tree` ×2, `untracked_lines_reach_the_churn_totals` ×2), and every

```rust
pipeline::abs(&X, &AbsOptions { side: S, ..Default::default() }).unwrap()
```

with `abs_at(&X, S)` (in `tree_reports_absolute_complexity_with_contributions`, `abs_measures_the_snapshot_it_is_asked_for`'s `measure` closure, `an_unmerged_path_is_scored_once`). The `ignore_tests: true` literals in `line_churn_counts_what_git_counts` and `ignoring_tests_drops_their_cost_and_leaves_the_rest` become

```rust
&DiffOptions {
    scope: Scope { ignore_tests, ..Default::default() },
    ..Default::default()
}
```

(with `ignore_tests: true` spelled out in the first). `tree_contributions_are_suppressable` keeps its literal, as

```rust
&AbsOptions {
    no_files: true,
    scope: Scope { side: Side::Head, ..Default::default() },
}
```

`DiffReport` and `AbsReport` are already `pub` in `pipeline.rs`; the helpers' return types are why they join the import.

- [ ] **Step 4: Verify nothing changed**

Run: `cargo test -p cx-cli`
Expected: all tests pass, same count as before the task (17 end-to-end + unit tests). Also `cargo build` must emit no dead-code warning for `comments`/`prose` (pub fields of a pub struct never warn).

- [ ] **Step 5: Commit**

```bash
git add crates/cx-cli/src/pipeline.rs crates/cx-cli/src/main.rs crates/cx-cli/tests/end_to_end.rs
git commit -m "Gather what is scored into one Scope shared by diff and abs"
```

---

### Task 3: Prose files are skipped; `--prose` keeps them

**Files:**
- Modify: `crates/cx-cli/src/filter.rs`
- Modify: `crates/cx-cli/src/pipeline.rs` (two `Filter::new` calls)
- Modify: `crates/cx-cli/src/main.rs` (`--prose`)
- Modify: `crates/cx-cli/tests/end_to_end.rs` (`setup()`, env test)

- [ ] **Step 1: Write the failing filter tests**

In `crates/cx-cli/src/filter.rs` tests module, change the `filter()` helper and `respects_linguist_attributes` / `detects_tests_by_naming_convention` to the new constructor (it takes a `&Scope` now):

```rust
    use crate::pipeline::Scope;

    fn filter() -> Filter {
        Filter::new(Path::new("/nonexistent"), HashMap::new(), &Scope::default()).unwrap()
    }
```

In `respects_linguist_attributes`: `Filter::new(Path::new("/nonexistent"), attrs, &Scope::default())`.
In `detects_tests_by_naming_convention`:

```rust
        let f = Filter::new(
            Path::new("/nonexistent"),
            HashMap::new(),
            &Scope { ignore_tests: true, ..Default::default() },
        )
        .unwrap();
```

Then add:

```rust
    /// Prose is what tokei types as such plus the conventional
    /// extensionless documents; data and markup are code.
    #[test]
    fn skips_prose_unless_asked_to_keep_it() {
        let f = filter();
        for path in [
            "README.md", "docs/guide.markdown", "docs/page.mdx", "CHANGES.rst",
            "notes.txt", "book/ch1.adoc", "todo.org",
            "LICENSE", "COPYING", "COPYING.LESSER", "LICENSE-MIT", "README",
            "sub/NOTICE", "AUTHORS", "CONTRIBUTORS", "CHANGELOG", "ChangeLog",
        ] {
            assert_eq!(f.exclusion(path, b"words"), Some("prose"), "{path}");
        }
        for path in [
            "ci.yaml", "package.json", "index.html", "Cargo.toml", "style.css",
            "LICENSE.py", "README.rs", "src/main.rs", "Makefile",
        ] {
            assert_eq!(f.exclusion(path, b"code"), None, "{path}");
        }

        let keep = Filter::new(
            Path::new("/nonexistent"),
            HashMap::new(),
            &Scope { prose: true, ..Default::default() },
        )
        .unwrap();
        assert_eq!(keep.exclusion("README.md", b"words"), None);
        assert_eq!(keep.exclusion("LICENSE", b"words"), None);
    }

    /// A prose document about tests is prose first.
    #[test]
    fn prose_is_recognised_before_tests() {
        let f = Filter::new(
            Path::new("/nonexistent"),
            HashMap::new(),
            &Scope { ignore_tests: true, ..Default::default() },
        )
        .unwrap();
        assert_eq!(f.exclusion("docs/tests/plan.md", b"words"), Some("prose"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p cx-cli filter::`
Expected: compile error — `Filter::new` takes a `bool`, not `&Scope`.

- [ ] **Step 3: Implement the prose layer**

In `crates/cx-cli/src/filter.rs`:

Module doc — replace the numbered list with:

```rust
//! The file-filtering stack (plan §3, "order matters"):
//! 1. `.gitattributes` linguist-generated / -vendored / -documentation
//! 2. binary detection on content (UTF-16/32 aware)
//! 3. ported linguist generated/vendored patterns
//! 4. prose, unless `--prose` asks to keep it
//! 5. test files, when `--ignore-tests` asks for it
//! 6. `.cxignore`
//!
//! The density backstop (layer 7) lives in the report, not here — it
//! flags, it doesn't drop.
```

Imports — add:

```rust
use tokei::LanguageType;

use crate::git::LinguistAttrs;
use crate::language;
use crate::pipeline::Scope;
```

After `SEPARATORS`, add:

```rust
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

/// Whether a blob is a prose document: a prose language by tokei's
/// table, or — only when no language is found at all, so `LICENSE.py`
/// is Python — a conventional extensionless document.
fn is_prose(path: &str, content: &[u8]) -> bool {
    match language::of(path, content) {
        Some(lang) => PROSE_LANGUAGES.contains(&lang),
        None => {
            let file = path.rsplit('/').next().unwrap_or(path);
            let stem = file.split(['.', '-', '_']).next().unwrap_or(file);
            PROSE_FILENAMES.iter().any(|n| stem.eq_ignore_ascii_case(n))
        }
    }
}
```

`Filter` gains a field and `new` takes the scope:

```rust
pub struct Filter {
    attrs: HashMap<String, LinguistAttrs>,
    patterns: GlobSet,
    ignore_tests: bool,
    prose: bool,
    cxignore: Option<Gitignore>,
}
```

```rust
    pub fn new(root: &Path, attrs: HashMap<String, LinguistAttrs>, scope: &Scope) -> Result<Self> {
        …
        Ok(Filter {
            attrs,
            patterns: glob_set(&LINGUIST_PATTERNS)?,
            ignore_tests: scope.ignore_tests,
            prose: scope.prose,
            cxignore: cxignore.transpose()?,
        })
    }
```

In `exclusion`, between the pattern check and the test check:

```rust
        if !self.prose && is_prose(path, content) {
            return Some("prose");
        }
```

In `crates/cx-cli/src/pipeline.rs`, both `Filter::new(git.root(), git.linguist_attrs(&attr_paths)?, opts.scope.ignore_tests)` calls become `Filter::new(git.root(), git.linguist_attrs(&attr_paths)?, &opts.scope)`.

- [ ] **Step 4: Run the filter tests**

Run: `cargo test -p cx-cli filter::`
Expected: all pass, including the two new ones. If `ChangeLog` fails, `eq_ignore_ascii_case` is missing on the stem comparison.

- [ ] **Step 5: Write the failing end-to-end tests**

In `crates/cx-cli/tests/end_to_end.rs` `setup()`, on the feature branch right after the `tests/novel_test.rs` write, add:

```rust
    // A prose document: skipped by default, scored with --prose.
    fs::write(root.join("README.md"), gen_code(31, 40)).unwrap();
```

(Its content only has to be novel; the prose layer keys on the path.) Existing tests keep passing because a skipped file is in no count they assert on.

Replace `ignore_tests_can_be_pinned_through_the_environment` whole with a table over both skip-list knobs:

```rust
/// Environment defaults through the real binary: a pinned value is only
/// useful if it reaches scoring and a single run can still veto it. Each
/// knob is observed through the skip list — `CX_IGNORE_TESTS` puts a test
/// there, `CX_PROSE` takes a document out.
#[test]
fn defaults_can_be_pinned_through_the_environment() {
    let (dir, _git) = setup();
    for (var, flag, path, reason, skipped_when_on) in [
        ("CX_IGNORE_TESTS", "--ignore-tests", "tests/novel_test.rs", "test", true),
        ("CX_PROSE", "--prose", "README.md", "prose", false),
    ] {
        for (pinned, arg, on) in [
            (None, None, false),
            (Some("1"), None, true),
            (Some("true"), None, true),
            // A set variable must not mean "true" whatever its value.
            (Some("0"), None, false),
            (Some("1"), Some(format!("{flag}=false")), false),
            (None, Some(flag.to_owned()), true),
        ] {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_cx"));
            cmd.current_dir(dir.path())
                .args(["diff", "--json"])
                .args(&arg)
                .env_remove("CX_IGNORE_TESTS")
                .env_remove("CX_PROSE");
            if let Some(value) = pinned {
                cmd.env(var, value);
            }
            let out = cmd.output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            let skipped = report["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["path"] == path && s["reason"] == reason);
            assert_eq!(skipped, on == skipped_when_on, "{var}={pinned:?}, {arg:?}");
        }
    }
}
```

And a library-level test of what `prose` does to the numbers, after `ignoring_tests_drops_their_cost_and_leaves_the_rest`:

```rust
/// Prose is out of the universe by default and fully scored on request.
#[test]
fn prose_is_skipped_by_default_and_scored_on_request() {
    let (_dir, git) = setup();
    let scored = |prose| {
        pipeline::diff(
            &git,
            &DiffOptions {
                scope: Scope { prose, ..Default::default() },
                ..Default::default()
            },
        )
        .unwrap()
    };
    let (without, with) = (scored(false), scored(true));

    assert!(
        without.skipped.iter().any(|s| s.path == "README.md" && s.reason == "prose"),
        "README.md must be skipped as prose: {:?}",
        without.skipped.iter().map(|s| &s.path).collect::<Vec<_>>()
    );
    assert!(without.files.iter().all(|f| f.path != "README.md"));
    assert!(
        with.files.iter().any(|f| f.path == "README.md" && f.review_bytes > 300.0),
        "with --prose the document is scored like anything else"
    );
    assert!(with.skipped.iter().all(|s| s.path != "README.md"));
}
```

- [ ] **Step 6: Run to verify the new tests fail**

Run: `cargo test -p cx-cli --test end_to_end prose`
Expected: `defaults_can_be_pinned_through_the_environment` fails on `--prose` (unknown flag) — the library test passes already since Task 3 Step 3 wired `scope.prose` into the filter. That is fine: the binary path is what is still missing.

- [ ] **Step 7: Add the `--prose` flag**

In `crates/cx-cli/src/main.rs` `CommonArgs`, after `ignore_tests`:

```rust
    /// Score prose files too — Markdown, reStructuredText, plain text,
    /// AsciiDoc, Org, and extensionless documents such as LICENSE. By
    /// default they are skipped. Takes an optional value so a pinned
    /// default can be vetoed for one run: `--prose=false`.
    #[arg(
        long,
        env = "CX_PROSE",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    prose: bool,
```

and in `scope()`: `prose: self.prose,`.

- [ ] **Step 8: Run the whole suite**

Run: `cargo test -p cx-cli`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add crates/cx-cli/src/filter.rs crates/cx-cli/src/pipeline.rs crates/cx-cli/src/main.rs crates/cx-cli/tests/end_to_end.rs
git commit -m "Skip prose files by default; --prose scores them"
```

---

### Task 4: Comments are stripped before scoring; `--comments` keeps them

**Files:**
- Modify: `crates/cx-cli/src/pipeline.rs` (`diff`, `abs`)
- Modify: `crates/cx-cli/src/main.rs` (`--comments`)
- Modify: `crates/cx-cli/tests/end_to_end.rs`

- [ ] **Step 1: Write the failing end-to-end test**

Append to `crates/cx-cli/tests/end_to_end.rs`. It builds its own repo so the shared fixture's churn numbers stay put, and goes through the binary so the flag, the environment default, and the scoring are one receipt:

```rust
/// A comment-only change is free by default: both sides reduce to the
/// same code, and the line count is of that code. With comments scored,
/// sixty lines of novel prose cost what novel content costs.
#[test]
fn comments_are_stripped_unless_asked_to_score_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    fs::create_dir(root.join("src")).unwrap();
    let code = gen_code(1, 120);
    fs::write(root.join("src/lib.rs"), &code).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    let comments: String = String::from_utf8(gen_code(9, 60))
        .unwrap()
        .lines()
        .map(|l| format!("// {l}\n"))
        .collect();
    fs::write(root.join("src/lib.rs"), [comments.as_bytes(), &code].concat()).unwrap();
    git(root, &["commit", "-q", "-am", "comments"]);

    let run = |pinned: Option<&str>, flag: Option<&str>| -> serde_json::Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cx"));
        cmd.current_dir(root)
            .args(["diff", "--json", "--committed"])
            .args(flag)
            .env_remove("CX_COMMENTS");
        if let Some(value) = pinned {
            cmd.env("CX_COMMENTS", value);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        report["files"][0].clone()
    };

    let stripped = run(None, None);
    assert_eq!(stripped["path"], "src/lib.rs");
    assert_eq!(stripped["new_lines"], 120, "lines are counted after stripping");
    assert!(
        stripped["review_bytes"].as_f64().unwrap() < 64.0,
        "a comment-only change is ≈ free to review: {stripped}"
    );
    assert!(stripped["delta_bytes"].as_f64().unwrap().abs() < 64.0);

    for kept in [run(None, Some("--comments")), run(Some("1"), None)] {
        assert_eq!(kept["new_lines"], 180);
        assert!(
            kept["review_bytes"].as_f64().unwrap() > 300.0,
            "novel comments cost review attention when scored: {kept}"
        );
        assert!(kept["delta_bytes"].as_f64().unwrap() > 300.0);
    }
    assert_eq!(run(Some("1"), Some("--comments=false"))["new_lines"], 120);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cx-cli --test end_to_end comments_are_stripped`
Expected: FAIL — `--comments` is an unknown flag (and, once added, `new_lines` would be 180 by default).

- [ ] **Step 3: Strip at the one point blobs enter scoring**

In `crates/cx-cli/src/pipeline.rs`:

Imports — add `use crate::strip;`.

Add after `fn lines`:

```rust
/// What a blob becomes on its way into scoring: its code, or the reason
/// the filter dropped it. The filter sees raw bytes (binary detection
/// needs them); everything after sees code only.
type Prepared = Result<Vec<u8>, &'static str>;

fn prepare(filter: &Filter, scope: &Scope, path: &str, raw: Vec<u8>) -> Prepared {
    match filter.exclusion(path, &raw) {
        Some(reason) => Err(reason),
        None if scope.comments => Ok(raw),
        None => Ok(strip::code_only(path, raw)),
    }
}

/// Every path with a blob, prepared. Paths whose blob is missing (a
/// submodule, a file gone from disk) are simply absent.
fn load<'a>(
    filter: &Filter,
    scope: &Scope,
    paths: &[&'a str],
    blobs: Vec<Option<Vec<u8>>>,
) -> HashMap<&'a str, Prepared> {
    paths
        .iter()
        .copied()
        .zip(blobs)
        .filter_map(|(path, blob)| Some((path, prepare(filter, scope, path, blob?))))
        .collect()
}
```

`Item` borrows instead of cloning:

```rust
/// One changed file with whichever sides exist and passed the filter.
struct Item<'a> {
    path: String,
    status: Status,
    old: Option<&'a [u8]>,
    new: Option<&'a [u8]>,
}
```

Rewrite the body of `diff()` from `let tree_paths = …` down to (not including) `let scorer = Scorer::default();`:

```rust
    let scope = &opts.scope;
    let tree_paths = git.ls_tree(&merge_base)?;
    let tree_refs: Vec<&str> = tree_paths.iter().map(String::as_str).collect();
    let new_side_paths: Vec<&str> = changes
        .iter()
        .filter(|c| c.status != Status::Deleted)
        .map(|c| c.path.as_str())
        .collect();
    let attr_paths: Vec<String> = tree_paths
        .iter()
        .cloned()
        .chain(new_side_paths.iter().map(|p| p.to_string()))
        .collect();
    let filter = Filter::new(git.root(), git.linguist_attrs(&attr_paths)?, scope)?;

    // The whole old tree plus the new side of every change, each blob
    // filtered and reduced to code once.
    let old_tree = load(&filter, scope, &tree_refs, git.tree_contents(&merge_base, &tree_refs)?);
    let new_contents = load(&filter, scope, &new_side_paths, git.contents(scope.side, &new_side_paths)?);

    // The universe is kept files only: a file the filter excludes exists
    // in no reference and no scoring pass.
    let kept_tree: Vec<(&str, &[u8])> = tree_refs
        .iter()
        .filter_map(|p| Some((*p, old_tree.get(p)?.as_deref().ok()?)))
        .collect();

    // Partition changes into scorable items and skipped files. A change
    // is skipped when any side it has fails the filter (e.g. a file that
    // flipped binary→text is skipped whole rather than half-scored).
    let mut items: Vec<Item> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut touched: HashSet<&str> = HashSet::new();
    for change in &changes {
        touched.insert(change.path.as_str());
        let old_path = match &change.status {
            Status::Added => None,
            Status::Modified | Status::Deleted => Some(change.path.as_str()),
            Status::Renamed { from } => {
                touched.insert(from.as_str());
                Some(from.as_str())
            }
        };
        let old = old_path.and_then(|p| old_tree.get(p));
        let new = (change.status != Status::Deleted)
            .then(|| new_contents.get(change.path.as_str()))
            .flatten();
        if let Some(reason) = [new, old].into_iter().flatten().find_map(|b| b.as_ref().err()) {
            skipped.push(Skipped {
                path: change.path.clone(),
                reason: (*reason).to_owned(),
            });
            continue;
        }
        let code = |blob: Option<&'_ Prepared>| blob.and_then(|b| b.as_deref().ok());
        let (old, new) = (code(old), code(new));
        if old.is_none() && new.is_none() {
            continue;
        }
        items.push(Item {
            path: change.path.clone(),
            status: change.status.clone(),
            old,
            new,
        });
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
```

Then replace the `assemble` closure and the two references with:

```rust
    let scorer = Scorer::default();
    let old_tree_ref = scorer.assemble(&kept_tree.iter().map(|(_, c)| *c).collect::<Vec<_>>());
    let remainder: Vec<&[u8]> = kept_tree
        .iter()
        .filter(|(p, _)| !touched.contains(p))
        .map(|(_, c)| *c)
        .collect();
    let remainder_ref = scorer.assemble(&remainder);
```

and the item slices become `items.iter().filter_map(|i| i.new).collect()` / `items.iter().filter_map(|i| i.old).collect()`. In the per-file loop, `item.new.as_deref().map_or(0, lines)` → `item.new.map_or(0, lines)`.

Note on the `code` closure: `find_map(|b| b.as_ref().err())` yields `&&'static str`, hence the `(*reason).to_owned()`. If the borrow checker objects to `code` capturing nothing, write it as a nested `fn code(blob: Option<&Prepared>) -> Option<&[u8]>`.

Rewrite `abs()` from `let contents` down to `let kept_contents` as:

```rust
    let scope = &opts.scope;
    let paths = git.list(scope.side)?;
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let blobs = git.contents(scope.side, &path_refs)?;
    let attr_paths: Vec<String> = path_refs
        .iter()
        .zip(&blobs)
        .filter_map(|(p, b)| b.as_ref().map(|_| p.to_string()))
        .collect();
    let filter = Filter::new(git.root(), git.linguist_attrs(&attr_paths)?, scope)?;
    // `load` hands back a map; the chain rule wants sorted-path order,
    // which is the order `git.list` produced.
    let mut kept: Vec<(&str, Vec<u8>)> = load(&filter, scope, &path_refs, blobs)
        .into_iter()
        .filter_map(|(p, prepared)| Some((p, prepared.ok()?)))
        .collect();
    kept.sort_by(|a, b| a.0.cmp(b.0));
    let kept_contents: Vec<&[u8]> = kept.iter().map(|(_, c)| c.as_slice()).collect();
```

The `AbsFile { path: (*path).clone(), … }` becomes `path: (*path).to_owned()` since `path` is now `&str`.

`git.contents` and `git.tree_contents` both return `Result<Vec<Option<Vec<u8>>>>` (`git.rs:234-238`), which is what `load` takes after the `?`.

- [ ] **Step 4: Add the `--comments` flag**

In `crates/cx-cli/src/main.rs` `CommonArgs`, after `ignore_tests` (before `prose`):

```rust
    /// Score comments too. By default every file is reduced to code —
    /// comments stripped, blank lines dropped — before scoring. Takes an
    /// optional value so a pinned default can be vetoed for one run:
    /// `--comments=false`.
    #[arg(
        long,
        env = "CX_COMMENTS",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    comments: bool,
```

and in `scope()`: `comments: self.comments,`.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test`
Expected: all pass, including `comments_are_stripped_unless_asked_to_score_them`. The generated fixture code (`fn f_… { … }`) has no comments, so every existing number is unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/cx-cli/src/pipeline.rs crates/cx-cli/src/main.rs crates/cx-cli/tests/end_to_end.rs
git commit -m "Reduce every blob to code before scoring; --comments scores them too"
```

---

### Task 5: Documentation and final verification

**Files:**
- Modify: `README.md:54-64` (flag block, env sentence) and `README.md:83-85` (filter paragraph)

- [ ] **Step 1: Update the README**

Flag block (line 54) — replace

```
# any of the above: [--staged|--committed] [-v|--verbose] [--no-files] [--ignore-tests] [--json]
```

with

```
# any of the above: [--staged|--committed] [-v|--verbose] [--no-files]
#                   [--comments] [--prose] [--ignore-tests] [--json]
```

Env sentence (lines 62–65) — replace with:

```
Defaults can be pinned through the environment — `CX_COMMENTS=1`,
`CX_PROSE=1`, `CX_IGNORE_TESTS=1`, `CX_TOP=15`, `CX_BASE=develop` — and
any single run can still override them on the command line
(`--comments=false`, `-n 50`). `cx --help` lists which variable backs
each flag.
```

Filter paragraph (lines 83–85) — replace with:

```
cx scores **code**. Before anything is compressed, every file is reduced
to its code: comments are stripped and blank lines dropped, using
[tokei](https://github.com/XAMPPRocky/tokei)'s per-language syntax table
(line and block comment delimiters, nesting, string quotes — so a `//`
inside a string literal stays), for the 300-odd languages it knows;
anything else passes through untouched. A comment-only change scores ≈0
on both axes. `--comments` scores comments too.

Prose files — Markdown, reStructuredText, plain text, AsciiDoc, Org, and
extensionless documents such as `LICENSE`, `README`, `CHANGELOG` — are
skipped. `--prose` scores them. Data and markup (JSON, YAML, TOML, HTML,
CSS) are code.

Files are filtered before scoring: `.gitattributes` linguist annotations,
binary detection, common generated/vendored patterns (lockfiles, `dist/`,
`vendor/`, minified assets…), prose, and a `.cxignore` (gitignore syntax).
```

Also in the `--verbose` example near line 40, the `skipped:` line is illustrative; leave it.

- [ ] **Step 2: Verify from clean**

Run, in order:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: fmt clean, no clippy warnings (watch for `needless_borrow` around `&arg` in the env test — `.args(&arg)` on an `Option<String>` is correct, but if clippy wants `.args(arg.as_deref())` do that), all tests pass.

Then run the tool on itself and eyeball it:

```bash
cargo run -q -- abs -v
cargo run -q -- abs -v --comments --prose
```

Expected: the default run lists `README.md` and both `docs/superpowers/**/*.md` files under `skipped: … (prose)`, and `C(tree)` is noticeably smaller than with `--comments --prose`.

Finally:

```bash
nix build
```

Expected: builds (the new `Cargo.lock` entries vendor through `cargoLock.lockFile`). This takes a few minutes.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Document code-only scoring and the --comments/--prose knobs"
```

---

## Self-review against the spec

- Prose layer, order, tokei set, extensionless names: Task 3.
- `strip.rs` + `Syntax` + scanner rules (longest match, escapes, nesting, statement-position docstrings, EOF, blank-line pass): Task 1.
- `Scope` replacing duplicated knobs: Task 2.
- raw → filter → strip once, at the entry point; line counts post-strip; numstat untouched: Task 4.
- `--comments` / `--prose` with env + veto: Tasks 3 and 4; documented in Task 5.
- Every test in the spec's Testing section has a home: unit tables in Tasks 1 and 3, e2e in Tasks 3 and 4.
