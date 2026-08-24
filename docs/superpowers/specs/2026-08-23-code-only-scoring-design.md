# cx: code-only scoring — strip comments, skip prose (2026-08-23)

Approved design. cx currently scores every kept file byte-for-byte, so a
large share of what it highlights is comments and prose documents. Most
of the time spent optimizing against cx goes into debating those, when the
target is structural complexity. This change makes cx score *code* by
default: comments are stripped from every blob before compression, and
prose files (Markdown, reStructuredText, plain text, …) are skipped.
Both behaviours are opt-out per run.

Language coverage comes from the `tokei` crate's syntax table (329
languages: line/block comment delimiters, nesting, string quotes,
verbatim quotes, doc quotes) so cx stays language-agnostic without a
grammar per language. Chosen over tree-sitter (one compiled C grammar per
language, not agnostic, heavy multiarch builds) and over vendoring
tokei's `languages.json` (zero extra deps but a table to refresh by
hand). If tokei's dependency weight ever hurts the nix builds, the
scanner consumes a small `Syntax` struct, so swapping the table's source
is a local change.

## Architecture

Two additions to `cx-cli`. `cx-core` is unchanged: bytes in, scores out,
no new dependencies.

**`filter.rs` gains a prose layer.** A file whose tokei language is one
of linguist's `type: prose` languages is excluded with reason `prose`,
unless `--prose`. The set is a list of `tokei::LanguageType`s — tokei
knows six of linguist's eighteen: Markdown, Mdx, reStructuredText, Text,
AsciiDoc, Org — so every extension tokei maps to those languages comes
along (`.md`, `.markdown`, `.mdx`, `.rst`, `.txt`, `.text`, `.adoc`,
`.asciidoc`, `.org`). The twelve tokei lacks (Textile, Pod, RDoc, Creole,
Wikitext, RMarkdown, …) are rare enough to leave undetected rather than
keep a second table. Conventional documents with no extension —
`LICENSE`, `COPYING`, `README`, `CHANGELOG`, `NOTICE`, `AUTHORS`,
`CONTRIBUTORS`, plus `LICENSE-MIT`/`COPYING.LESSER`-style variants —
have no language for tokei to find, so they are matched by basename
instead, only when tokei found nothing (`LICENSE.py` is Python).
Order in the stack: after the generated/vendored patterns,
before the test layer, so `docs/2026-04-27-web-e2e-design.md` reads
`prose` rather than being kept because plural `specs/` is documentation.
Data and markup files (JSON, YAML, TOML, HTML, CSS) stay in: a `Cargo.toml`
or a CI workflow is real complexity.

**New `strip.rs`.** `strip_comments(path: &str, content: &[u8]) ->
Cow<[u8]>`. Looks the language up with `tokei::LanguageType::from_path`,
builds `Syntax { line, block, nested, quotes, verbatim, doc }` from its
table, and runs the scanner below. Unknown language → borrowed input,
untouched.

**One `Scope` for what is scored.** `ignore_tests` is today copied into
both `DiffOptions` and `AbsOptions`; the two new knobs would triple that.
Instead:

```rust
pub struct Scope {
    pub side: Side,
    pub ignore_tests: bool,
    pub comments: bool,
    pub prose: bool,
}
```

`DiffOptions { base, scope }`, `AbsOptions { no_files, scope }`, and
`CommonArgs::scope()` builds it once in `main.rs`.

## Data flow

Load raw → filter → strip → everything else.

The filter sees raw bytes because binary detection needs them. Stripping
happens once per kept blob, at the single point blobs enter scoring, so
the old-tree reference, the remainder, the diff items, per-file `lines`,
and density all see the same code-only view. Applying it anywhere later
would let the reference "know" comments the new side has lost.

A comment-only change strips to identical old and new content and lands
at ≈0 on both axes. It is still listed, like a whitespace-only change
today. The footer's `lines +X −Y` stays git's `--numstat`: the README
already frames it as the familiar size the others are read against, not a
verdict.

## The scanner

A byte-level state machine; no UTF-8 decoding, so any file the filter
kept is fine.

States: `Code`, `Str { close, verbatim }`, `Line`, `Block { close, depth }`.

- In `Code`, the longest matching opener at the current position wins, so
  Python `"""` beats `"` and Rust `r#"` beats `"`. A string opener enters
  `Str`; a line-comment prefix enters `Line`; a block opener enters
  `Block`. Anything else is copied through.
- `Str` copies through to its closer. A `\` skips the next byte unless the
  string is verbatim.
- `Line` drops to the newline; the newline itself is kept.
- `Block` drops to its closer. When the language nests (Rust, Haskell),
  another opener increments depth and the closer decrements it; the
  state ends at depth zero. Newlines inside are kept for the pass below.
- tokei's `doc_quotes` (Python/Elixir docstrings) are treated as a block
  comment only when the opener is the first non-whitespace on its line —
  statement position. `x = """…"""` is a real string and stays.
- An unterminated string or comment consumes to end of file.

Final pass: each line is right-trimmed and whitespace-only lines are
dropped. `foo(); // bar` leaves `foo();`; a deleted comment block leaves
no hole; pre-existing blank lines go too, which is the same normalization
applied uniformly to every blob.

## CLI and docs

`--comments` (score comments too) and `--prose` (score prose files too)
on `CommonArgs`, in the same shape as `--ignore-tests`: env `CX_COMMENTS`
and `CX_PROSE`, `num_args = 0..=1` with `default_missing_value = "true"`
and the boolish parser, so a pinned default can be vetoed for one run
with `--comments=false`.

README: the filtering paragraph gains the prose layer and the default
statement "cx scores code: comments stripped, prose files skipped"; the
flag block and the env-var sentence gain the two knobs. The JSON contract
gains nothing except the new skip reason string `prose`.

## Testing

- `strip.rs` unit table across syntax families: C-style `//` and `/* */`,
  `#`, Lua `--` and `--[[ ]]`, nested Haskell `{- -}`, OCaml `(* *)`,
  HTML `<!-- -->`; a trailing comment; `"http://x"` and `r#"/* kept */"#`
  survive; a Python docstring is stripped in statement position and kept
  in expression position; an unterminated block; an unknown extension is
  returned untouched; blank-line collapse.
- `filter.rs`: prose table — `.md`, `.rst`, `.txt`, `.adoc`, `.org`
  excluded as `prose`; `.yaml`, `.json`, `.html` kept; `prose: true`
  keeps them all.
- `end_to_end.rs`: a comment-only commit scores ≈0 review and ΔC by
  default and clearly more with `--comments`; `README.md` appears in
  `skipped` as `prose` by default and in `files` with `--prose`.
