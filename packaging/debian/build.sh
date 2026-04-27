#!/usr/bin/env bash
# Build a Debian/Ubuntu `.deb` for nexo-rs.
#
# Targets:
#   - amd64 (Debian 12+, Ubuntu 22.04+)
#   - arm64 (Raspberry Pi 4/5, AWS Graviton, ARM servers)
#
# Outputs `dist/nexo-rs_<version>_<arch>.deb`. Ships a systemd
# service unit, creates a `nexo` system user on install, owns
# `/var/lib/nexo-rs/`. The unit does NOT auto-start — the
# operator runs `systemctl enable --now nexo-rs` after wiring
# `/etc/nexo-rs/` config files.
#
# Usage:
#   packaging/debian/build.sh                  # cross-compile from host
#   packaging/debian/build.sh --binary <path>  # use pre-built binary
#   packaging/debian/build.sh --arch arm64     # cross-target (requires zig/cross)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DIST_DIR="$REPO_ROOT/dist"
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

ARCH="amd64"
BINARY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --binary) BINARY="$2"; shift 2;;
        --arch)   ARCH="$2"; shift 2;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

case "$ARCH" in
    amd64) RUST_TARGET="x86_64-unknown-linux-gnu";;
    arm64) RUST_TARGET="aarch64-unknown-linux-gnu";;
    *)     echo "unsupported arch: $ARCH (use amd64 or arm64)" >&2; exit 2;;
esac

VERSION=$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | cut -d'"' -f2)
DESCRIPTION=$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^description/{sub(/^description *= */, ""); gsub(/^"|"$/, ""); print; exit}' "$REPO_ROOT/Cargo.toml")
MAINTAINER="Cristian Garcia <informacion@cristiangarcia.co>"
HOMEPAGE="https://lordmacu.github.io/nexo-rs/"

# ---------------------------------------------------------------------
# 1. Acquire the binary
# ---------------------------------------------------------------------
if [ -z "$BINARY" ]; then
    if [ "$RUST_TARGET" != "x86_64-unknown-linux-gnu" ] || [ "$(uname -m)" != "x86_64" ]; then
        if ! command -v cargo-zigbuild >/dev/null; then
            echo "ERROR: cross-target ($ARCH) needs cargo-zigbuild." >&2
            echo "  cargo install cargo-zigbuild && pip install ziglang" >&2
            exit 3
        fi
        rustup target add "$RUST_TARGET"
        echo "==> cross-compiling for $RUST_TARGET (cargo-zigbuild)"
        (cd "$REPO_ROOT" && \
         cargo zigbuild --release --bin nexo --target "$RUST_TARGET")
    else
        echo "==> compiling for host ($RUST_TARGET)"
        (cd "$REPO_ROOT" && cargo build --release --bin nexo)
    fi
    BINARY="$REPO_ROOT/target/$RUST_TARGET/release/nexo"
fi
[ -x "$BINARY" ] || { echo "binary not executable: $BINARY" >&2; exit 1; }

# ---------------------------------------------------------------------
# 2. Stage tree
# ---------------------------------------------------------------------
PKG_DIR="$BUILD_DIR/nexo-rs"
mkdir -p \
    "$PKG_DIR/usr/bin" \
    "$PKG_DIR/lib/systemd/system" \
    "$PKG_DIR/etc/nexo-rs" \
    "$PKG_DIR/usr/share/doc/nexo-rs" \
    "$PKG_DIR/DEBIAN"

cp "$BINARY"                "$PKG_DIR/usr/bin/nexo"
chmod 0755                  "$PKG_DIR/usr/bin/nexo"
cp "$REPO_ROOT/LICENSE-APACHE" "$PKG_DIR/usr/share/doc/nexo-rs/copyright-apache"
cp "$REPO_ROOT/LICENSE-MIT"    "$PKG_DIR/usr/share/doc/nexo-rs/copyright-mit"
cp "$REPO_ROOT/README.md"      "$PKG_DIR/usr/share/doc/nexo-rs/README.md"
cp "$SCRIPT_DIR/nexo-rs.service" "$PKG_DIR/lib/systemd/system/nexo-rs.service"

INSTALLED_SIZE=$(du -sk "$PKG_DIR" | cut -f1)

# ---------------------------------------------------------------------
# 3. control + maintainer scripts
# ---------------------------------------------------------------------
# Phase 27.4 — the bundled `nexo` binary is musl-static, so libsqlite3
# and libssl are linked in and NOT runtime deps. `adduser` runs in the
# preinst script before `Depends:` is resolved, so it lives in
# `Pre-Depends:`. Optional channel-plugin runtimes stay in `Recommends:`.
cat > "$PKG_DIR/DEBIAN/control" <<EOF
Package: nexo-rs
Version: $VERSION
Section: net
Priority: optional
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: $INSTALLED_SIZE
Pre-Depends: adduser
Depends: ca-certificates
Recommends: nats-server, git, ffmpeg, tesseract-ocr, cloudflared, yt-dlp, python3
Suggests: chromium
Homepage: $HOMEPAGE
Description: $DESCRIPTION
 Nexo is a multi-agent Rust framework with a NATS event bus,
 pluggable LLM providers, per-agent credentials, MCP support,
 and channel plugins for WhatsApp, Telegram, Email, and Browser.
 .
 The package ships a systemd unit (nexo-rs.service) which the
 operator enables manually after wiring /etc/nexo-rs/ configs.
 The 'nexo' system user is created on first install and owns
 /var/lib/nexo-rs/.
EOF

# Pre-install: create system user before extracting files.
cat > "$PKG_DIR/DEBIAN/preinst" <<'EOF'
#!/bin/sh
set -e
if ! getent passwd nexo >/dev/null; then
    adduser --system --group --no-create-home \
            --home /var/lib/nexo-rs \
            --shell /usr/sbin/nologin \
            --disabled-password \
            --gecos "Nexo agent runtime" \
            nexo
fi
EOF
chmod 0755 "$PKG_DIR/DEBIAN/preinst"

# Post-install: chown state dir, reload systemd. NO auto-start.
cat > "$PKG_DIR/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
mkdir -p /var/lib/nexo-rs /var/log/nexo-rs /etc/nexo-rs
chown -R nexo:nexo /var/lib/nexo-rs /var/log/nexo-rs
chmod 0750         /var/lib/nexo-rs /var/log/nexo-rs
chmod 0750         /etc/nexo-rs
systemctl daemon-reload >/dev/null 2>&1 || :
cat <<MSG

  nexo-rs installed.

  Next steps:
    1. Wire your config:    sudo -u nexo nexo setup
       (writes /etc/nexo-rs/{agents,broker,llm,memory}.yaml)
    2. Enable + start:      sudo systemctl enable --now nexo-rs
    3. Tail logs:           sudo journalctl -u nexo-rs -f

  Docs: https://lordmacu.github.io/nexo-rs/

MSG
EOF
chmod 0755 "$PKG_DIR/DEBIAN/postinst"

# Pre-removal: stop the service if running.
cat > "$PKG_DIR/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then
    systemctl stop nexo-rs.service >/dev/null 2>&1 || :
fi
EOF
chmod 0755 "$PKG_DIR/DEBIAN/prerm"

# Post-removal: reload systemd; preserve state dir on `remove`,
# wipe it on `purge`.
cat > "$PKG_DIR/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
case "$1" in
    purge)
        rm -rf /var/lib/nexo-rs /var/log/nexo-rs
        deluser --system nexo 2>/dev/null || :
        ;;
esac
systemctl daemon-reload >/dev/null 2>&1 || :
EOF
chmod 0755 "$PKG_DIR/DEBIAN/postrm"

# ---------------------------------------------------------------------
# 4. Build the .deb
# ---------------------------------------------------------------------
mkdir -p "$DIST_DIR"
DEB_NAME="nexo-rs_${VERSION}_${ARCH}.deb"
OUT="$DIST_DIR/$DEB_NAME"

if command -v dpkg-deb >/dev/null; then
    dpkg-deb --build --root-owner-group "$PKG_DIR" "$OUT"
else
    echo "ERROR: dpkg-deb missing. Install with 'apt install dpkg-dev'." >&2
    exit 4
fi

echo "==> built $OUT ($(du -h "$OUT" | cut -f1))"
echo "==> install with:  sudo apt install ./dist/$DEB_NAME"
