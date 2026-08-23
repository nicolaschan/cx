---
name: cx-triage
description: Triage a branch's changes by information content before review. Runs cx score --json and routes each file - dense novel logic to human attention, high-volume/low-information additions to consolidation, the rest to skim. Use when asked to "triage this PR", "what needs review here", "route this diff", or before a full code review of a large branch.
---

# cx-triage

Route review attention by *marginal description length*: what `cx score`
says each file newly adds, conditioned on the codebase. You are the routing
layer; cx provides the measurements.

## Run

```sh
cx score --json          # add --base <ref> if the target branch isn't main/master
```

If `cx` is not on PATH: `nix run github:nicolaschan/cx -- score --json`.

Fields you route on, per file: `review_bytes` (what a reviewer must newly
absorb), `delta_bytes` (complexity added/refunded), `new_lines`,
`bytes_per_line` (density; added files only), `density_outlier`, `status`.
Trust gates: if the report's worst `scales.*` value is outside 0.7–1.1,
per-file attribution is noisy — triage on totals and file rank only, and
say so.

## Route

Work down the `review_bytes` ranking. Caps are hard: at most 8 files
routed to human attention, at most 5 consolidation candidates examined per
run; everything else is skim. Files below 256 review bytes are skim unless
`delta_bytes` is strongly negative (call out large refunds — deleted
unique complexity is worth a human glance).

**→ Human attention** — `review_bytes` high (top of ranking, roughly
≥ 1 KB) or high `bytes_per_line` (≥ ~20 B/line): dense novel logic.
Summarize per file: what the change appears to do, why it scored dense,
and the one or two regions a reviewer should read line-by-line. Do not
attempt to "fix" these; density is not a defect.

**→ Consolidation check** — added files with many lines but low
information (`new_lines` ≥ ~80 and `bytes_per_line` ≤ ~5, or review ≈ 0
on a large addition): the content is nearly derivable from what the repo
already contains. Read the file and the existing code it echoes, then
judge which kind of repetition it is:

- *Incidental*: the sameness is an accident of copy-paste — the shared
  shape could live in one helper/abstraction without distorting any
  caller. Route to `/simplify` (or propose the consolidation directly),
  scoped to exactly these files.
- *Interface-imposed*: the repetition is the cost of a framework,
  protocol, or convention (trait impls, view boilerplate, FFI shims) and
  consolidating would add indirection without removing the pattern.
  Leave it; note it as conventional in the triage summary.

A v1 caveat, from the plan: cx reports *that* a file is low-information,
not *which* existing code it matches. If you can't find the echoed code
quickly, say "conventional per cx, provenance unverified" instead of
guessing.

**→ Skim** — everything else. List them in one line each; no analysis.

## After any fix

Only act on consolidation candidates when the user asked for fixes, and
then: tests must pass before and after; re-run `cx score --json` and show
the delta (a real consolidation lowers Δcomplexity); if `lizard` is
available, a cognitive-complexity delta on the touched functions is a
useful second opinion. Never modify files routed to human attention.

## Output

One triage table (path, route, one-line reason), then the human-attention
summaries, then consolidation findings. Lead with the PR totals and the
attribution-scale verdict. Skipped files (`skipped[]` in the JSON) are
reported as-is — cx already excluded them for a stated reason.

## Known limits

Information ≠ verification effort: twenty one-character `<`→`<=` flips
score tiny but need careful eyes — density flags don't replace reading
the diff summary. Scores compare within this repo and compressor version
only; never quote them across repos.
