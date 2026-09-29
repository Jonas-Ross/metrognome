#!/usr/bin/env bash
# Compare two `metrognome validate` JSON reports, base first, row by row.
#
#   scripts/validate-compare.sh <base.json> <head.json>
#
# Exits non-zero when head loses anything base had: a tempo within tolerance, a
# tempo a consumer keeps, or a key that agreed. A row that errored on either
# side makes the comparison inconclusive, which also fails: a lookup that did
# not happen is not a pass.
set -euo pipefail

[ $# -eq 2 ] || { printf 'usage: %s <base.json> <head.json>\n' "$0" >&2; exit 2; }

jq -rn --slurpfile base "$1" --slurpfile head "$2" '
  def by_label: map({key: .label, value: .}) | from_entries;
  def kept: .verdict == "ok" and .tempo_uncertain != true;
  ($base[0].rows | by_label) as $b
  | ($head[0].rows | by_label) as $h
  | [ $b | keys[] as $label | $b[$label] as $was | $h[$label] as $now
      | if $now == null or $was.error != null or $now.error != null then
          "inconclusive \($label): \($was.error // $now.error // "missing from head")"
        elif $was.verdict == "ok" and $now.verdict != "ok" then
          "regressed    \($label): tempo \($was.estimated_bpm) -> \($now.estimated_bpm) (\($now.verdict))"
        elif ($was | kept) and ($now | kept | not) then
          "regressed    \($label): tempo now flagged uncertain (conf \($was.tempo_confidence) -> \($now.tempo_confidence))"
        elif $was.key_ok == true and $now.key_ok != true then
          "regressed    \($label): key \($was.estimated_key) -> \($now.estimated_key)"
        else empty end
    ] as $problems
  | "tempo ok: \($base[0].tempo_ok) -> \($head[0].tempo_ok)",
    "tempo discarded: \($base[0].tempo_discarded) -> \($head[0].tempo_discarded)",
    "key agreed: \($base[0].key_agreed) of \($base[0].key_checked) -> \($head[0].key_agreed) of \($head[0].key_checked)",
    ($problems[]),
    (if ($problems | length) > 0 then error("validate regressed or was inconclusive") else "no regression" end)
'
