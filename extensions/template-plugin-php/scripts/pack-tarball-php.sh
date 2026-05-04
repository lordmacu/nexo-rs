#!/usr/bin/env bash
# Phase 31.5.c — Pack the per-release `noarch` tarball for a
# PHP plugin. Output asset matches the convention 31.1 expects:
#
#   <id>-<version>-noarch.tar.gz
#   ├── nexo-plugin.toml
#   ├── bin/<id>             # bash launcher, mode 0755
#   └── lib/
#       ├── plugin/main.php
#       └── vendor/          # composer install --no-dev output
#           ├── autoload.php
#           ├── nexo/plugin-sdk/...
#           ├── yosymfony/toml/...
#           └── composer/...
#
# Plus a sidecar `<asset>.sha256` containing one line of
# lowercase 64-char hex.
#
# Usage:
#   bash scripts/pack-tarball-php.sh
#
# Env overrides for tests:
#   SDK_SRC=/abs/path  Override the in-tree SDK source (default
#                      ../sdk-php). Used by test_pack_tarball.
#   SKIP_COMPOSER=1    Skip `composer install`. Use when vendor/
#                      already exists from a prior step or when
#                      the test is providing a synthetic vendor
#                      tree.

set -euo pipefail

# shellcheck source=./extract-plugin-meta.sh
source "$(dirname "$0")/extract-plugin-meta.sh"

TARGET="noarch"
SDK_SRC="${SDK_SRC:-../sdk-php}"

if [[ ! -d "$SDK_SRC" ]]; then
  echo "::error::SDK source not found at $SDK_SRC. Adjust SDK_SRC env or check the relative path." >&2
  exit 1
fi

# 1. Vendor production deps via Composer.
if [[ -z "${SKIP_COMPOSER:-}" ]] && [[ -f composer.json ]]; then
  composer install --no-dev --optimize-autoloader --classmap-authoritative --quiet
fi

if [[ ! -d vendor ]]; then
  echo "::error::vendor/ missing — run 'composer install' or unset SKIP_COMPOSER" >&2
  exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/bin" "$STAGE/lib/plugin" "$STAGE/lib/vendor"

# 2. Plugin source.
cp -r src/. "$STAGE/lib/plugin/"

# 3. Vendor dir from local composer install.
cp -r vendor/. "$STAGE/lib/vendor/"

# 4. Bash launcher.
cat > "$STAGE/bin/$PLUGIN_ID" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec env php -d display_errors=stderr -d log_errors=0 \
  "$DIR/lib/plugin/main.php" "$@"
LAUNCHER
chmod 0755 "$STAGE/bin/$PLUGIN_ID"

# 5. Manifest at root.
cp nexo-plugin.toml "$STAGE/nexo-plugin.toml"

# 6. Tar + sha256 sidecar.
mkdir -p dist
ASSET="$PLUGIN_ID-$PLUGIN_VERSION-$TARGET.tar.gz"
tar -czf "dist/$ASSET" -C "$STAGE" .
( cd dist && sha256sum "$ASSET" | awk '{print $1}' > "$ASSET.sha256" )
( cd dist && printf '%s  %s\n' "$(cat "$ASSET.sha256")" "$ASSET" \
  | sha256sum -c - >/dev/null )

bytes="$(wc -c < "dist/$ASSET")"
echo "::notice::packed dist/$ASSET ($bytes bytes)"
