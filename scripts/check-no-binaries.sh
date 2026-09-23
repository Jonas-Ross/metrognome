#!/usr/bin/env bash
# A SQLite database reached selecta's `main` once under the name `x`, which no
# extension-based ignore rule could have caught, so this matches on content.
set -euo pipefail

max_bytes=${MAX_BINARY_BYTES:-65536}
status=0

# `grep -I` only calls a file binary when it holds a NUL byte, so a large blob
# of nonzero bytes would read as text. Valid UTF-8 is the test instead, which
# still accepts a document that is mostly non-ASCII.
is_text() {
  LC_ALL=C tr -d '\000' < "$1" | cmp -s - "$1" &&
    iconv -f UTF-8 -t UTF-8 < "$1" > /dev/null 2>&1
}

while IFS= read -r -d '' file; do
  [ -f "$file" ] || continue

  if [ "$(head -c 15 -- "$file")" = 'SQLite format 3' ]; then
    printf 'error: %s is a SQLite database\n' "$file" >&2
    status=1
    continue
  fi

  # Only large binaries fail, so a small fixture stays possible without an
  # allowlist to keep in sync.
  is_text "$file" && continue

  size=$(wc -c < "$file")

  if [ "$size" -gt "$max_bytes" ]; then
    printf 'error: %s is %s bytes of binary data (limit %s)\n' "$file" "$size" "$max_bytes" >&2
    status=1
  fi
done < <(git ls-files -z)

exit $status
