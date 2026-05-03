#!/usr/bin/env bash
# Phase 31.4 — Pack the per-release `noarch` tarball for a Python
# plugin. Output asset matches the convention 31.1 expects:
#
#   <id>-<version>-noarch.tar.gz
#   ├── nexo-plugin.toml
#   ├── bin/<id>            # bash launcher, mode 0755
#   └── lib/
#       ├── plugin/
#       │   └── main.py
#       └── nexo_plugin_sdk/
#           └── ...
#       (plus any vendored requirements.txt deps)
#
# Plus a sidecar `<asset>.sha256` containing one line of
# lowercase 64-char hex.
#
# Usage:
#   bash scripts/pack-tarball-python.sh
#
# Override the SDK source via `SDK_SRC=/abs/path` and the deps
# vendor target via `SKIP_PIP=1` for tests.

set -euo pipefail

# shellcheck source=./extract-plugin-meta.sh
source "$(dirname "$0")/extract-plugin-meta.sh"

TARGET="noarch"
SDK_SRC="${SDK_SRC:-../sdk-python/nexo_plugin_sdk}"

if [[ ! -d "$SDK_SRC" ]]; then
  echo "::error::SDK source not found at $SDK_SRC. Adjust SDK_SRC env or check the relative path." >&2
  exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/bin" "$STAGE/lib/plugin"

# 1. Vendor the SDK at lib/nexo_plugin_sdk.
cp -r "$SDK_SRC" "$STAGE/lib/"

# 2. Vendor any requirements.txt deps (pure-Python only — see
#    verify-pure-python.sh).
if [[ -z "${SKIP_PIP:-}" ]] && [[ -s requirements.txt ]] \
    && grep -qvE '^\s*(#|$)' requirements.txt; then
  pip install --target "$STAGE/lib" --quiet -r requirements.txt
fi

# 3. Plugin source.
cp -r src/. "$STAGE/lib/plugin/"

# 4. Author's launcher script (universal — sets PYTHONPATH).
cat > "$STAGE/bin/$PLUGIN_ID" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec env PYTHONPATH="$DIR/lib" python3 "$DIR/lib/plugin/main.py" "$@"
LAUNCHER
chmod 0755 "$STAGE/bin/$PLUGIN_ID"

# 5. Manifest at root.
cp nexo-plugin.toml "$STAGE/nexo-plugin.toml"

# 6. Pack + sha256 sidecar.
mkdir -p dist
ASSET="$PLUGIN_ID-$PLUGIN_VERSION-$TARGET.tar.gz"
tar -czf "dist/$ASSET" -C "$STAGE" .
( cd dist && sha256sum "$ASSET" | awk '{print $1}' > "$ASSET.sha256" )
( cd dist && printf '%s  %s\n' "$(cat "$ASSET.sha256")" "$ASSET" \
  | sha256sum -c - >/dev/null )

bytes="$(wc -c < "dist/$ASSET")"
echo "::notice::packed dist/$ASSET ($bytes bytes)"
