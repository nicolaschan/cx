# Path scoping for large repos

## Problem

`cx` has no way to look at part of a repository. Every view scores the
whole tree, which is both wrong (you cannot ask "what does *this*
subsystem cost?") and slow (every blob in the tree is fetched and
compressed, even the ones the filter was always going to drop).

## Surface

One repeatable flag on `CommonArgs`, so every view takes it:

```
-g, --glob <GLOB>    [env: CX_GLOB]
```

Gitignore glob syntax, exactly as `.cxignore` already uses: a bare glob
includes, a `!` prefix excludes, and among globs the last match wins.
This is ripgrep's `-g`, and it is ripgrep's own implementation —
`ignore::overrides::OverrideBuilder`, from a crate cx already depends on.

```console
cx -g 'crates/cx-cli/**'
cx -g '!**/generated/**'
cx abs -g 'src/**' -g '!src/legacy/**'
```

## Semantics

A path outside the globs **leaves the universe entirely**: it is in no
reference, no scoring pass, and not in the skipped list either. This is
how `.cxignore`, tests and prose already behave, and it is what makes
scoping useful — `cx -g 'crates/foo/**'` scores that subtree as if it
were the whole codebase.

Out-of-scope files are *absent*, not *skipped*. The skipped list names
files cx looked at and declined to score; a file the user never asked
about does not belong there, and on a large repo it would bury the list.

A directory decides for its whole subtree, deepest first — so `!target`
prunes everything under it without the user writing `target/**`. That is
the rule a directory traversal produces, and the rule gitignore already
states.

## Shape

Two concerns, kept apart:

- **`scope.rs` (new)** — *which paths cx looks at.* `Scope::allows(path)`.
  Applied to path lists before anything else, so an out-of-scope blob is
  never fetched.
- **`filter.rs` (existing)** — *which of those files are scorable code.*
  Reports a reason per dropped file.

`filter.rs` is untouched. Splitting its stack into path-decidable and
content-decidable halves, so the pipeline could drop `vendor/**` without
fetching it, was considered and rejected: the filter already runs before
the scoring pass, so those blobs are fetched but never compressed, and
compression is what the run's time goes to. That refactor buys I/O alone,
against no measurement, and it would move reason strings around
(`generated/vendored pattern` displacing `binary`) for no behavior change.
Not in this change.

## Call sites

`Scope` is applied in three places in `pipeline.rs`:

- `abs`: the path list from `git.list(side)`
- `diff`: the merge-base tree (`git.ls_tree`), which is the reference
- `diff`: the changed files (`git.changes`)

## Testing

Unit tests in `scope.rs` for the match rule: no globs admits everything;
an include glob admits only matches; `!` excludes; a bare directory name
covers its subtree; a deeper glob beats a shallower one; `.gitignore`
anchoring (a glob containing `/` is rooted, one without matches at any
depth).

End-to-end tests that a scoped run scores only in-scope files, that
out-of-scope files appear in neither `files` nor `skipped`, and that
`--glob` on a subtree yields the same numbers as running `cx` in a repo
containing only that subtree — the claim that scoping means "as if this
were the codebase".
