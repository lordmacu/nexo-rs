#!/usr/bin/env bash
# Phase 31.5.c — audit a Composer vendor dir for native PHP
# extensions that would invalidate the `noarch` claim. Returns
# non-zero with a clear hint if any are present.
#
# Usage:
#   bash scripts/verify-pure-php.sh [vendor_dir]
# Default vendor_dir is `lib/vendor`.

set -euo pipefail

VENDOR_DIR="${1:-lib/vendor}"
if [[ ! -d "$VENDOR_DIR" ]]; then
  echo "::error::vendor dir $VENDOR_DIR does not exist" >&2
  exit 1
fi

# Native PHP extension suffixes (.so on Linux, .dylib on macOS,
# .dll on Windows — all forbidden in noarch tarballs).
NATIVE_HITS="$(find "$VENDOR_DIR" -type f \
  \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' \) | head -20)"

if [[ -n "$NATIVE_HITS" ]]; then
  echo "::error::native extension files found in $VENDOR_DIR — cannot publish as noarch:" >&2
  echo "$NATIVE_HITS" >&2
  echo "Hint: PHP extensions ship via php.ini, not Composer. If a Composer dep" >&2
  echo "smuggled in a native build artifact, swap to a pure-PHP alternative" >&2
  echo "or publish per-target tarballs (Phase 31.5.c.b — not yet shipped)." >&2
  exit 1
fi

echo "::notice::$VENDOR_DIR is pure-PHP (no native ext files found)"
