#!/usr/bin/env bash
# A SQLite database reached selecta's `main` once under the name `x`, which no
# extension-based ignore rule could have caught, so this matches on content.
set -euo pipefail

command -v perl > /dev/null || { printf 'error: perl is required\n' >&2; exit 2; }

export MAX_BINARY_BYTES=${MAX_BINARY_BYTES:-65536}

# One perl process for every file: a fork per file took over 5s on macOS.
# `grep -I` would call any NUL-free blob text, so text here means valid UTF-8
# carrying no control characters beyond the four a document uses. Perl also
# keeps the answer identical on Linux and macOS, and can compare the NUL that
# ends SQLite's magic header, which a shell comparison drops.
git ls-files -z | perl -MEncode -0 -ne '
  BEGIN { $status = 0; $max = $ENV{MAX_BINARY_BYTES} }
  chomp(my $file = $_);
  next unless -f $file;

  open(my $fh, q{<:raw}, $file) or die "error: cannot read $file: $!\n";
  my $body = do { local $/; <$fh> } // q{};
  close $fh;

  if ($body =~ /\ASQLite format 3\0/) {
    print STDERR "error: $file is a SQLite database\n";
    $status = 1;
    next;
  }

  my $text = eval { Encode::decode(q{UTF-8}, $body, Encode::FB_CROAK | Encode::LEAVE_SRC) };
  next if defined $text && $text !~ /(?![\t\n\f\r])\p{Cc}/;

  # Only large binaries fail, so a small fixture stays possible without an
  # allowlist to keep in sync.
  my $size = length $body;
  if ($size > $max) {
    print STDERR "error: $file is $size bytes of binary data (limit $max)\n";
    $status = 1;
  }

  END { $? = $status }
'
