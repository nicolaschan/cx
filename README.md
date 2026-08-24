# cx

Score git diffs by **marginal description length**: how much new information
a change adds, conditioned on what the codebase already contains. The
estimator is zstd with the repo as a reference prefix — language-independent,
grounded in MDL / the software-naturalness literature (Hindle et al. 2012;
Ray et al. 2016).

Two independent axes per file, one PR total:

- **REVIEW** — `C(new | old tree)`: what a reviewer who knows the codebase
  must newly absorb. Repo-conventional plumbing compresses to ≈ 0 even when
  it spans hundreds of lines; a dense 60-line contract change does not.
- **ΔC** — `C(new | remainder) − C(old | remainder)`, where the
  remainder is the tree minus all touched content: how much complexity the
  change adds to (or refunds from) the codebase. A full rewrite of equal
  intrinsic complexity scores REVIEW high, Δ ≈ 0. Deleting one of N
  duplicated copies refunds ≈ 0; deleting unique content refunds in full.

```console
$ cx diff
 REVIEW  ΔC          LINES  PATH                     SHARE
 4.1 KB  +3.6 KB      1206  ├─┬ crates               █████████░  93.0%
 4.1 KB  +3.6 KB      1206  │ └─┬ cx-cli             █████████░  93.0%
 1.5 KB  +1.2 KB       267  │   ├── report.rs        ███░░░░░░░  32.8%
 1.4 KB  +1.5 KB  +    169  │   ├── breakdown.rs     ███░░░░░░░  30.4%
     ≈0  −5.0 KB  −      -  │   └── poller.rs        ░░░░░░░░░░   0.1%
 1.9 KB   +248 B        92  └── README.md            █░░░░░░░░░   9.5%

 review 4.4 KB   ΔC +3.9 KB   1 skipped
```

The `+`/`−`/`→` column marks added, deleted, and renamed files (`⚠` for
density outliers).

The footer line carries what the run is for — C(tree) where the view has
it, review, ΔC — colored on the same magnitude scale as the cells above.
`--verbose` adds the rest:

```console
$ cx --verbose
 C(tree) 23.4 KB   review 4.4 KB   ΔC +3.9 KB   1 skipped
 C(tree) over 23 files (83.7 KB raw)
 skipped: Cargo.lock (generated/vendored pattern)
 attribution scale: 0.94 (ok)   zstd 1.5.7, level 19, window≤2^31
```

```
cx       [-n <N>] [--base <ref>] [--staged]   # overview: one merged table — tree
                                              #   breakdown plus the diff's ΔC per path
cx diff  [-n <N>] [--base <ref>] [--staged]   # just the diff, sized by review cost
cx abs   [-n <N>]                             # absolute C(tree): the trend-line number

# every view: [-v|--verbose] [--no-files] [--ignore-tests] [--json]
```

Defaults can be pinned through the environment — `CX_IGNORE_TESTS=1`,
`CX_TOP=15`, `CX_BASE=develop` — and any single run can still override
them on the command line (`--ignore-tests=false`, `-n 50`). `cx --help`
lists which variable backs each flag.

The tree breakdown is dust-style: contributions aggregate up the
directory tree, only the `-n` globally biggest files/directories are
shown (default 30), and everything pruned collapses into a per-directory
`… +N more` row — so the view stays one screen even on repos with
thousands of files. `--no-files` suppresses the breakdown, leaving the
footer alone (on `cx` and `cx abs` it also skips computing it entirely).
Output colorizes on a terminal and degrades to plain text when piped.

`--json` emits the full report (per-file scores, skipped files, totals,
scale factors, compressor version) — the stable contract for tooling.

Per-file attribution is sequential (chain rule): a pattern repeated across
files in one PR is charged once, at its first occurrence. Sums are robust;
`--verbose`'s `attribution scale` line is the built-in noise gauge (≈ 1.0
→ trust per-file numbers, far off → trust totals).

Files are filtered before scoring: `.gitattributes` linguist annotations,
binary detection, common generated/vendored patterns (lockfiles, `dist/`,
`vendor/`, minified assets…), and a `.cxignore` (gitignore syntax).

`--ignore-tests` adds test files, recognised by naming convention
alone — no language, build system, or parser. A path is a test when any
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
