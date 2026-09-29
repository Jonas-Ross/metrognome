#!/usr/bin/env bash
# Decide whether a pull request may merge, from the results of the CI jobs
# before it and Jonas's reviews. Run by the `gate` job; DECISIONS.md 40.
#
# Reads TIER, CHECK_RESULT, VALIDATE_RESULT, REVIEW_RESULT, REPO, PR_NUMBER,
# BASE_REF, HEAD_SHA and APPROVER from the environment.
# Prints `state=` (success, pending or failure; pending sends nobody a
# failure email), `auto=` (whether it merges without Jonas) and `description=`,
# for the job to post as the `merge-gate` commit status.
set -euo pipefail

# The latest review from APPROVER that changes approval state, as
# `<STATE> <commit>`, or nothing.
latest_review() {
  gh api --paginate "repos/$REPO/pulls/$PR_NUMBER/reviews" --jq '
    .[] | select(.user.login == env.APPROVER)
        | select(.state == "APPROVED" or .state == "CHANGES_REQUESTED" or .state == "DISMISSED")
        | "\(.state) \(.commit_id)"' | tail -n 1
}

# The tree a commit produces merged into the base branch as it is now. Equal
# trees mean the head lands exactly what was approved, wherever main has moved;
# a conflict yields no tree, so it needs a fresh approval.
merged_tree() {
  local out
  out=$(git merge-tree --write-tree "origin/$BASE_REF" "$1" 2> /dev/null) || return 1
  echo "${out%%$'\n'*}"
}

# decide <state> <auto> <description>
decide() {
  printf 'state=%s\nauto=%s\ndescription=%s\n' "$1" "$2" "$3"
  printf 'merge-gate: %s: %s\n' "$1" "$3" >&2
  exit 0
}

[ "$CHECK_RESULT" = success ] || decide failure false "CI is not green ($CHECK_RESULT)"

review=$(latest_review)
state=${review%% *}
approved_sha=${review#* }
[ "$state" != CHANGES_REQUESTED ] || decide failure false "$APPROVER requested changes"

case $TIER:$VALIDATE_RESULT:$REVIEW_RESULT in
  auto:*:success | validate:success:success)
    decide success true "tier $TIER passed without $APPROVER"
    ;;
esac

why="tier $TIER, validate $VALIDATE_RESULT, Claude review $REVIEW_RESULT"
[ "$state" = APPROVED ] || decide pending false "needs $APPROVER's approval ($why)"
git cat-file -e "$approved_sha^{commit}" 2> /dev/null ||
  decide pending false "approved commit is gone; needs a fresh approval ($why)"
approved_tree=$(merged_tree "$approved_sha") || approved_tree=
head_tree=$(merged_tree "$HEAD_SHA") || head_tree=
[ -n "$approved_tree" ] && [ "$approved_tree" = "$head_tree" ] ||
  decide pending false "change moved since approval; needs a fresh one ($why)"

decide success false "$APPROVER approved this change ($why)"
