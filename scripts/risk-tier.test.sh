#!/usr/bin/env bash
# Tests for risk-tier.sh against throwaway repositories. No network.
set -euo pipefail

tier_script=$(cd "$(dirname "$0")" && pwd)/risk-tier.sh
failures=0

# Each case starts from a base commit carrying these files.
fresh_repo() {
  dir=$(mktemp -d)
  cd "$dir"
  git init -q -b main
  git config user.email test@example.com
  git config user.name test
  mkdir -p crates/metrognome/src crates/metrognome/tests .github/workflows scripts
  printf 'pub const ALGORITHM_VERSION: u32 = 10;\npub fn f() {}\n' > crates/metrognome/src/lib.rs
  for f in dsp tempo validate types; do echo "// $f" > "crates/metrognome/src/$f.rs"; done
  echo '# x' > README.md
  echo 'on: push' > .github/workflows/ci.yml
  git add -A
  git commit -qm base
  git checkout -qb pr
}

expect() {
  local want=$1 name=$2 got
  git add -A
  git commit -qm change --allow-empty
  got=$("$tier_script" main pr 2>/dev/null)
  if [ "$got" = "$want" ]; then
    printf 'ok   %s\n' "$name"
  else
    printf 'FAIL %s: want %s, got %s\n' "$name" "$want" "$got"
    failures=$((failures + 1))
  fi
  cd /
  rm -rf "$dir"
}

fresh_repo; echo more >> README.md; expect auto 'docs only'
fresh_repo; echo 't' > crates/metrognome/tests/new.rs; echo lock > Cargo.lock; expect auto 'tests and lockfile'
fresh_repo; echo x >> crates/metrognome/src/dsp.rs; expect validate 'dsp change'
fresh_repo
echo x >> crates/metrognome/src/tempo.rs
sed -i.bak 's/= 10;/= 11;/' crates/metrognome/src/lib.rs && rm crates/metrognome/src/lib.rs.bak
expect validate 'dsp change bumping ALGORITHM_VERSION'
fresh_repo; echo 'pub fn g() {}' >> crates/metrognome/src/lib.rs; expect jonas 'other lib.rs change'
fresh_repo; echo x >> crates/metrognome/src/validate.rs; expect jonas 'the yardstick'
fresh_repo; echo x >> crates/metrognome/src/dsp.rs; echo x >> .github/workflows/ci.yml; expect jonas 'dsp plus workflow'
fresh_repo; echo x >> README.md; echo x >> crates/metrognome/src/types.rs; expect jonas 'docs plus contract'
fresh_repo; echo x > CLAUDE.md; expect jonas 'agent rules'
fresh_repo; echo x > crates/metrognome/src/new_module.rs; expect jonas 'unlisted file'
fresh_repo; git rm -q crates/metrognome/src/dsp.rs; expect validate 'deleted dsp file'
fresh_repo; git mv README.md crates/metrognome/src/sneaky.rs; expect jonas 'rename out of an auto path'
fresh_repo; expect jonas 'empty diff'

# The tier is measured from the merge base, so main moving on after the branch
# point does not leak main's own changes into the PR's tier.
fresh_repo
echo x >> README.md
git add -A && git commit -qm docs
git checkout -q main && echo x >> .github/workflows/ci.yml && git commit -qam ci && git checkout -q pr
expect auto 'main moved after branching'

[ "$failures" -eq 0 ] || { printf '%d failed\n' "$failures"; exit 1; }
