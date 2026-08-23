#!/usr/bin/env bash
# Phase 0 validation gate: crude metric 1 (review cost) via `zstd --patch-from`,
# compared against a dumb baseline (changed line count), on historical merges
# of a real repo. See docs/phase0-validation.md for results and the go/no-go.
#
# Usage: phase0.sh <repo> <merge-commit>...
# Output: TSV  merge<TAB>path<TAB>cx_bytes<TAB>lines_changed<TAB>new_size
set -euo pipefail

repo=$1
shift

zstd_flags=(--single-thread -19 --long=27 -c)
code_re='\.(rs|toml|ts|tsx|js|jsx|css|html|nix|md|gleam|sh|py|yaml|yml)$'
skip_re='(^|/)(Cargo\.lock|flake\.lock|package-lock\.json|yarn\.lock)$|\.min\.'

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

g() { git -C "$repo" "$@"; }

for merge in "$@"; do
  mb=$(g merge-base "$merge^1" "$merge^2")

  # Reference: every code file in the old tree (crude: no exclusion of the
  # changed files' old versions — metric 1 conditions on them by design).
  ref="$tmp/ref"
  : >"$ref"
  while IFS= read -r path; do
    g show "$mb:$path" >>"$ref" 2>/dev/null || continue
    printf '\0CX-SEP\0' >>"$ref"
  done < <(g ls-tree -r --name-only "$mb" | grep -E "$code_re" | grep -Ev "$skip_re" | sort)

  : >"$tmp/empty"
  empty=$(zstd "${zstd_flags[@]}" --patch-from="$ref" "$tmp/empty" 2>/dev/null | wc -c)

  # Baseline: added+deleted lines per changed file.
  declare -A lines=()
  while IFS=$'\t' read -r add del path; do
    [[ $add == - ]] && continue
    lines[$path]=$((add + del))
  done < <(g diff --numstat "$mb" "$merge")

  # Score each added/modified code file sequentially, growing the reference
  # so repeated new patterns within one merge are charged once.
  while IFS=$'\t' read -r status path; do
    [[ $status =~ ^[AM] ]] || continue
    grep -qE "$code_re" <<<"$path" || continue
    grep -qEv "$skip_re" <<<"$path" || continue
    new="$tmp/new"
    g show "$merge:$path" >"$new" 2>/dev/null || continue
    size=$(zstd "${zstd_flags[@]}" --patch-from="$ref" "$new" 2>/dev/null | wc -c)
    cx=$((size > empty ? size - empty : 0))
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "$merge" "$path" "$cx" "${lines[$path]:-0}" "$(wc -c <"$new")"
    cat "$new" >>"$ref"
    printf '\0CX-SEP\0' >>"$ref"
  done < <(g diff --name-status "$mb" "$merge")
done
