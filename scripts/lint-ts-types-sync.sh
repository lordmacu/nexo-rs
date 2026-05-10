#!/usr/bin/env bash
# Phase 83.12.ts-types-codegen — verify the checked-in
# `agent-creator-microapp/frontend/src/api/types.gen.ts` is
# up-to-date with the Rust sources at HEAD. Mirrors the
# locale-list drift-lint pattern from
# `scripts/lint-locale-list-sync.sh`.
#
# Strategy:
#   1. Snapshot the current `types.gen.ts`.
#   2. Run `regen-ts-types.sh` (which overwrites the file).
#   3. Diff snapshot vs new content.
#   4. If different: print the diff, restore the snapshot
#      (so a failed lint doesn't leave the checkout dirty),
#      exit 1.
#   5. If identical: print success, exit 0.

set -euo pipefail

cd "$(dirname "$0")/.."

TS_FILE="../agent-creator-microapp/frontend/src/api/types.gen.ts"
if [[ ! -f "$TS_FILE" ]]; then
    echo "::warning::types.gen.ts not present at $TS_FILE — skipping lint" >&2
    exit 0
fi

# Snapshot.
SNAPSHOT=$(mktemp)
trap "rm -f $SNAPSHOT" EXIT
cp "$TS_FILE" "$SNAPSHOT"

# Regenerate.
bash scripts/regen-ts-types.sh > /dev/null

# Diff.
if ! diff -q "$SNAPSHOT" "$TS_FILE" >/dev/null 2>&1; then
    echo "❌ types.gen.ts is out-of-date with Rust sources." >&2
    echo >&2
    echo "Diff (snapshot vs regenerated):" >&2
    diff "$SNAPSHOT" "$TS_FILE" | head -60 >&2 || true
    echo >&2
    echo "Regenerate by running:" >&2
    echo "  cd proyecto && bash scripts/regen-ts-types.sh" >&2
    echo "  git add -- ../agent-creator-microapp/frontend/src/api/types.gen.ts" >&2

    # Restore the snapshot so the lint failure doesn't leave the
    # checkout dirty.
    cp "$SNAPSHOT" "$TS_FILE"
    exit 1
fi

echo "✅ types.gen.ts in sync with Rust sources."
