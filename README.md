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
- **ΔE** — `C(new | remainder) − C(old | remainder)`, where the
  remainder is the tree minus all touched content: how much complexity the
  change adds to (or refunds from) the codebase. A full rewrite of equal
  intrinsic complexity scores REVIEW high, Δ ≈ 0. Deleting one of N
  duplicated copies refunds ≈ 0; deleting unique content refunds in full.

```console
$ cx score
 REVIEW    ΔE    B/LINE   PATH
  2.0 KB    +2.0 KB       13   crates/cx-core/src/lib.rs (added)
  1.7 KB    +1.7 KB       11   crates/cx-core/tests/invariants.rs (added)
   459 B     +448 B       15   crates/cx-core/tests/golden.rs (added)
      ≈0         ≈0        -   crates/cx-cli/src/main.rs (renamed from src/main.rs)
──────────────────────────────────────────────
 PR total: review 4.5 KB, ΔE +4.3 KB
 attribution scale: 0.94 (ok)   zstd 1.5.7, level 19, window≤2^31
 skipped: Cargo.lock (generated/vendored pattern)
```

```
cx                                            # overview: tree score with per-file
                                              #   contributions, then the diff score
cx score [--base <ref>] [--staged] [--json]   # score merge-base..HEAD (or the index)
cx tree  [--no-files] [--json]                # absolute C(tree): the trend-line number
```

`--no-files` (also on the bare `cx`) suppresses per-file tree
contributions — and skips computing them, which matters on large trees.
Tables colorize on a terminal and degrade to plain text when piped.

`--json` emits the full report (per-file scores, skipped files, totals,
scale factors, compressor version) — the stable contract for tooling.

Per-file attribution is sequential (chain rule): a pattern repeated across
files in one PR is charged once, at its first occurrence. Sums are robust;
the `attribution scale` line is the built-in noise gauge (≈ 1.0 → trust
per-file numbers, far off → trust totals).

Files are filtered before scoring: `.gitattributes` linguist annotations,
binary detection, common generated/vendored patterns (lockfiles, `dist/`,
`vendor/`, minified assets…), and a `.cxignore` (gitignore syntax). Density
outliers (`B/LINE` far from the run median on added files) are flagged, not
dropped — probable generated content no pattern anticipated.

Known limits, by design: scores are relative rankings within one repo and
compressor version, not absolute cross-repo numbers; information ≠
verification effort (twenty subtle one-bit `<`→`<=` flips score tiny); the
reference normalizes the repo's existing sins — this measures *marginal*
complexity against the codebase as it is.

## Run

```sh
nix run github:nicolaschan/cx -- score
docker run -v "$PWD:/repo" -w /repo ghcr.io/nicolaschan/cx score
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
