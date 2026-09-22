#!/usr/bin/env bash
# A SQLite database reached selecta's `main` once under the name `x`, which no
# extension-based ignore rule could have caught, so this matches on content.
set -euo pipefail

max_bytes=${MAX_BINARY_BYTES:-65536}
status=0

while IFS= read -r -d '' file; do
  [ -f "$file" ] || continue

  if [ "$(head -c 15 -- "$file")" = 'SQLite format 3' ]; then
    printf 'error: %s is a SQLite database\n' "$file" >&2
    status=1
    continue
  fi

  # Only large binaries fail, so a small fixture stays possible without an
  # allowlist to keep in sync.
  LC_ALL=C grep -qI . -- "$file" 2>/dev/null && continue

  size=$(wc -c < "$file")

  if [ "$size" -gt "$max_bytes" ]; then
    printf 'error: %s is %s bytes of binary data (limit %s)\n' "$file" "$size" "$max_bytes" >&2
    status=1
  fi
done < <(git ls-files -z)

exit $status
