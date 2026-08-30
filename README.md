# cx

cx scores git diffs by how much new information they add to the codebase.

The idea comes from minimum description length: code that repeats existing
patterns is cheap, and code that introduces new ideas is expensive. To
measure this, cx compresses the codebase first and the change second, in
one zstd stream. The compressed size of the change is its score. This
works for any language (Hindle et al. 2012; Ray et al. 2016).

Each file gets two scores:

- **REVIEW** — how much a reviewer who knows the codebase must newly
  absorb. Plumbing that follows repo conventions compresses to ≈ 0, even
  across hundreds of lines. A dense 60-line contract change does not.
- **ΔCX** — how much complexity the change adds to the codebase, or
  removes from it. A full rewrite scores high on REVIEW but ≈ 0 on ΔCX.
  Deleting one copy of duplicated code refunds ≈ 0. Deleting unique code
  refunds in full.

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

The `+`/`−`/`→` column marks added, deleted, and renamed files. `⚠` marks
a density outlier, which is probably generated content. LINES is net
churn. `--verbose` shows what was skipped and why.

## Usage

```
cx       [-n <N>] [--base <ref>]  # tree breakdown plus the diff's ΔCX per path
cx diff  [-n <N>] [--base <ref>]  # just the diff, sized by review cost
cx abs   [-n <N>]                 # absolute C(tree): the trend-line number

# any of the above: [--staged|--committed] [-v|--verbose] [--no-files]
#                   [-g <glob>] [--comments] [--strings] [--prose] [--data]
#                   [--include-tests] [--json]
```

By default, every view scores the **working tree**: staged changes,
unstaged changes, and untracked files. `--staged` scores the index
instead. `--committed` scores HEAD.

`-g/--glob` limits a run to matching paths. It uses gitignore syntax:
`!` excludes, the flag repeats, and the last match wins. cx scores the
selection as if it were the whole repository. So `cx abs -g
'crates/api/**'` reports the same number as a repo that contains only
`crates/api`.

Every flag has a matching environment variable, like `CX_COMMENTS=1` or
`CX_BASE=develop`. Flags on the command line override them. `cx --help`
lists the pairs.

`--json` prints the full report. It is the stable contract for tooling.

## What gets scored

cx scores **code**, not text. Before compressing a file, it strips
comments, empties string literals, and drops blank lines. It uses
[tokei](https://github.com/XAMPPRocky/tokei)'s syntax tables to do this
for each language. As a result, a change that only rewords comments or
strings scores ≈ 0. Use `--comments` or `--strings` to score those parts
too.

Three kinds of files are skipped entirely. Each has a flag to opt back
in:

- **Prose**: Markdown, plain text, and documents like `LICENSE`. Flag:
  `--prose`.
- **Data**: JSON, XML, SVG, CSV, and similar. Config and markup (YAML,
  TOML, HTML, CSS) still count as code. Flag: `--data`.
- **Tests**: recognised by name alone. A path is a test when one of its
  segments is `test`, `tests`, or `spec`, or when a directory is named
  `e2e`, `mocks`, or `testdata`. Segments split on `/`, `_`, `-`, and
  `.`, so `foo_test.go` matches and `latest.rs` does not. Flag:
  `--include-tests`.

cx also skips binaries, files marked generated in `.gitattributes`,
common generated or vendored patterns (lockfiles, `vendor/`, minified
assets), and anything listed in a `.cxignore` file.

Known limits, by design:

- Scores are relative rankings within one repo and one compressor
  version. They do not compare across repos.
- Information is not verification effort. Twenty subtle `<`→`<=` flips
  score tiny but take a long time to check.
- The current codebase is the baseline, existing sins included. cx
  measures the complexity added on top of it.

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

`crates/cx-core` is the pure scoring engine: bytes in, scores out, no git
or I/O. `crates/cx-cli` adds git handling, filtering, and rendering.
`nix build` builds and tests. `nix build .#docker` builds the container
image, and
[publish-nix-image](https://github.com/nicolaschan/publish-nix-image)
publishes it multiarch.
