#!/usr/bin/env bash
# Idempotently append a `[profile.release-fast]` block to every Rust
# extension Cargo.toml under proyecto/extensions/. Skips files that
# already have a `release-fast` profile defined.
#
# Usage: bash /tmp/add-release-fast.sh
set -euo pipefail

ROOT=/home/familia/chat/proyecto/extensions

block=$(cat <<'TOML'

# Iterative release builds — full opt-level, no LTO, parallel
# codegen. Costs ~5 % runtime perf vs `release`; saves ~50 % build
# time. Use for local validation; reserve `--release` for publish.
[profile.release-fast]
inherits      = "release"
lto           = false
codegen-units = 16
strip         = "none"
TOML
)

added=0
skipped=0
for d in "$ROOT"/*/; do
    f="$d/Cargo.toml"
    [ -f "$f" ] || continue
    grep -qE "^\[package\]" "$f" || continue            # only Rust crates
    if grep -qE "^\[profile\.release-fast\]" "$f"; then
        skipped=$((skipped + 1))
        continue
    fi
    printf '%s\n' "$block" >> "$f"
    added=$((added + 1))
    echo "  + $f"
done

echo
echo "Added: $added | Skipped (already had it): $skipped"
