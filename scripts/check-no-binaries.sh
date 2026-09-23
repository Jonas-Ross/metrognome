#!/usr/bin/env bash
# A SQLite database reached selecta's `main` once under the name `x`, which no
# extension-based ignore rule could have caught, so this matches on content.
set -euo pipefail

command -v perl > /dev/null || { printf 'error: perl is required\n' >&2; exit 2; }

max_bytes=${MAX_BINARY_BYTES:-65536}
status=0

# `grep -I` only calls a file binary when it holds a NUL byte, so a large blob
# of nonzero bytes would read as text. Text here is valid UTF-8 carrying no
# control characters beyond the four a document actually uses. Perl keeps the
# answer identical on Linux and macOS, where `tr` and `iconv` differ.
is_text() {
  perl -0777 -MEncode -ne '
    my $text = eval { Encode::decode(q{UTF-8}, $_, Encode::FB_CROAK) };

    exit 1 unless defined $text;
    exit($text =~ /(?![\t\n\f\r])\p{Cc}/ ? 1 : 0);
  ' -- "$1"
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
