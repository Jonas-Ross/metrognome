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

# The change a commit makes relative to where it meets the base branch, byte
# for byte except line positions and blob ids, which merging the base in moves.
# `git patch-id` would also drop whitespace, which a shell string can depend on.
change_id() {
  git diff --binary --full-index "$(git merge-base "origin/$BASE_REF" "$1")" "$1" |
    sed -E -e '/^index [0-9a-f]+\.\.[0-9a-f]+/d' -e 's/^@@ -[0-9,]+ \+[0-9,]+ @@/@@/' |
    sha256sum | cut -d' ' -f1
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
[ "$(change_id "$approved_sha")" = "$(change_id "$HEAD_SHA")" ] ||
  decide pending false "change moved since approval; needs a fresh one ($why)"

decide success false "$APPROVER approved this change ($why)"
