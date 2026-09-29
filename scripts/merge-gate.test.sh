#!/usr/bin/env bash
# Tests for merge-gate.sh against a throwaway repository and a stubbed `gh`.
set -euo pipefail

gate=$(cd "$(dirname "$0")" && pwd)/merge-gate.sh
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
failures=0

# `gh api ... --jq` stands in as whatever FAKE_REVIEWS holds.
mkdir "$root/bin"
# shellcheck disable=SC2016 # expanded by the stub, not here
printf '#!/bin/sh\nprintf "%%s" "$FAKE_REVIEWS"\n' > "$root/bin/gh"
chmod +x "$root/bin/gh"
export PATH="$root/bin:$PATH" REPO=o/r PR_NUMBER=1 BASE_REF=main APPROVER=Jonas-Ross

git init -q -b main "$root/upstream"
cd "$root/upstream"
git config user.email test@example.com && git config user.name test
printf '%s\n' a b c d e f g h i j k l m n o p q r s t > code && echo x > other && git add -A && git commit -qm base
git clone -q "$root/upstream" "$root/pr"
cd "$root/pr"
git config user.email test@example.com && git config user.name test
git checkout -qb pr
sed -i.bak 's/^j$/J/' code && rm code.bak && git commit -qam change
approved=$(git rev-parse HEAD)

expect() {
  local want=$1 name=$2 out got
  out=$(HEAD_SHA=$(git rev-parse HEAD) "$gate" 2> /dev/null)
  got=$(sed -n 's/^state=//p; s/^auto=/ auto=/p' <<< "$out" | tr -d '\n')
  case $want:$got in
    auto:'success auto=true' | approved:'success auto=false' | pending:'pending auto=false' | failure:'failure auto=false')
      printf 'ok   %s\n' "$name"
      ;;
    *)
      printf 'FAIL %s: want %s, got %s\n' "$name" "$want" "$got"
      failures=$((failures + 1))
      ;;
  esac
}

export CHECK_RESULT=success TIER=auto VALIDATE_RESULT=skipped REVIEW_RESULT=success FAKE_REVIEWS=
expect auto 'auto tier, reviewed'
CHECK_RESULT=failure expect failure 'red CI'
REVIEW_RESULT=failure expect pending 'auto tier, review failed'
FAKE_REVIEWS="CHANGES_REQUESTED $approved" expect failure 'auto tier, Jonas requested changes'
FAKE_REVIEWS="$(printf 'CHANGES_REQUESTED %s\nAPPROVED %s' "$approved" "$approved")" expect auto 'latest review wins'

TIER=validate VALIDATE_RESULT=success expect auto 'validate tier, no regression'
TIER=validate VALIDATE_RESULT=failure expect pending 'validate tier, regressed'
TIER=validate VALIDATE_RESULT=failure FAKE_REVIEWS="APPROVED $approved" expect approved 'regression Jonas accepted'

export TIER=jonas REVIEW_RESULT=skipped
expect pending 'jonas tier, unapproved'
FAKE_REVIEWS="APPROVED $approved" expect approved 'jonas tier, approved at head'
FAKE_REVIEWS="DISMISSED $approved" expect pending 'approval dismissed'
FAKE_REVIEWS="APPROVED 0123456789abcdef0123456789abcdef01234567" expect pending 'approved commit gone'

# main moves on and is merged in: the approved change is the same change.
(cd "$root/upstream" && echo y > other && git commit -qam 'main moves')
git fetch -q origin && git merge -q --no-edit origin/main
FAKE_REVIEWS="APPROVED $approved" expect approved 'approval survives merging main'

# Line positions move when main changes above the change; the change does not.
(cd "$root/upstream" && sed -i.bak '1i\
top' code && rm code.bak && git commit -qam 'main edits above')
git fetch -q origin && git merge -q --no-edit origin/main
FAKE_REVIEWS="APPROVED $approved" expect approved 'approval survives main shifting its lines'

before=$(git rev-parse HEAD)
sed -i.bak 's/^J$/J /' code && rm code.bak && git commit -qam 'whitespace only'
FAKE_REVIEWS="APPROVED $approved" expect pending 'approval does not cover a whitespace edit'
git reset -q --hard "$before"

sed -i.bak 's/^s$/S/' code && rm code.bak && git commit -qam 'more change'
FAKE_REVIEWS="APPROVED $approved" expect pending 'approval does not cover a later edit'

[ "$failures" -eq 0 ] || { printf '%d failed\n' "$failures"; exit 1; }
