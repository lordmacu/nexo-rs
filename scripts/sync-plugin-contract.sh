#!/usr/bin/env bash
# Vendor the workspace-root nexo-plugin-contract.md into
# docs/src/plugins/contract.md so mdbook builds carry the
# canonical wire spec.
#
# Phase 31.9 — keeps a single source of truth (the workspace-root
# file) while letting the published mdbook ship the spec inline.
# Run after editing nexo-plugin-contract.md, or call --check from
# CI to gate against drift.
#
# Usage:
#   scripts/sync-plugin-contract.sh           # rewrite vendored copy
#   scripts/sync-plugin-contract.sh --check   # exit 1 if vendored copy is stale

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT_DIR/nexo-plugin-contract.md"
DEST="$ROOT_DIR/docs/src/plugins/contract.md"

if [[ ! -f "$SRC" ]]; then
    echo "ERROR: source $SRC missing" >&2
    exit 1
fi

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

{
    echo '<!-- AUTO-VENDORED FROM nexo-plugin-contract.md — DO NOT EDIT.'
    echo '     Run scripts/sync-plugin-contract.sh to refresh. -->'
    echo
    cat "$SRC"
    echo
    echo '## See also'
    echo
    echo '- [Authoring overview](./authoring.md)'
    echo '- [Rust SDK](./rust-sdk.md), [Python SDK](./python-sdk.md), [TypeScript SDK](./typescript-sdk.md), [PHP SDK](./php-sdk.md)'
    echo '- [Publishing a plugin](./publishing.md), [Signing & publishing](./signing-and-publishing.md)'
} > "$TMP"

if [[ "${1:-}" == "--check" ]]; then
    if ! diff -q "$DEST" "$TMP" >/dev/null 2>&1; then
        echo "ERROR: $DEST is stale; run scripts/sync-plugin-contract.sh" >&2
        exit 1
    fi
    echo "OK: $DEST is up-to-date with $SRC"
    exit 0
fi

mv "$TMP" "$DEST"
trap - EXIT
echo "Synced $DEST"
