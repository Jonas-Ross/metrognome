#!/usr/bin/env bash
# Decide whether a pull request may merge, from the results of the CI jobs
# before it and Jonas's reviews. Run by the `merge-gate` job; DECISIONS.md 40.
#
# Reads TIER, CHECK_RESULT, VALIDATE_RESULT, REVIEW_RESULT, REPO, PR_NUMBER,
# BASE_REF, HEAD_SHA and APPROVER from the environment. Prints `auto=true` when
# the PR may merge without Jonas, `auto=false` otherwise, and exits non-zero
# when it may not merge at all.
set -euo pipefail

# The latest review from APPROVER that changes approval state, as
# `<STATE> <commit>`, or nothing.
latest_review() {
  gh api --paginate "repos/$REPO/pulls/$PR_NUMBER/reviews" --jq '
    .[] | select(.user.login == env.APPROVER)
        | select(.state == "APPROVED" or .state == "CHANGES_REQUESTED" or .state == "DISMISSED")
        | "\(.state) \(.commit_id)"' | tail -n 1
}

# The change a commit makes relative to where it meets the base branch. Stable
# across merging the base in, so an approval survives an update from main but
# not an edit to the change itself.
change_id() {
  git diff "$(git merge-base "origin/$BASE_REF" "$1")" "$1" | git patch-id --stable | cut -d' ' -f1
}

fail() {
  echo "auto=false"
  printf 'merge-gate: %s\n' "$1" >&2
  exit 1
}

[ "$CHECK_RESULT" = success ] || fail "CI is not green ($CHECK_RESULT)"

review=$(latest_review)
state=${review%% *}
approved_sha=${review#* }
[ "$state" != CHANGES_REQUESTED ] || fail "$APPROVER requested changes"

case $TIER:$VALIDATE_RESULT:$REVIEW_RESULT in
  auto:*:success | validate:success:success)
    printf 'merge-gate: tier %s passed without %s\n' "$TIER" "$APPROVER" >&2
    echo "auto=true"
    exit 0
    ;;
esac

why="tier $TIER, validate $VALIDATE_RESULT, Claude review $REVIEW_RESULT"
[ "$state" = APPROVED ] || fail "needs $APPROVER's approval ($why)"
git cat-file -e "$approved_sha^{commit}" 2> /dev/null ||
  fail "$APPROVER approved a commit no longer in the branch; needs a fresh approval ($why)"
[ "$(change_id "$approved_sha")" = "$(change_id "$HEAD_SHA")" ] ||
  fail "the change moved since $APPROVER approved it; needs a fresh approval ($why)"

printf 'merge-gate: %s approved this change (%s)\n' "$APPROVER" "$why" >&2
echo "auto=false"
