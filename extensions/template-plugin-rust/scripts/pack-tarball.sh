#!/usr/bin/env bash
# Phase 31.2 — Pack the per-target release tarball published as a
# GitHub Release asset. The asset name + layout match the
# convention 31.1 expects:
#
#   <id>-<version>-<target>.tar.gz
#   ├── nexo-plugin.toml
#   └── bin/<id>            # executable, mode 0755
#
# Plus a sidecar `<asset>.sha256` containing one line of
# lowercase 64-char hex.
#
# Usage:
#   bash scripts/pack-tarball.sh <target-triple>
# e.g.
#   bash scripts/pack-tarball.sh x86_64-unknown-linux-musl
#
# Run from the plugin repo root so `target/<target>/release/<bin>`
# and `nexo-plugin.toml` resolve. Override the binary search with
# `BIN_SRC=/abs/path` for tests.

set -euo pipefail

TARGET="${1:-}"
if [[ -z "$TARGET" ]]; then
  echo "::error::target triple required as first arg" >&2
  exit 1
fi

# shellcheck source=./extract-plugin-meta.sh
source "$(dirname "$0")/extract-plugin-meta.sh"

BIN_SRC="${BIN_SRC:-target/$TARGET/release/$PLUGIN_ID}"
if [[ ! -f "$BIN_SRC" ]]; then
  echo "::error::built binary missing at $BIN_SRC" >&2
  exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/bin"
cp "$BIN_SRC" "$STAGE/bin/$PLUGIN_ID"
chmod 0755 "$STAGE/bin/$PLUGIN_ID"
cp nexo-plugin.toml "$STAGE/nexo-plugin.toml"

mkdir -p dist
ASSET="$PLUGIN_ID-$PLUGIN_VERSION-$TARGET.tar.gz"
tar -czf "dist/$ASSET" -C "$STAGE" .

# sha256 sidecar — one line of lowercase hex.
( cd dist && sha256sum "$ASSET" | awk '{print $1}' > "$ASSET.sha256" )

# Self-test: re-verify sidecar matches.
( cd dist && printf '%s  %s\n' "$(cat "$ASSET.sha256")" "$ASSET" \
  | sha256sum -c - >/dev/null )

bytes="$(wc -c < "dist/$ASSET")"
echo "::notice::packed dist/$ASSET ($bytes bytes)"
