# Streaming estimator

## What changed

A score used to be one zstd frame per file, with `ref_prefix` set to
`reference ++ files[..i]`. zstd re-indexes the whole prefix for every
frame, so a run cost Σᵢ prefixᵢ ≈ N·total/2 bytes of level-19 indexing:
61 s for `cx abs` on a 315-file, 5.4 MB repo, 135 s for the overview.

Now a pass is one stream. The reference parts go in first, then the
items, with a flush at every part boundary; an item's score is the bytes
its part added to the output. Every byte is indexed once, so a pass costs
one compression of its stream: `abs` 0.97 s, `diff` 1.2 s (three passes
on three threads), overview 2.2 s, on the same repo. Memory is one zstd
context per concurrent stream — ~85 MB of tables plus a copy of the
stream, 2^WindowLog — three at once for `diff`. Pledging the stream's
size would shrink that, but lets zstd pick size-class search parameters
for small streams; unpledged, every stream is searched the same way.

## Why this is the better number, not just the faster one

C(x | context) estimated with a compressor is C(context·x) − C(context):
compress both as one stream and take what x added. That is what the
stream does. The old frame-per-file form handed zstd the context as a
dictionary of matches only; every file relearned its coding model from
scratch. In the stream the entropy tables, repeat offsets, and parser
statistics learned from the context carry into each item, and zstd keeps
inherited tables only when they are cheaper than fresh ones. Per-file
scores are consecutive slices of one output, so they sum to the total by
construction — `rescale`, `scale`, and the "attribution scale" gauge are
gone, and scores are integers. Flushing at hunk boundaries instead of
file boundaries would cost nothing extra in time (each hunk pays
~10–30 B of block framing), where the old shape would have been
O(hunks · total).

## Mechanics

- Parameters as before: level 19, `NbWorkers(0)`, long-distance matching,
  `WindowLog` = the smallest window covering the whole stream (reference
  and items) clamped to [10, 31]. No content size, no checksum.
- Each part is fed with `ZSTD_e_flush` until zstd reports nothing pending
  and the part is consumed. No `ZSTD_e_end`: its bytes belong to no part.
- The first non-empty part of a stream absorbs the frame header (6 B);
  every non-empty part pays its block header (3 B); empty parts cost 0. For `abs` that lands on the first file in
  path order; for `diff` on the discarded reference. Nothing is
  subtracted: under 64 B renders as ≈0 anyway.
- No separators. A flush ends the block, so no encoded sequence spans two
  parts; the old 15-byte `SEPARATOR` existed to fake that boundary.
- The output buffer grows as zstd asks for room, so nothing rests on a
  size bound; every call has space for a block and so makes progress.
- zstd copies input into its own window buffer, so a score is a function
  of the bytes alone, not of where the caller's parts live in memory
  (`scores_do_not_depend_on_memory_layout`).
- A part's score is a function of the bytes before it: appending anything
  after it, even enough to grow the window, leaves it untouched
  (`later_items_do_not_change_earlier_scores`) — unpledged, zstd's search
  parameters do not vary with the stream's size, only the window does.
  Reference vs items is a label on one stream: moving the line moves
  nothing (`attributed_scores_are_per_item_conditionals`). Reference file
  boundaries are flush boundaries by design; chunking the reference
  differently moves item scores by a few bytes.

## What moved

Sunset, 315 files, old estimator → stream: C(tree) 916,160 → 925,471
(+1.0 %, the per-flush block overhead); per-file ratio median 1.004,
p10 0.94, p90 1.02, extremes 0.81 (a 72-byte file) and 1.06 (a 2 KB TOML
file between Rust files — the optimal parser's price statistics carry
across parts and are not re-chosen per block, so a file unlike its
predecessor can pay a little — the entropy coder never pays more than
fresh tables would, the parser's prices are inherited and only rescaled;
~0.5 % worst observed after an unrelated part). Diff on the same repo: review 35,018 →
35,757 (+2 %), ΔC 31,232 → 31,215, per-file review ratios 0.95–1.04.
Golden values re-pinned: `C(REFERENCE)` 60 → 69, `C(NOVEL)` 83 → 92, the
two-item attribution [77, 19] → [80, 22]. The old estimator subtracted
9 B per frame (a 6 B header plus a 3 B empty block a non-empty frame
never carries); the stream pays the 6 B once, where it occurs, and each
part its own 3 B block header.

## Contract changes

- `review_bytes`/`bytes` are `u64`, `delta_bytes` is `i64`; `review_raw`,
  `scales`, and `scale` are gone; `raw_bytes` is the kept files' sizes.
- `--no-files` only hides the breakdown; per-file attribution is free.
- `--verbose` no longer prints an attribution scale.
- Historical scores are not comparable with new ones. The README already
  scopes comparability to one repo and one compressor version; this is a
  version bump of the estimator, made once, on purpose.

## Superseded

PR #13 kept the frame-per-file estimator byte-for-byte and ran the frames
in parallel (12× on 32 threads, DRAM-bound). It found that zstd emits
different bytes when a prefix is memory-adjacent to its input and that
per-frame context reuse is output-identical; neither matters here, since
there is no prefix and one context per stream.
