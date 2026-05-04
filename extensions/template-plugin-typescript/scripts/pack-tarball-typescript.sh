#!/usr/bin/env bash
# Phase 31.5 — Pack the per-release `noarch` tarball for a
# TypeScript plugin. Output asset matches the convention 31.1
# expects:
#
#   <id>-<version>-noarch.tar.gz
#   ├── nexo-plugin.toml
#   ├── bin/<id>             # bash launcher, mode 0755
#   └── lib/
#       ├── plugin/
#       │   └── main.js      # compiled from src/main.ts via tsc
#       └── node_modules/
#           ├── nexo-plugin-sdk/dist/...
#           └── ...           # other vendored production deps
#
# Plus a sidecar `<asset>.sha256` containing one line of
# lowercase 64-char hex.
#
# Usage:
#   bash scripts/pack-tarball-typescript.sh
#
# Env overrides for tests:
#   SDK_SRC=/abs/path  Override the in-tree SDK source (default
#                      ../sdk-typescript). Must contain dist/.
#   SKIP_BUILD=1       Skip `tsc` — assume dist/ is already built.
#   SKIP_NPM=1         Skip vendoring deps from node_modules.

set -euo pipefail

# shellcheck source=./extract-plugin-meta.sh
source "$(dirname "$0")/extract-plugin-meta.sh"

TARGET="noarch"
SDK_SRC="${SDK_SRC:-../sdk-typescript}"

if [[ ! -d "$SDK_SRC" ]]; then
  echo "::error::SDK source not found at $SDK_SRC. Adjust SDK_SRC env or check the relative path." >&2
  exit 1
fi
if [[ ! -d "$SDK_SRC/dist" ]]; then
  echo "::error::SDK at $SDK_SRC has no dist/ — run 'npm run build' inside the SDK first." >&2
  exit 1
fi

# 1. Compile TypeScript → JavaScript (unless skipped).
if [[ -z "${SKIP_BUILD:-}" ]] && [[ -f tsconfig.json ]]; then
  if [[ -d node_modules/typescript ]]; then
    npx tsc --project tsconfig.json
  else
    echo "::warning::node_modules/typescript missing; skipping tsc step" >&2
  fi
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/bin" "$STAGE/lib/plugin" "$STAGE/lib/node_modules"

# 2. Plugin compiled source.
if [[ -d dist ]]; then
  cp -r dist/. "$STAGE/lib/plugin/"
elif [[ -d src ]]; then
  cp -r src/. "$STAGE/lib/plugin/"
fi

# 3. Vendor SDK at lib/node_modules/nexo-plugin-sdk.
mkdir -p "$STAGE/lib/node_modules/nexo-plugin-sdk"
cp -r "$SDK_SRC/dist" "$STAGE/lib/node_modules/nexo-plugin-sdk/dist"
cp "$SDK_SRC/package.json" "$STAGE/lib/node_modules/nexo-plugin-sdk/"

# 4. Vendor production deps from local node_modules. The publish
#    workflow runs `npm ci --omit=dev` before invoking this
#    script so node_modules contains only runtime deps. We skip
#    the SDK itself (already copied above) to avoid double-vendor.
if [[ -z "${SKIP_NPM:-}" ]] && [[ -d node_modules ]]; then
  for dep in node_modules/*/; do
    name="$(basename "$dep")"
    case "$name" in
      .bin) continue;;
      nexo-plugin-sdk) continue;;
      *)
        if [[ "$name" == @* ]]; then
          # Scoped packages: copy the whole @scope/ directory tree.
          mkdir -p "$STAGE/lib/node_modules/$name"
          cp -r "$dep". "$STAGE/lib/node_modules/$name/"
        else
          cp -r "$dep" "$STAGE/lib/node_modules/$name"
        fi
        ;;
    esac
  done
fi

# 5. Bash launcher.
cat > "$STAGE/bin/$PLUGIN_ID" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec env NODE_PATH="$DIR/lib/node_modules" node "$DIR/lib/plugin/main.js" "$@"
LAUNCHER
chmod 0755 "$STAGE/bin/$PLUGIN_ID"

# 6. Manifest at root.
cp nexo-plugin.toml "$STAGE/nexo-plugin.toml"

# 7. Tar + sha256 sidecar.
mkdir -p dist
ASSET="$PLUGIN_ID-$PLUGIN_VERSION-$TARGET.tar.gz"
tar -czf "dist/$ASSET" -C "$STAGE" .
( cd dist && sha256sum "$ASSET" | awk '{print $1}' > "$ASSET.sha256" )
( cd dist && printf '%s  %s\n' "$(cat "$ASSET.sha256")" "$ASSET" \
  | sha256sum -c - >/dev/null )

bytes="$(wc -c < "dist/$ASSET")"
echo "::notice::packed dist/$ASSET ($bytes bytes)"
