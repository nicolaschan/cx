# Parallel scoring with progress

## Problem

`cx` on a mid-sized repo (~430 files, 6.6 MB) takes minutes on one core.
The cost is structural: `Scorer::score_sequential` scores file *i* against
`reference ++ files[..i]` through `ZSTD_CCtx_refPrefix`, and zstd
re-indexes the whole prefix (level 19, binary-tree match finder) on every
call. Over N files that is Σᵢ prefixᵢ ≈ N·total/2 bytes of indexing —
O(N·total), single-threaded, with no feedback while it runs.

## Goals

1. Use every core. The numbers must not change: every existing golden and
   invariant test passes untouched, and `--json` output on a real repo is
   byte-identical to master's.
2. Show progress on stderr while scoring, with an ETA that tracks
   wall-clock rather than file count.
3. Lean on the ecosystem: `rayon` for parallelism, `indicatif` for the
   bar. No hand-rolled thread pools or terminal drawing.

Non-goal: changing the estimator. An O(total) streaming estimator exists
(compress the whole stream once, measure per-file output growth) and would
be ~100× faster still, but it changes every score, so it is a separate,
deliberate decision — reported, not made, here.

## Why the compressions are independent

Item *i*'s prefix is `reference ++ item₀ ++ SEP ++ … ++ itemᵢ₋₁ ++ SEP`: a
prefix of one buffer, `reference ++ assemble(items)`. The joint score is
the same buffer split at `reference.len()`. No compression reads another's
result, so all of them — every item of every pass, plus every joint — can
run at once. Parallelism changes scheduling only, never inputs, so scores
are identical by construction.

## Design

### cx-core: `Attribution`

One attribution = the compressions behind a `Rescaled`. A new type owns
the assembled buffer and knows each item's byte range within it:

```rust
pub struct Attribution<'s> {
    scorer: &'s Scorer,
    buffer: Vec<u8>,          // reference ++ item₀ ++ SEP ++ item₁ ++ SEP …
    reference_len: usize,
    items: Vec<Range<usize>>, // each item's bytes within buffer
}

impl Scorer {
    pub fn attribution<'s>(&'s self, reference: &[u8], items: &[&[u8]]) -> Attribution<'s>;
}

impl Attribution<'_> {
    /// Bytes zstd will index and compress over every job — the unit
    /// progress advances in, so a bar over it tracks wall-clock.
    pub fn cost(&self) -> u64;
    pub fn joint_cost(&self) -> u64;
    /// C(itemᵢ | reference ++ items[..i]) for each item, in parallel.
    pub fn sequential(&self, progress: &(dyn Fn(u64) + Sync)) -> Vec<u64>;
    /// C(all items | reference).
    pub fn joint(&self, progress: &(dyn Fn(u64) + Sync)) -> u64;
    /// Sequential ∥ joint, then rescale.
    pub fn run(&self, progress: &(dyn Fn(u64) + Sync)) -> Rescaled;
}
```

- `sequential` is a rayon `par_iter` over item indices, **biggest job
  first** (`.rev()`): costs grow with *i*, so scheduling the long ones
  first keeps the tail short (LPT).
- `Rescaled` gains `joint: u64`. It is not derivable from `scores` and
  `scale` in the degenerate all-zero case, and every caller wants it.
- `score_sequential`, `score_joint`, `score_absolute` stay as thin
  wrappers with a silent progress sink — they are the vocabulary the
  golden and invariant tests are written in.
- Progress is a plain `Fn(u64) + Sync` callback: no trait, no I/O in
  core, and rayon-friendly. `&|_| {}` is silence.
- rayon falls back to serial execution on wasm32 without threads, so the
  WASM boundary is unaffected.

Per-job CCtx reuse (one context per rayon worker via `map_init`) is a
second, measured step: zstd documents context reuse as the fast path
(skips ~80 MB of table zeroing per job) and index continuation is
designed to keep output identical. It stays only if the sunset JSON is
byte-identical with it.

### cx-cli: `progress` module

Wraps indicatif so the pipeline never sees its API:

```rust
pub struct Progress { bar: ProgressBar }
impl Progress {
    /// `visible` is the caller's answer to "is stderr a terminal?" —
    /// main decides, like it does for color.
    pub fn new(visible: bool) -> Self;
    pub fn hidden() -> Self;
    /// Start a phase of `cost` bytes; returns the sink its jobs advance.
    pub fn phase(&self, label: &str, cost: u64) -> Phase<'_>;
}
impl Phase<'_> { pub fn advance(&self, bytes: u64); }
impl Drop for Phase<'_> { /* finish_and_clear */ }
```

Style: a steady-tick spinner, label, bar, percent, ETA. The bar clears
when the phase ends, so the report is the only thing left on screen.

### Pipeline

`pipeline::diff(git, opts, progress)` / `pipeline::abs(git, opts, progress)`
take the sink. `diff` builds its three attributions, starts one phase
sized to their summed cost, and runs the three concurrently
(`par_iter` over passes; rayon nests). `abs` runs one attribution, or
only its joint under `--no-files`. The `seq; joint; rescale` triplet
that currently appears four times in the pipeline collapses into
`attribution.run(..)`.

`main` passes `Progress::new(stderr().is_terminal())`; tests pass
`Progress::hidden()`.

Thread count is rayon's default (all logical CPUs), overridable with
`RAYON_NUM_THREADS` — documented, not wrapped in a flag.

## Testing

- **Exactness**: a cx-core test asserts the parallel `sequential` equals
  a literal serial loop of `score` over the growing prefix, on
  generated inputs; the existing golden test pins absolute values.
- **Cost**: `cost()` equals Σ(prefixᵢ + itemᵢ) + joint over a small
  fixture, so the progress bar's length is a checked quantity.
- **Progress plumbing**: a cx-cli test runs `abs` with a counting sink
  and asserts the advanced bytes sum to the attribution's cost.
- **End-to-end receipt**: `cx abs --json` and `cx --base HEAD~5 --json`
  on ~/src/sunset, master binary vs branch binary, `diff`-identical.
  Benchmark wall-clock before/after with `hyperfine`.
