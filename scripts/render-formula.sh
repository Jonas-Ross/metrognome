#!/usr/bin/env bash
# Usage: scripts/render-formula.sh <version> <sha256>
# Prints the Homebrew formula for a published release to stdout.
set -euo pipefail

version=${1:?usage: render-formula.sh <version> <sha256>}
sha=${2:?usage: render-formula.sh <version> <sha256>}
[[ $sha =~ ^[0-9a-f]{64}$ ]] || { printf 'error: not a sha256: %s\n' "$sha" >&2; exit 2; }

sed -e "s/@VERSION@/${version}/g" -e "s/@SHA256@/${sha}/g" \
  "$(dirname "$0")/../packaging/metrognome.rb.in"
