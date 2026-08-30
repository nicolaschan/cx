# cx

Score git diffs by **marginal description length**: how much new
information a change adds, given what the codebase already contains. To
estimate this, cx compresses the codebase and then the change in a single
zstd stream — the bytes the change costs after the compressor has seen
everything else are its score. This is language-independent and grounded
in MDL and software naturalness (Hindle et al. 2012; Ray et al. 2016).

Each file gets two independent scores:

- **REVIEW** — what a reviewer who already knows the codebase must newly
  absorb. Repo-conventional plumbing compresses to ≈ 0 even when it spans
  hundreds of lines; a dense 60-line contract change does not.
- **ΔCX** — how much complexity the change adds to (or refunds from) the
  codebase. A full rewrite of equal complexity scores high on REVIEW but
  ≈ 0 on ΔCX. Deleting one of several duplicated copies refunds ≈ 0;
  deleting unique content refunds in full.

```console
$ cx diff
 REVIEW  ΔCX         LINES  PATH                     SHARE
 4.1 KB  +3.6 KB      +436  ├─┬ crates               █████████░  93.0%
 4.1 KB  +3.6 KB      +436  │ └─┬ cx-cli             █████████░  93.0%
 1.5 KB  +1.2 KB      +698  │   ├── report.rs        ███░░░░░░░  32.8%
 1.4 KB  +1.5 KB  +   +169  │   ├── breakdown.rs     ███░░░░░░░  30.4%
     ≈0  −5.0 KB  −   −431  │   └── poller.rs        ░░░░░░░░░░   0.1%
 1.9 KB   +248 B       +92  └── README.md            █░░░░░░░░░   9.5%

 review 4.4 KB   ΔCX +3.9 KB   lines +1059 −531   1 skipped
```

The `+`/`−`/`→` column marks added, deleted, and renamed files (`⚠` for
density outliers, i.e. probable generated content). LINES is net churn.
`--verbose` prints what was skipped and why.

## Usage

```
cx       [-n <N>] [--base <ref>]  # tree breakdown plus the diff's ΔCX per path
cx diff  [-n <N>] [--base <ref>]  # just the diff, sized by review cost
cx abs   [-n <N>]                 # absolute C(tree): the trend-line number

# any of the above: [--staged|--committed] [-v|--verbose] [--no-files]
#                   [-g <glob>] [--comments] [--strings] [--prose] [--data]
#                   [--include-tests] [--json]
```

Every view scores the **working tree** by default: staged and unstaged
changes, plus untracked files that aren't ignored. `--staged` scores the
index; `--committed` scores HEAD.

`-g/--glob` restricts a run to matching paths — gitignore syntax, `!` to
exclude, repeatable, last match wins. The selection is scored as if it
were the whole repository: `cx abs -g 'crates/api/**'` reports the same
number a repo containing only `crates/api` would.

Every flag can be pinned through an environment variable (`CX_COMMENTS=1`,
`CX_BASE=develop`, …) and overridden per run; `cx --help` lists them.
`--json` emits the full report — the stable contract for tooling.

## What gets scored

cx scores **code**. Before compression, every file is reduced using
[tokei](https://github.com/XAMPPRocky/tokei)'s per-language syntax
tables: comments are stripped, string literals are emptied to their
delimiters, and blank lines are dropped. A comment-only or
string-rewording change therefore scores ≈ 0 on both axes. `--comments`
and `--strings` opt those parts back in.

Some files are skipped entirely, each with a flag to opt back in:

- **Prose** (Markdown, plain text, documents like `LICENSE`) — `--prose`.
- **Data** (JSON, XML, SVG, CSV, and similar) — `--data`. Config and
  markup (YAML, TOML, HTML, CSS) count as code.
- **Tests** — `--include-tests`. Recognised by naming convention alone: a
  path segment (split on `/`, `_`, `-`, `.`) of `test`, `tests`, or
  `spec`, or a directory named `e2e`, `mocks`, or `testdata`. So
  `foo_test.go` and `e2e/*` match; `latest.rs` stays production code.

Also filtered: binaries, files marked generated in `.gitattributes`,
common generated/vendored patterns (lockfiles, `vendor/`, minified
assets), and anything matched by a `.cxignore` (gitignore syntax).

Known limits, by design: scores are relative rankings within one repo and
compressor version, not cross-repo numbers; information ≠ verification
effort (twenty subtle `<`→`<=` flips score tiny); and the reference
normalizes the repo's existing sins — cx measures *marginal* complexity
against the codebase as it is.

## Run

```sh
nix run github:nicolaschan/cx
docker run --rm -v "$PWD:/repo" ghcr.io/nicolaschan/cx
```

Prebuilt binaries for Linux and macOS are attached to
[releases](https://github.com/nicolaschan/cx/releases).

## Develop

```sh
nix develop   # shell with the pinned toolchain + rust-analyzer
cargo test
```

`crates/cx-core` is the pure scoring engine (bytes in, scores out — no
git, no I/O); `crates/cx-cli` adds git orchestration, filtering, and
rendering. `nix build` builds and tests; `nix build .#docker` builds the
container image, published multiarch by
[publish-nix-image](https://github.com/nicolaschan/publish-nix-image).
