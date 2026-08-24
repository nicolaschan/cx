# cx: streaming estimator design (2026-08-23)

Approved design for scoring by one zstd stream per pass, replacing the
frame-per-file estimator. The mechanics are in `cx-core`'s doc comments
and the motivation in PR #16; this records what was verified about zstd
and what the change measured.

## zstd facts the design rests on

- A `ZSTD_e_flush` ends the block, so no encoded sequence spans two
  parts; the old 15-byte `SEPARATOR` existed to fake that boundary. No
  `ZSTD_e_end`: its epilogue bytes belong to no part.
- The first non-empty part absorbs the 6 B frame header — the first file
  in path order for `abs`, the discarded reference for `diff` — and every
  non-empty part its 3 B block header; empty parts cost 0. Nothing is
  subtracted: under 64 B renders as ≈0 anyway.
- Entropy tables, repeat offsets, and parser statistics learned from the
  context carry into each item; zstd keeps inherited entropy tables only
  when they are cheaper than fresh ones, but the optimal parser's price
  statistics are inherited and only rescaled, never re-chosen per block,
  so a file unlike its predecessor can pay a little (~0.5 % worst
  observed).
- Unpledged, zstd's search parameters do not vary with the stream's
  size, only the window does, so appending items never changes earlier
  scores. Pledging the size would shrink memory but lets zstd pick
  size-class parameters for small streams.
- zstd copies input into its own window buffer, so a score is a function
  of the bytes alone, not of where the caller's parts live in memory.
- Reference file boundaries are flush boundaries; chunking the reference
  differently moves item scores by a few bytes.

## Cost

Old: Σᵢ prefixᵢ ≈ N·total/2 bytes of level-19 indexing per pass. New:
one compression of the stream. Sunset (315 files, 5.4 MB): `abs` 61 s →
0.97 s, `diff` 74 s → 1.2 s (three passes on three threads), overview
135 s → 2.2 s. Memory is one context per concurrent stream — ~85 MB of
tables plus a 2^WindowLog copy of the stream — three at once for `diff`.
Flushing at hunk boundaries later costs ~10–30 B of block framing each
and no time, where the old shape would have been O(hunks · total).

## What moved

Sunset, old → stream: C(tree) 916,160 → 925,471 (+1.0 %, per-flush block
overhead); per-file ratio median 1.004, p10 0.94, p90 1.02, extremes
0.81 (a 72-byte file) and 1.06 (a 2 KB TOML file between Rust files).
Diff: review 35,018 → 35,757 (+2 %), ΔC 31,232 → 31,215, per-file review
ratios 0.95–1.04. Golden values re-pinned: `C(REFERENCE)` 60 → 69,
`C(NOVEL)` 83 → 92, attribution [77, 19] → [80, 22] — the old estimator
subtracted 9 B per frame (a 6 B header plus a 3 B empty block a non-empty
frame never carries); the stream pays each where it occurs.

## Contract changes

- `review_bytes`/`bytes` are `u64`, `delta_bytes` is `i64`; `review_raw`,
  `scales`, and `scale` are gone; `raw_bytes` is the kept files' sizes.
- `--no-files` only hides the breakdown; `--verbose` no longer prints an
  attribution scale.
- Historical scores are not comparable with new ones: a version bump of
  the estimator, made once, on purpose.
