#!/usr/bin/env bash
# Phase 31.2 — Source this from a publish workflow to export
#   PLUGIN_ID
#   PLUGIN_VERSION
# read out of `nexo-plugin.toml`. Convention: `id` and `version`
# appear as single-line `key = "value"` entries inside the
# `[plugin]` section (the template manifest matches).
#
# Override the manifest path with `MANIFEST=/path/to/file` for
# tests.

set -euo pipefail

MANIFEST="${MANIFEST:-nexo-plugin.toml}"

if [[ ! -f "$MANIFEST" ]]; then
  echo "::error::manifest not found at $MANIFEST" >&2
  return 1 2>/dev/null || exit 1
fi

PLUGIN_ID="$(grep -E '^[[:space:]]*id[[:space:]]*=' "$MANIFEST" \
  | head -1 \
  | sed -E 's/.*=[[:space:]]*"([^"]*)".*/\1/')"
PLUGIN_VERSION="$(grep -E '^[[:space:]]*version[[:space:]]*=' "$MANIFEST" \
  | head -1 \
  | sed -E 's/.*=[[:space:]]*"([^"]*)".*/\1/')"

if [[ -z "$PLUGIN_ID" || -z "$PLUGIN_VERSION" ]]; then
  echo "::error::failed to extract plugin id/version from $MANIFEST" >&2
  return 1 2>/dev/null || exit 1
fi

export PLUGIN_ID PLUGIN_VERSION
