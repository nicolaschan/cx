# cx: streaming estimator design (2026-08-23)

Approved design for scoring by one zstd stream per pass, replacing the
frame-per-file estimator. The mechanics are in `cx-core`'s doc comments,
the motivation and measurements in PR #16; this records only what was
verified about zstd that neither says.

- A `ZSTD_e_flush` ends the block, so no encoded sequence spans two
  parts; `ZSTD_e_end` is never issued because its epilogue bytes belong
  to no part.
- The first non-empty part absorbs the 6 B frame header — the first file
  in path order for `abs`, the discarded reference for `diff` — and every
  non-empty part its 3 B block header; empty parts cost 0. Nothing is
  subtracted: under 64 B renders as ≈0 anyway.
- zstd keeps inherited entropy tables only when they are cheaper than
  fresh ones, but the optimal parser's price statistics are inherited and
  only rescaled, never re-chosen per block (~0.5 % worst observed).
- Unpledged, zstd's search parameters do not vary with the stream's
  size, only the window does, so appending items never changes earlier
  scores. Pledging the size would shrink memory but lets zstd pick
  size-class parameters for small streams.
- Reference file boundaries are flush boundaries; chunking the reference
  differently moves item scores by a few bytes.
- Memory is one context per concurrent stream — ~85 MB of tables plus a
  2^WindowLog copy of the stream — three at once for `diff`.
- A flush at a hunk boundary later costs ~10–30 B of block framing and
  no time.
