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

# Every changed line of a file is the named constant, so a DSP change can bump
# its cache version without the version's home dragging it to `jonas`.
only_const_changed() {
  git diff -U0 "$base" "$head" -- "$2" |
    grep -E '^[+-]' | grep -vE '^(\+\+\+|---) ' |
    grep -qvE "^[+-]pub const $1: [A-Za-z0-9_]+ = [0-9]+;\$" && return 1
  return 0
}

classify() {
  case $1 in
    # The gate itself and the rules agents follow: a change cannot grade its own
    # grader.
    .github/* | scripts/* | CLAUDE.md | AGENTS.md | LICENSE) echo jonas ;;
    # The yardsticks the validate tier is measured against.
    crates/metrognome/src/validate.rs | crates/metrognome/src/testsig.rs) echo jonas ;;
    # A manifest can add a dependency, which needs a stated reason.
    Cargo.toml | */Cargo.toml) echo jonas ;;
    *.md | crates/*/tests/* | Cargo.lock | rust-toolchain.toml) echo auto ;;
    crates/metrognome/src/dsp.rs | crates/metrognome/src/tempo.rs | \
      crates/metrognome/src/key.rs | crates/metrognome/src/pipeline.rs | \
      crates/metrognome/src/decode.rs) echo validate ;;
    crates/metrognome/src/lib.rs)
      if only_const_changed ALGORITHM_VERSION "$1"; then echo validate; else echo jonas; fi
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
