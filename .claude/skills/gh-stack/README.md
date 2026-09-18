# Vendored skill: gh-stack

`SKILL.md` and `references/` are copied verbatim from
[github/gh-stack](https://github.com/github/gh-stack) at commit `2bd699a544a09cb5c45a013d03416e0894b0454e`
(path `skills/gh-stack/`, MIT licensed, Copyright GitHub, Inc.). To refresh, re-copy that
path at a newer commit and update this line. Don't hand-edit those files — local deviations
belong here, or the next refresh drops them silently.

## Where it works

`gh stack` needs the `gh` CLI plus the extension, installed once per machine:

```bash
gh extension install github/gh-stack
git config rerere.enabled true
```

**Claude Code web sessions cannot run it.** `gh` is absent from that sandbox, the extension
install is blocked (`github/gh-stack` is outside the session's repo scope), and its proxy
rejects GitHub's GraphQL API, which `gh stack` uses to find, create, and link PRs. A
from-source build gets the local commands working — `init`, `add`, `view`, `rebase`,
`checkout` — but `submit` still pushes without creating or linking anything. That is why
no session hook installs it.

Web sessions stack by hand instead: branch off the unmerged PR's head rather than `main`,
and open the new PR with its base set to that branch. Same shape, no stack metadata on
GitHub. Rebase every layer above after changing a lower one; copying a fix across branches
loses the ancestry.

## Local conventions that override the skill

- **Submit with `--open`, never bare `--auto`.** `--auto` alone opens drafts; PRs here
  are always ready for review so Codex runs.
- **Merge with `--squash`, and only once Jonas has approved every PR in the set.**
  `gh stack merge <pr> --yes` takes that PR *and every unmerged PR below it*,
  all-or-nothing, reusing the last-used merge method when no flag is passed.
- Branch names pass through verbatim, so `gh stack add fix/foo` keeps the conventional
  naming both repos already require.
