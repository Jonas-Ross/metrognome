#!/usr/bin/env bash
# Classify a change by the files it touches, for the merge gate.
#
#   scripts/risk-tier.sh <base> <head>
#
# Prints the tier on stdout: `auto` (merges once CI and the Claude review pass),
# `validate` (also needs `metrognome validate` not to regress), or `jonas`.
# One `<class> <path>` line per file goes to stderr. Anything unlisted is
# `jonas`, so a new kind of file fails closed. DECISIONS.md 40 has the tiers.
set -euo pipefail

[ $# -eq 2 ] || { printf 'usage: %s <base> <head>\n' "$0" >&2; exit 2; }
base=$(git merge-base "$1" "$2")
head=$2

# The file's only change raises ALGORITHM_VERSION, so a DSP change can bump its
# cache version without the version's home dragging it to `jonas`. Read in full,
# not with `grep -q`, whose early exit SIGPIPEs the diff under pipefail.
algorithm_bump_only() {
  local lines old new
  lines=$(git diff -U0 "$base" "$head" -- "$1" | grep -E '^[+-]' | grep -vE '^(\+\+\+|---) ' || true)
  old=$(sed -nE 's/^-pub const ALGORITHM_VERSION: u32 = ([0-9]+);$/\1/p' <<< "$lines")
  new=$(sed -nE 's/^\+pub const ALGORITHM_VERSION: u32 = ([0-9]+);$/\1/p' <<< "$lines")
  [ "$(wc -l <<< "$lines")" -eq 2 ] && [ -n "$old" ] && [ -n "$new" ] && [ "$new" -gt "$old" ]
}

classify() {
  case $1 in
    # The gate itself and the rules agents follow: a change cannot grade its own
    # grader.
    .github/* | scripts/* | LICENSE) echo jonas ;;
    # Agent instructions nest, so they are the gate's inputs at any depth.
    CLAUDE*.md | */CLAUDE*.md | AGENTS*.md | */AGENTS*.md | REVIEW.md | */REVIEW.md | \
      .claude/* | */.claude/* | .codex/* | */.codex/*) echo jonas ;;
    # The yardsticks the validate tier is measured against.
    crates/metrognome/src/validate.rs | crates/metrognome/src/testsig.rs) echo jonas ;;
    # A manifest can add a dependency, which needs a stated reason.
    Cargo.toml | */Cargo.toml) echo jonas ;;
    *.md | crates/*/tests/* | Cargo.lock | rust-toolchain.toml) echo auto ;;
    crates/metrognome/src/dsp.rs | crates/metrognome/src/tempo.rs | \
      crates/metrognome/src/key.rs | crates/metrognome/src/pipeline.rs | \
      crates/metrognome/src/decode.rs) echo validate ;;
    crates/metrognome/src/lib.rs)
      if algorithm_bump_only "$1"; then echo validate; else echo jonas; fi
      ;;
    *) echo jonas ;;
  esac
}

tier=
while IFS= read -r -d '' path; do
  class=$(classify "$path")
  printf '%-8s %s\n' "$class" "$path" >&2
  case $class:$tier in
    jonas:* | validate:auto | validate: | auto:) tier=$class ;;
  esac
done < <(git diff -z --name-only --no-renames "$base" "$head")

# An empty diff has nothing to vouch for it.
echo "${tier:-jonas}"
