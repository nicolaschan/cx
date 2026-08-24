# Parallel scoring: what was measured

Scoring file *i* against `reference ++ files[..i]` through
`ZSTD_CCtx_refPrefix` re-indexes the whole prefix on every call (level
19, binary-tree match finder): Σᵢ prefixᵢ ≈ N·total/2 bytes, O(N·total).
No compression reads another's result, so they all run as one rayon
batch. What follows is what the code cannot tell you: the facts that
were verified against zstd 1.5.7 and by measurement.

## Two zstd facts

**Memory layout is an input to zstd.** When the referenced prefix sits
immediately before the input in memory, zstd takes a contiguous-window
code path and emits different bytes: 29 of 40 sampled scores moved by
1–3 bytes once items were scored out of one assembled buffer. Master's
separate allocations were non-adjacent only by accident of the
allocator. `ZSTD_c_deterministicRefPrefix` forces the non-contiguous
path; with it the single-buffer layout matches master on every fixture.
The parameter needs zstd-safe's `experimental` feature (pre-generated
bindings; the C library is unchanged).
`scores_do_not_depend_on_memory_layout` and both chain-rule tests fail
without it.

**Context reuse is output-identical.** One context per worker, checked
out of a pool, instead of a fresh one per job. zstd's index continuation
keeps reuse byte-identical: verified on 427 real files with reuse in
shuffled orders, with one context used serially for everything, and
through the index-overflow correction path at 3.5 GB cumulative (0
mismatches / 125 jobs). Reuse skips zeroing 48–80 MB of tables per job
and, more importantly, the per-job 84 MB mmap/munmap whose page-fault
storms were measured to triple wall-clock under 32 threads. It has to
be a pool: rayon's `map_init` runs once per split job, not per thread —
at 32 threads over 430 items, once per item.

## Ceiling

On 16 cores / 32 threads (sunset: 315 kept files, 5.4 MB) `abs` went
61 s → 5.4 s and the overview 135 s → 11 s. The level-19 match finder
is bound by DRAM random access — 84 MB of tables per context, per-job
time 2.4× slower at 32 concurrent jobs — so 10–12× is the ceiling, not
32×. Cost-descending order is worth 0–5 %; starting joints first about
one contended joint duration; context reuse ~5 % plus the removed
page-fault mode.

Each progress phase gets a fresh indicatif bar: reusing one bar across
phases with a steady tick is a trap (the ticker exits at `finish`, and
`inc` never draws while a ticker handle is installed).

## The O(total) alternative

The O(N·total) shape is what `ref_prefix` costs. One streaming
compression of `reference ++ items`, flushing at each item boundary and
taking the output growth as that item's score, is O(total). Measured on
the same corpus: 1.4 s single-threaded, against ~5 s for this design on
32 threads and 61 s on master; +0.95 % total bytes (≈24 B per flush);
per-item numbers within 0–5 % of the chain scores (entropy tables carry
across blocks instead of being re-learned); Σ items == total by
construction, so `scale` disappears. It changes every golden value and
makes historical scores incomparable, which is why it is a separate,
deliberate decision.
