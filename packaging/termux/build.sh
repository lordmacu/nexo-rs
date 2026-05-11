#!/usr/bin/env bash
# Build a Termux-compatible `nexo-rs.deb` for aarch64-linux-android.
#
# Termux uses Debian-style packages (`pkg install` is `apt` underneath)
# but the `arch:` field is `aarch64` (not `arm64`) and binaries link
# against bionic libc, not glibc. This script produces a minimal deb
# that drops the `nexo` binary into `$PREFIX/bin/` and pulls runtime
# deps Termux already ships (`libsqlite`, `openssl`, `git`, `ffmpeg`,
# `tesseract`, `python`, `yt-dlp`).
#
# Usage:
#   packaging/termux/build.sh                  # cross-compile from host
#   packaging/termux/build.sh --binary <path>  # use pre-built binary
#
# The output `dist/nexo-rs_<version>_aarch64.deb` is uploaded as a
# release artifact by `.github/workflows/release.yml` (Phase 27.2).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DIST_DIR="$REPO_ROOT/dist"
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

# Read the version + maintainer once from the workspace bin's Cargo.toml
# so `build.sh` doesn't drift from the source of truth.
VERSION=$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | cut -d'"' -f2)
DESCRIPTION=$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^description/{sub(/^description *= */, ""); gsub(/^"|"$/, ""); print; exit}' "$REPO_ROOT/Cargo.toml")
MAINTAINER="Cristian Garcia <informacion@cristiangarcia.co>"
HOMEPAGE="https://lordmacu.github.io/nexo-rs/"
ARCH="aarch64"

# ---------------------------------------------------------------------
# 1. Acquire the binary
# ---------------------------------------------------------------------
BINARY=""
if [ "${1:-}" = "--binary" ] && [ -n "${2:-}" ]; then
    BINARY="$2"
    [ -f "$BINARY" ] || { echo "no such file: $BINARY" >&2; exit 1; }
elif [ -n "${TERMUX_BINARY:-}" ]; then
    BINARY="$TERMUX_BINARY"
else
    # Cross-compile via `cargo-ndk` + the real Android NDK. Phase
    # 27.2-follow-up.b proved that zig 0.13's bundled Android sysroot
    # is incomplete for the C-source crates we depend on (`ring`
    # needs `<assert.h>`, `libsqlite3-sys` needs `<stdio.h>`), and
    # cargo-zigbuild 0.22.3 doesn't accept an API-level suffix on
    # the target name. cargo-ndk delegates to the actual NDK
    # toolchain which ships full Bionic libc headers — works
    # end-to-end with the rustls/tokio-rustls ring pin landed in
    # the same wave.
    #
    # Requires:
    #   - cargo-ndk:        cargo install cargo-ndk --locked
    #   - Android NDK r26+: download via `setup-ndk` action in CI
    #                       or `sdkmanager` locally, then export
    #                       `ANDROID_NDK_HOME` to the install root.
    echo "==> cross-compiling for aarch64-linux-android (cargo-ndk)"
    if ! command -v cargo-ndk >/dev/null; then
        echo "  ERROR: cargo-ndk missing. Install with:" >&2
        echo "    cargo install cargo-ndk --locked" >&2
        exit 2
    fi
    if [ -z "${ANDROID_NDK_HOME:-}" ]; then
        echo "  ERROR: ANDROID_NDK_HOME unset." >&2
        echo "  Install Android NDK r26+ and export ANDROID_NDK_HOME to its root." >&2
        exit 2
    fi
    rustup target add aarch64-linux-android
    # `--platform 21` matches the minimum Bionic API level we target
    # (Android 5.0+). cargo-ndk infers the right toolchain wrapper
    # from `$ANDROID_NDK_HOME` + the requested target.
    (cd "$REPO_ROOT" && \
     cargo ndk --target arm64-v8a --platform 21 build --release --bin nexo)
    BINARY="$REPO_ROOT/target/aarch64-linux-android/release/nexo"
fi

[ -x "$BINARY" ] || { echo "binary not executable: $BINARY" >&2; exit 1; }
echo "==> using binary: $BINARY"

# ---------------------------------------------------------------------
# 2. Stage the .deb tree under $PREFIX
# ---------------------------------------------------------------------
PKG_DIR="$BUILD_DIR/nexo-rs"
PREFIX="data/data/com.termux/files/usr"

mkdir -p "$PKG_DIR/$PREFIX/bin"
mkdir -p "$PKG_DIR/$PREFIX/share/doc/nexo-rs"
mkdir -p "$PKG_DIR/$PREFIX/share/licenses/nexo-rs"
mkdir -p "$PKG_DIR/DEBIAN"

cp "$BINARY" "$PKG_DIR/$PREFIX/bin/nexo"
chmod 0755 "$PKG_DIR/$PREFIX/bin/nexo"

cp "$REPO_ROOT/LICENSE-APACHE" "$PKG_DIR/$PREFIX/share/licenses/nexo-rs/"
cp "$REPO_ROOT/LICENSE-MIT"    "$PKG_DIR/$PREFIX/share/licenses/nexo-rs/"
cp "$REPO_ROOT/README.md"      "$PKG_DIR/$PREFIX/share/doc/nexo-rs/"

INSTALLED_SIZE=$(du -sk "$PKG_DIR/$PREFIX" | cut -f1)

# ---------------------------------------------------------------------
# 3. control file — Termux pkg metadata
# ---------------------------------------------------------------------
cat > "$PKG_DIR/DEBIAN/control" <<EOF
Package: nexo-rs
Version: $VERSION
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: $INSTALLED_SIZE
Depends: libsqlite, openssl, ca-certificates
Recommends: git, ffmpeg, tesseract, python, yt-dlp, dumb-init
Section: net
Priority: optional
Homepage: $HOMEPAGE
Description: $DESCRIPTION
 Nexo is a multi-agent Rust framework with a NATS event bus,
 pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat,
 Gemini, DeepSeek), per-agent credentials, MCP support, and
 channel plugins for WhatsApp, Telegram, Email, and Browser.
 .
 The Termux build runs broker.type=local by default so a phone
 install does not need a separate NATS daemon. Switch to
 broker.type=nats once a NATS server is reachable from the phone.
EOF

# ---------------------------------------------------------------------
# 4. Maintainer scripts — first-run setup
# ---------------------------------------------------------------------
cat > "$PKG_DIR/DEBIAN/postinst" <<'EOF'
#!/data/data/com.termux/files/usr/bin/sh
# postinst: scaffold $HOME/.nexo/ + ~/.config/nexo/ on first install.
set -eu
NEXO_HOME="${HOME}/.nexo"
if [ ! -d "$NEXO_HOME" ]; then
    mkdir -p "$NEXO_HOME/data" "$NEXO_HOME/secret"
    chmod 700 "$NEXO_HOME/secret"
    # Phase 95 — seed sample YAMLs in the XDG default location so
    # `nexo` (Phase 93 zero-config) AND `nexo --config <dir>`
    # paths both have something to read from a fresh install.
    # Skipped on upgrade because the inner if-guards on the HOME
    # dir presence.
    CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/nexo"
    if [ ! -f "$CFG_DIR/broker.yaml" ]; then
        mkdir -p "$CFG_DIR"
        nexo init --output "$CFG_DIR" >/dev/null 2>&1 || :
    fi
    cat <<MSG

  nexo-rs installed.

  Quick smoke test (Phase 93 zero-config):
    nexo                  # boots with defaults — admin RPCs live

  Common commands:
    nexo init             # re-emit sample YAMLs (already seeded
                          # by postinst; re-run with --force to
                          # refresh templates after upgrade)
    nexo --help
    nexo setup            # interactive wizard (LLM key + channels)
    nexo doctor           # verify dependencies + capabilities

  Switching broker mode (Phase 92.9):
    nexo set-broker nats --url nats://localhost:4222
    nexo set-broker local

  Docs: https://lordmacu.github.io/nexo-rs/

MSG
fi
exit 0
EOF
chmod 0755 "$PKG_DIR/DEBIAN/postinst"

# ---------------------------------------------------------------------
# 5. Build the .deb
# ---------------------------------------------------------------------
mkdir -p "$DIST_DIR"
DEB_NAME="nexo-rs_${VERSION}_${ARCH}.deb"
OUT="$DIST_DIR/$DEB_NAME"

if command -v dpkg-deb >/dev/null; then
    dpkg-deb --build --root-owner-group "$PKG_DIR" "$OUT"
elif command -v fakeroot >/dev/null && command -v ar >/dev/null; then
    # Fallback for hosts without dpkg-deb (e.g., macOS without
    # homebrew dpkg). Builds the deb manually.
    (cd "$PKG_DIR" && \
     fakeroot tar -czf "$BUILD_DIR/data.tar.gz" -C . --transform='s|^\./||' --exclude=DEBIAN .)
    (cd "$PKG_DIR/DEBIAN" && fakeroot tar -czf "$BUILD_DIR/control.tar.gz" .)
    echo "2.0" > "$BUILD_DIR/debian-binary"
    (cd "$BUILD_DIR" && \
     ar -rc "$OUT" debian-binary control.tar.gz data.tar.gz)
else
    echo "ERROR: need dpkg-deb or (fakeroot + ar) on PATH" >&2
    exit 3
fi

echo "==> built $OUT ($(du -h "$OUT" | cut -f1))"

# Phase 27.2 — emit a sha256 sidecar so `gh release upload` (and
# `scripts/release-check.sh`) can verify integrity end-to-end.
( cd "$DIST_DIR" && sha256sum "$DEB_NAME" > "${DEB_NAME}.sha256" )
echo "==> wrote $OUT.sha256"

echo "==> install on Termux with:"
echo "      pkg install ./nexo-rs_${VERSION}_${ARCH}.deb"
