#!/usr/bin/env bash
# A SQLite database reached selecta's `main` once under the name `x`, which no
# extension-based ignore rule could have caught, so this matches on content.
set -euo pipefail

command -v perl > /dev/null || { printf 'error: perl is required\n' >&2; exit 2; }

max_bytes=${MAX_BINARY_BYTES:-65536}
status=0

# 0 text, 1 binary, 2 SQLite database. `grep -I` would call any NUL-free blob
# text, so text here means valid UTF-8 carrying no control characters beyond
# the four a document uses. Perl also keeps the answer identical on Linux and
# macOS, and can compare the NUL that ends SQLite's magic header, which a shell
# comparison drops.
classify() {
  perl -0777 -MEncode -ne '
    exit 2 if /\ASQLite format 3\0/;

    my $text = eval { Encode::decode(q{UTF-8}, $_, Encode::FB_CROAK) };

    exit 1 unless defined $text;
    exit($text =~ /(?![\t\n\f\r])\p{Cc}/ ? 1 : 0);
  ' -- "$1"
}

while IFS= read -r -d '' file; do
  [ -f "$file" ] || continue

  verdict=0
  classify "$file" || verdict=$?

  if [ "$verdict" -eq 2 ]; then
    printf 'error: %s is a SQLite database\n' "$file" >&2
    status=1
    continue
  fi

  # Only large binaries fail, so a small fixture stays possible without an
  # allowlist to keep in sync.
  if [ "$verdict" -eq 0 ]; then
    continue
  fi

  size=$(wc -c < "$file")

  if [ "$size" -gt "$max_bytes" ]; then
    printf 'error: %s is %s bytes of binary data (limit %s)\n' "$file" "$size" "$max_bytes" >&2
    status=1
  fi
done < <(git ls-files -z)

exit $status
