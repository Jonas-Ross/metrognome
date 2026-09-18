# Vendored skill: gh-stack

Copied verbatim from [github/gh-stack](https://github.com/github/gh-stack) at commit
`2bd699a544a09cb5c45a013d03416e0894b0454e`, path `skills/gh-stack/`. Upstream is MIT licensed (Copyright GitHub, Inc.).
To refresh, re-copy that path at a newer commit and update this line — do not hand-edit
`SKILL.md` or `references/`, or the next refresh silently drops the edit.

## Local conventions that override the skill

The skill documents `gh stack` in general. Where it differs from this project:

- **Submit with `--open`, never bare `--auto`.** `--auto` opens drafts; PRs here are
  always opened ready for review so Codex runs.
- **Merge with `--squash`, and only after Jonas has approved every PR being merged.**
  `gh stack merge <pr> --yes` merges that PR *and every unmerged PR below it*,
  all-or-nothing, reusing the last-used merge method when no flag is given.
- Branch names are passed through verbatim, so `gh stack add fix/foo` keeps the
  conventional naming this repo already requires.

## Sandbox limitation

`gh` is not installed in Claude Code web sessions, and that sandbox's GitHub proxy
rejects GraphQL, which `gh stack` needs to find, create, and update PRs. Local stack
commands (`init`, `add`, `view`, `rebase`, `checkout`) work once `gh` is present;
`submit`, `sync`, and `merge` do not. Stacks are driven from a local checkout —
see `.claude/hooks/README.md`… (setup below).
