#!/usr/bin/env bash
# Phase 31.4 — audit `lib/` (or any vendor dir) for native
# extension files that would invalidate the `noarch` claim.
# Returns non-zero with a clear hint if any are present.
#
# Usage:
#   bash scripts/verify-pure-python.sh [vendor_dir]
# Default vendor_dir is `lib/`.

set -euo pipefail

VENDOR_DIR="${1:-lib}"
if [[ ! -d "$VENDOR_DIR" ]]; then
  echo "::error::vendor dir $VENDOR_DIR does not exist" >&2
  exit 1
fi

# Look for native ext suffixes the noarch convention forbids.
NATIVE_HITS="$(find "$VENDOR_DIR" -type f \
  \( -name '*.so' -o -name '*.pyd' -o -name '*.dylib' \) | head -20)"

if [[ -n "$NATIVE_HITS" ]]; then
  echo "::error::native extension files found in $VENDOR_DIR — cannot publish as noarch:" >&2
  echo "$NATIVE_HITS" >&2
  echo "Hint: pin pure-Python deps only, or publish per-target tarballs (Phase 31.4.b)." >&2
  exit 1
fi

echo "::notice::$VENDOR_DIR is pure-Python (no native ext files found)"
