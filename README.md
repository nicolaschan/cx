# cx

Score git diffs by **marginal description length**: how much new information
a change adds, conditioned on what the codebase already contains. The
estimator is one zstd stream, the codebase then the change —
language-independent,
grounded in MDL / the software-naturalness literature (Hindle et al. 2012;
Ray et al. 2016).

Two independent axes per file, one PR total:

- **REVIEW** — `C(new | old tree)`: what a reviewer who knows the codebase
  must newly absorb. Repo-conventional plumbing compresses to ≈ 0 even when
  it spans hundreds of lines; a dense 60-line contract change does not.
- **ΔCX** — `C(new | remainder) − C(old | remainder)`, where the
  remainder is the tree minus all touched content: how much complexity the
  change adds to (or refunds from) the codebase. A full rewrite of equal
  intrinsic complexity scores REVIEW high, ΔCX ≈ 0. Deleting one of N
  duplicated copies refunds ≈ 0; deleting unique content refunds in full.

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
density outliers).

LINES is net churn: added − deleted, so a file that shrank reads `−431`.
`cx abs` shows the same column as an absolute count, because its bytes
measure the tree rather than the change.

The footer is colored on the same magnitude scale as the cells above,
except the line churn — the familiar size the others are read against,
not a verdict of its own. Those counts are git's `--numstat` minus
whatever cx skipped. `--verbose` adds the rest:

```console
$ cx --verbose
 C(tree) 23.4 KB   review 4.4 KB   ΔCX +3.9 KB   lines +1059 −531   1 skipped
 C(tree) over 23 files (83.7 KB raw)
 skipped: Cargo.lock (generated/vendored pattern)
 zstd 1.5.7, level 19, window≤2^31
```

```
cx       [-n <N>] [--base <ref>]  # overview: one merged table — tree breakdown plus the diff's ΔCX per path
cx diff  [-n <N>] [--base <ref>]  # just the diff, sized by review cost
cx abs   [-n <N>]                 # absolute C(tree): the trend-line number

# any of the above: [--staged|--committed] [-v|--verbose] [--no-files]
#                   [-g <glob>] [--comments] [--strings] [--prose] [--data]
#                   [--include-tests] [--json]
```

Every view scores the **working tree** by default — staged and unstaged
changes, plus untracked files that aren't ignored. `--staged` scores the
index; `--committed` scores HEAD. `abs` takes the same choice — `C(tree)`
and `ΔCX` describe the same snapshot.

Defaults can be pinned through the environment — `CX_COMMENTS=1`,
`CX_STRINGS=1`, `CX_PROSE=1`, `CX_DATA=1`, `CX_INCLUDE_TESTS=1`,
`CX_TOP=15`, `CX_BASE=develop`,
`CX_GLOB='src/**'` — and any single run can still override them on the
command line (`--comments=false`, `-n 50`). `cx --help` lists which
variable backs each flag.

## Scoping to part of a repo

**Where you run cx is what cx measures.** A run inside a subdirectory
sizes that subtree as its own codebase and names its files from there,
exactly as `-g 'that/subdirectory/**'` would from the root. `cd` to the
repository root for the whole-repo number.

```console
$ cd crates/cx-cli && cx       # this subsystem's C(tree) and ΔCX, its own paths
```

`-g/--glob` narrows a run further — gitignore syntax, `!` to exclude,
repeatable, and among globs the last match wins. Same spelling as
ripgrep's `-g` and as a `.cxignore` line, because it is the same matcher
underneath, but read from a different place: a glob reads from the
directory cx runs in, like every path it prints, while `.cxignore` is the
repository's own file and names paths from the repository root wherever
you run.

```console
$ cx abs -g 'crates/cx-cli/**'          # size one subsystem
$ cx -g '!**/generated/**'              # leave a directory out of the diff
$ cx abs -g 'src/**' -g '!src/legacy/**'
```

What a run selects — by where it stands, by its globs, or both — is
scored **as if it were the whole repository**: a path outside the scope
is in no reference and no scoring pass, and does not appear in the
skipped list either — cx never looked at it. So `cx abs -g 'crates/api/**'`
gives that subtree's own `C(tree)`, the same number a repo containing
only `crates/api` would report, and on a large repository cx never
fetches the rest. Subtree scores do not add up to the whole: the
repo-wide number charges shared patterns once, which is the point of the
metric.

A file that arrives from outside the scope counts as an add rather than a
rename, and one that leaves counts as a delete: the other end of the move
is not part of this codebase, which is how the score already reads it.
`-v` names the directory a run is rooted in, below the summary.

Directories work as they do in gitignore. `-g '!target'` prunes that
subtree without a `target/**`, and — as in gitignore — nothing inside an
excluded directory can be added back, so carve out with a narrower
exclude (`-g '!vendor/lib/**'`) rather than excluding the parent.

The tree breakdown is dust-style: contributions aggregate up the
directory tree, only the `-n` globally biggest files/directories are
shown (default 30), and everything pruned collapses into a per-directory
`… +N more` row — so the view stays one screen even on repos with
thousands of files. `--no-files` suppresses the breakdown. Output
colorizes on a terminal and degrades to plain text when piped; a progress
bar on stderr tracks the run while it is a terminal.

`--json` emits the full report (per-file scores, skipped files, totals,
compressor version) — the stable contract for tooling.

The stream is flushed at every file, so a file's score is the bytes it
adds — the chain rule: a pattern repeated across files in one PR is
charged once, at its first occurrence, and per-file scores sum exactly to
the total.

cx scores **code**. Before anything is compressed, every file is reduced
to its code: comments are stripped, string literals are emptied to their
delimiters (the string counts, its contents do not), and blank lines are
dropped, using [tokei](https://github.com/XAMPPRocky/tokei)'s
per-language syntax table (line and block comment delimiters, nesting,
string quotes — so a `//` inside a string literal opens no comment) for
the 300-odd languages it knows; anything else passes through untouched.
A comment-only or string-rewording change then scores ≈0 on both axes.
`--comments` scores comments too; `--strings` scores string contents.

Prose files — Markdown, reStructuredText, plain text, AsciiDoc, Org, and
extensionless documents such as `LICENSE`, `README`, `CHANGELOG` — are
skipped entirely. `--prose` scores them. So are data files — JSON, XML,
SVG, and the tabular or line-delimited formats tokei has no language for
(CSV, TSV, JSON Lines, GeoJSON); `--data` scores them. Config and markup
(YAML, TOML, HTML, CSS) are code.

Files are filtered before scoring: `.gitattributes` linguist annotations,
binary detection, common generated/vendored patterns (lockfiles, `dist/`,
`vendor/`, minified assets…), prose, data files, and a `.cxignore`
(gitignore syntax).

Test files go too, recognised by naming convention alone — no language,
build system, or parser. `--include-tests` scores them anyway, for the
runs that want the whole picture. A path is a test when any
segment of it, split on `/`, `_`, `-`, and `.`, is `test`, `tests`, or
`spec`, or when a *directory* segment is `e2e`, `mocks`, or `testdata`.
So `foo_test.go`, `foo-test.js`, `foo.test.ts`, `test_foo.py`,
`tests.rs`, and `e2e/*` are one convention in different separators,
while `latest.rs` stays production code and a `…-e2e-design.md` stays a
document about tests.

Deliberate consequences: plural `specs/` is documentation; test support
like `test_helpers.rs` is test code; names only one toolchain knows
(`conftest.py`, `FooTest.java`) go undetected, since finding them means
teaching cx one ecosystem at a time; and inline `#[cfg(test)]` needs
hunk scoring to exclude. Density
outliers (bytes-per-line far from the run median on added files) are
flagged `⚠`, not dropped — probable generated content no pattern
anticipated.

Known limits, by design: scores are relative rankings within one repo and
compressor version, not absolute cross-repo numbers; information ≠
verification effort (twenty subtle one-bit `<`→`<=` flips score tiny); the
reference normalizes the repo's existing sins — this measures *marginal*
complexity against the codebase as it is.

## Run

```sh
nix run github:nicolaschan/cx
docker run --rm -v "$PWD:/repo" ghcr.io/nicolaschan/cx
```

Prebuilt binaries for Linux (x86_64, aarch64) and macOS (aarch64) are
attached to [releases](https://github.com/nicolaschan/cx/releases); tagging
`v*` publishes them.

## Develop

```sh
nix develop   # shell with the pinned toolchain (rust-toolchain.toml) + rust-analyzer
cargo test
```

Workspace layout: `crates/cx-core` is the pure scoring engine (bytes in,
scores out — no git, no I/O; the future WASM boundary), `crates/cx-cli`
adds git orchestration, filtering, and rendering. The Rust toolchain and
all dependencies come in through `flake.nix` via
[oxalica/rust-overlay](https://github.com/oxalica/rust-overlay). `nix build`
builds the binary and runs the tests; `nix build .#docker` builds the
container image, published multiarch by
[publish-nix-image](https://github.com/nicolaschan/publish-nix-image).
