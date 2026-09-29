#!/usr/bin/env bash
# Tests for validate-compare.sh on hand-written reports. No network.
set -euo pipefail

compare=$(cd "$(dirname "$0")" && pwd)/validate-compare.sh
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
failures=0

# report <file> <row>...: each row is a JSON object with at least a label.
report() {
  local out=$1
  shift
  printf '%s\n' "$@" | jq -s '{rows: ., tempo_ok: 0, tempo_discarded: 0, key_agreed: 0, key_checked: 0}' > "$out"
}

expect() {
  local want=$1 name=$2 status=0
  "$compare" "$dir/base.json" "$dir/head.json" > /dev/null 2>&1 || status=$?
  if { [ "$want" = pass ] && [ "$status" -eq 0 ]; } || { [ "$want" = fail ] && [ "$status" -ne 0 ]; }; then
    printf 'ok   %s\n' "$name"
  else
    printf 'FAIL %s: want %s, exit %d\n' "$name" "$want" "$status"
    failures=$((failures + 1))
  fi
}

ok='{"label":"a","verdict":"ok","estimated_bpm":120,"tempo_uncertain":false}'
key_ok='{"label":"k","verdict":"ok","estimated_bpm":127,"estimated_key":"D major","key_ok":true}'

report "$dir/base.json" "$ok" "$key_ok"
report "$dir/head.json" "$ok" "$key_ok"
expect pass 'identical'

report "$dir/head.json" '{"label":"a","verdict":"octave_error","estimated_bpm":60}' "$key_ok"
expect fail 'tempo lost'

report "$dir/head.json" '{"label":"a","verdict":"ok","estimated_bpm":120,"tempo_uncertain":true}' "$key_ok"
expect fail 'tempo kept before, discarded now'

report "$dir/head.json" "$ok" '{"label":"k","verdict":"ok","estimated_key":"E minor","key_ok":false}'
expect fail 'key lost'

report "$dir/head.json" "$ok" '{"label":"k","verdict":"ok","error":"network: timed out"}'
expect fail 'errored row is inconclusive'

report "$dir/head.json" "$ok"
expect fail 'row missing from head'

report "$dir/base.json" '{"label":"a","verdict":"wrong","estimated_bpm":90}' '{"label":"k","verdict":"ok","key_ok":false}'
report "$dir/head.json" "$ok" "$key_ok"
expect pass 'improvement'

[ "$failures" -eq 0 ] || { printf '%d failed\n' "$failures"; exit 1; }
