# Parallel scoring with progress

## Problem

`cx` on a mid-sized repo (315 kept files, 5.4 MB) took 61 s for `abs`
and 135 s for the overview, on one core, silently. The cost is
structural: scoring file *i* against `reference ++ files[..i]` through
`ZSTD_CCtx_refPrefix` re-indexes the whole prefix (level 19, binary-tree
match finder) on every call — Σᵢ prefixᵢ ≈ N·total/2 ≈ 850 MB of
indexing. O(N·total), not O(total).

## Goals

1. Use every core without changing a single score: golden and invariant
   tests pass, and `--json` on a real repo matches master's.
2. Progress on stderr while scoring, with an ETA that tracks wall-clock.
3. Lean on the ecosystem: `rayon` for parallelism, `indicatif` for the
   bar.

Non-goal: a faster estimator. See "The faster way" below.

## Why the compressions are independent

Item *i*'s prefix is `reference ++ item₀ ++ SEP ++ … ++ itemᵢ₋₁ ++ SEP`,
a prefix of the one buffer `reference ++ assemble(items)`; the joint is
that buffer split at `reference.len()`. No compression reads another's
result, so every item of every pass and every joint can run at once.

## What the review changed

Two facts, verified against zstd 1.5.7's source and by measurement,
reshaped the first draft:

- **Memory layout is an input to zstd.** When the referenced prefix sits
  immediately before the input in memory, zstd takes a contiguous-window
  code path and emits different bytes (29 of 40 sampled scores moved by
  1–3 bytes). Master's separate allocations were non-adjacent by
  accident of the allocator. `ZSTD_c_deterministicRefPrefix` forces the
  non-contiguous path; with it, the single-buffer layout matches master
  on every fixture, and the old latent hazard is gone. It needs
  zstd-safe's `experimental` feature (pre-generated bindings only; the C
  library is unchanged).
- **`map_init` is not per-thread.** rayon calls its init once per split
  job — at 32 threads over 430 items, once per item. Context reuse needs
  a checkout pool instead.

## Design

### cx-core

```rust
pub trait Progress: Sync { fn advance(&self, bytes: u64); }
pub struct Silent;                       // no reporting

impl Scorer {
    /// C(input | reference): the primitive. Empty reference → C(input).
    pub fn score(&self, reference: &[u8], input: &[u8]) -> u64;
    pub fn attribution<'s>(&'s self, reference: &[u8], items: &[&[u8]]) -> Attribution<'s>;
}

pub struct Attribution<'s> { scorer, buffer, reference_len, items: Vec<Range<usize>> }
impl Attribution<'_> {
    pub fn cost(&self) -> u64;                                // Σ over jobs of prefix + input
    pub fn run(&self, progress: &dyn Progress) -> Rescaled;   // = run_all([self], ..)
}
pub fn run_all<const N: usize>(plans: [&Attribution; N], progress: &dyn Progress) -> [Rescaled; N];

pub struct Rescaled { scores, scale, joint: u64 }             // joint: the rescale target
```

- `run_all` flattens every plan's jobs (N items + 1 joint each) into one
  list, sorts by cost descending — so joints and late items start
  first and the tail stays short — and runs it as one rayon batch with
  `with_max_len(1)`, so each compression is its own stealable task.
- Contexts come from a `Mutex<Vec<CCtx>>` checkout pool: one live
  context per worker, reused across jobs. zstd's index continuation
  skips zeroing 48–80 MB of tables per job and, more importantly, the
  per-job 84 MB mmap/munmap whose page-fault storms under 32 threads
  were measured to triple a run's wall-clock. Output is byte-identical
  (verified on 427 real files with reuse, shuffled orders, and one
  context used serially for everything).
- `DeterministicRefPrefix(true)` sits next to `NbWorkers(0)` under the
  same "determinism by construction" rationale.
- `score_sequential`, `score_joint`, `score_absolute` are gone: the
  tests' real vocabulary was `score(reference, x)` (nine of eleven uses
  were single-item) and one attribution.
- rayon-core falls back to serial execution when threads are
  unsupported (the implicit global pool retries with one thread on
  `io::ErrorKind::Unsupported`), so the WASM boundary is unaffected as
  long as cx-core never calls `build_global`.

### cx-cli

`progress.rs` wraps indicatif so nothing else sees it:

```rust
#[derive(Clone, Copy, Default)]
pub struct Progress { visible: bool }        // main decides stderr.is_terminal()
impl Progress {
    pub fn phase(self, label: &str, cost: u64) -> Phase;   // bar over cost bytes
    pub fn spinner(self, label: &str) -> Phase;            // one indivisible job
}
pub struct Phase(ProgressBar);               // clears itself on drop
impl cx_core::Progress for Phase { .. }      // inc(bytes) from any thread
```

Each phase owns a fresh bar: reusing one bar across phases with a steady
tick is a confirmed indicatif trap (the ticker exits at `finish`, and
`inc` never draws while a ticker handle is installed). A hidden phase
is a hidden bar with no ticker thread.

`pipeline::diff(git, opts, progress)` builds three attributions and runs
them as one `run_all` batch under a `"diff"` phase sized to their summed
cost. `pipeline::abs` runs one attribution under `"C(tree)"`, or under
`--no-files` computes `score(&[], assemble(kept))` behind a spinner and
wraps it in `rescale(&[], ..)` — the same report shape with nothing
attributed. The overview shows the two phases in sequence.

`raw_bytes` becomes Σ file sizes. Master reported `assemble(kept).len()`,
which counted 15 separator bytes per file — an artifact of the
estimator, not a property of the tree. Every other JSON field is
byte-identical to master.

## Testing

- `scores_do_not_depend_on_memory_layout`: adjacent vs separate
  allocations score the same. Fails without the flag, as do the two
  chain-rule tests below.
- `attributed_scores_are_per_item_conditionals`: every attributed score
  equals `score(explicit prefix, item)` — the serial definition as the
  oracle for the parallel run.
- `progress_advances_by_exactly_the_planned_cost`: `cost()` equals the
  written-out formula, and a counting sink over `run` sums to it, with
  more items than threads.
- Golden values unchanged; end-to-end suite runs with `Progress::hidden()`.
- Receipt on ~/src/sunset: `abs`, `abs --no-files`, `diff --base HEAD~5`,
  and the overview, master binary vs branch, JSON identical except
  `raw_bytes` (off by exactly 15 × file_count).

## Expectations

On 16 cores / 32 threads the level-19 match finder is bound by DRAM
random-access throughput (84 MB of tables per context; per-job time
rises 2.4× at 32 concurrent jobs), so the realistic ceiling is 10–12×,
not 32×. Ordering is worth 0–5 %, joints-first about one contended joint
duration, context reuse ~5 % plus the removed page-fault mode.

## The faster way

The O(N·total) shape is what `ref_prefix` costs. An O(total) estimator
exists: one streaming compression of `reference ++ items`, flushing at
each item boundary and taking the output growth as that item's score.
Measured on the same corpus: 1.4 s single-threaded for what takes the
parallel design ~5 s on 32 threads and master 61 s; +0.95 % total bytes
(≈24 B per flush); per-item numbers within 0–5 % of the chain scores
(entropy tables carry across blocks instead of being re-learned); Σ items
== total by construction, so `scale` disappears. It changes every golden
value and every historical number, which is why it is a separate,
deliberate decision.
