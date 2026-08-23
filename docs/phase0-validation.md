# Phase 0: validation gate results

**Verdict: GO.** Compression ranking beats the dumb baseline on every case
where they disagree, including the cross-file repeated-plumbing case the gate
was specifically designed to probe.

## Method

`scripts/phase0.sh` computes crude metric 1 (review cost) for the changed
files of historical merges: new file content compressed with
`zstd -19 --long=27 --single-thread --patch-from=<old tree>`, where the
reference is every code file at the merge-base concatenated with separators,
grown sequentially with each scored file (crude chain rule). Score =
compressed size minus the empty-input frame constant.

Evaluated on 20 first-parent feature merges of the `sunset` repo (Rust
workspace + Gleam/Lustre web UI, ~1.8 MB of code) — 291 file-scores.
Baseline: changed line count (added + deleted). The plan's alternative
baseline (lizard CCN-delta) was skipped: lizard cannot parse Gleam at all,
which is itself the point — compression is language-independent.

Judgment calls below were made by reading the actual diffs (adapted from the
plan's "PRs you remember reviewing", since this run was autonomous — the
evidence is laid out so the go/no-go can be overruled).

## Headline results

On ~13/20 merges both metrics agree on the top file (e.g.
`supervisor.rs` 657 lines / 3.2 KB for connection-liveness). The signal is in
the disagreements:

**Repeated plumbing discounted** (high lines, low cx — verified by diff):

| merge | file | cx bytes | lines |
|---|---|---|---|
| peer-status-ui | `views/peer_status_popover.gleam` (new file) | 28 | 198 |
| mobile-friendly | `views/phone_header.gleam` (new file) | 12 | 220 |
| mobile-friendly | `views/voice_minibar.gleam`, `drawer.gleam`, `bottom_sheet.gleam`, … | 23–65 each | 104–124 |
| sunset-core-liveness | `tests/liveness_with_bus.rs` | 41 | 175 |

`phone_header.gleam` is 220 lines of repo-conventional Lustre view code —
the same `ui.css([#("position", "sticky"), …])` idiom as a dozen existing
views. A reviewer who knows this codebase skims it. Line count ranks it #1
in its merge; compression prices it at 12 bytes. This is exactly the failure
mode per-hunk CCN/line-count has and cannot fix, because each new view is
individually "complex" — it's only cheap *given the other views*.

**Dense novelty surfaced** (low lines, high cx — verified by diff):

| merge | file | cx bytes | lines |
|---|---|---|---|
| peer-status-ui | `sunset-web-wasm/src/members.rs` | 643 | 59 |
| voice-webcodecs | `sunset-relay/src/status.rs` | 566 | 61 |
| ui-presence | `sunset-web-wasm/src/client.rs` | 655 | 77 |

The `members.rs` diff is an API-contract decision (wasm-bindgen
`Option<u64>` → `f64` with a `-1` sentinel, because BigInt doesn't mix with
JS Number arithmetic) plus new test logic — 59 lines a reviewer must actually
absorb. Line count ranks it below the popover view; compression ranks it #1
in the merge, matching the merge's real center of gravity.

## Anomalies examined (both resolve in compression's favor)

- `sunset-core/src/liveness.rs`: 546 new lines, only 173 cx bytes. Cause:
  the tree at merge-base already contained the *complete design doc and
  plan* for this feature; the implementation follows the spec the reader
  already has, down to structure and doc comments. Defensible — but it means
  **whether docs/ belongs in the reference is a policy choice** the real
  tool should expose (docs in reference = "reviewer has read the spec").
- `web/e2e/helpers/viewport.js`: 35 lines, 419 cx bytes (12 B/line —
  roughly the density of prose). Verified genuinely dense: novel
  CSS-transform-probing test infrastructure with subtle fallback logic.
  High bytes/line as a density flag works.

Also observed: prose (plan/spec markdown) dominates raw scores when included
(7 KB for one plan doc vs 3.2 KB for the meatiest code file in the sample).
The real tool should report docs separately or let filtering handle it.

## Verdict

Proceed to Phase 1. The estimator does the two things the design bet on:
prices repo-conventional plumbing at near zero even across files and in
languages no CCN tool parses, and surfaces small dense diffs that line
count buries. Nothing observed contradicts the plan's metric definitions;
two findings feed forward: (1) make docs-in-reference an explicit choice,
(2) bytes/line density is already a useful secondary signal.
