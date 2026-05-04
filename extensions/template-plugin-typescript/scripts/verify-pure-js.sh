#!/usr/bin/env bash
# Phase 31.5 — audit a vendor dir for native node addons that
# would invalidate the `noarch` claim. Returns non-zero with a
# clear hint if any are present.
#
# Usage:
#   bash scripts/verify-pure-js.sh [vendor_dir]
# Default vendor_dir is `lib/node_modules`.

set -euo pipefail

VENDOR_DIR="${1:-lib/node_modules}"
if [[ ! -d "$VENDOR_DIR" ]]; then
  echo "::error::vendor dir $VENDOR_DIR does not exist" >&2
  exit 1
fi

# Native addon suffixes the noarch convention forbids.
NATIVE_HITS="$(find "$VENDOR_DIR" -type f \
  \( -name '*.node' -o -name '*.so' -o -name '*.dylib' -o -name '*.dll' \) | head -20)"

if [[ -n "$NATIVE_HITS" ]]; then
  echo "::error::native addon files found in $VENDOR_DIR — cannot publish as noarch:" >&2
  echo "$NATIVE_HITS" >&2
  echo "Hint: pin pure-JS deps only, or publish per-target tarballs (Phase 31.5.b)." >&2
  exit 1
fi

echo "::notice::$VENDOR_DIR is pure-JS (no native addon files found)"
