#!/usr/bin/env bash
# Pack the per-release `noarch` tarball for a Python plugin. Output
# asset matches the convention the installer expects:
#
#   <id>-<version>-noarch.tar.gz
#   ├── nexo-plugin.toml
#   ├── bin/<id>            # bash launcher, mode 0755
#   └── lib/
#       ├── plugin/
#       │   └── main.py
#       └── nexo_plugin_sdk/  # the `nexoai` PyPI package; module name
#           └── ...           # is `nexo_plugin_sdk`
#       (plus any other vendored requirements.txt deps)
#
# Plus a sidecar `<asset>.sha256` containing one line of
# lowercase 64-char hex.
#
# Usage:
#   bash scripts/pack-tarball-python.sh
#
# The SDK (and any other deps) are vendored from `requirements.txt`
# via `pip install --target lib`. For tests / local dev against a
# checkout you can short-circuit that with `SDK_SRC=/abs/path/to/
# nexo_plugin_sdk` (copied verbatim into lib/) and `SKIP_PIP=1`.

set -euo pipefail

# shellcheck source=./extract-plugin-meta.sh
source "$(dirname "$0")/extract-plugin-meta.sh"

TARGET="noarch"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/bin" "$STAGE/lib/plugin"

# 1. Optional: vendor an SDK checkout directly (tests / local dev).
if [[ -n "${SDK_SRC:-}" ]]; then
  if [[ ! -d "$SDK_SRC" ]]; then
    echo "::error::SDK_SRC set but not a directory: $SDK_SRC" >&2
    exit 1
  fi
  cp -r "$SDK_SRC" "$STAGE/lib/"
fi

# 2. Vendor requirements.txt deps (pure-Python only — see
#    verify-pure-python.sh). This is where the SDK (`nexoai`) comes
#    from in a normal build.
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
