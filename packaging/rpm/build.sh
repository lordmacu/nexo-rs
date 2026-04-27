#!/usr/bin/env bash
# Build a `.rpm` for nexo-rs (Fedora / RHEL / openSUSE).
#
# Targets x86_64 by default; aarch64 supported with --arch arm64.
# Outputs `dist/nexo-rs-<version>-1.<dist>.<arch>.rpm`.
#
# Usage:
#   packaging/rpm/build.sh                  # cross-compile from host
#   packaging/rpm/build.sh --binary <path>  # use pre-built binary
#   packaging/rpm/build.sh --arch arm64     # build for aarch64

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DIST_DIR="$REPO_ROOT/dist"
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

ARCH="x86_64"
BINARY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --binary) BINARY="$2"; shift 2;;
        --arch)
            case "$2" in
                amd64|x86_64) ARCH="x86_64";;
                arm64|aarch64) ARCH="aarch64";;
                *) echo "unsupported arch: $2" >&2; exit 2;;
            esac
            shift 2;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

case "$ARCH" in
    x86_64)  RUST_TARGET="x86_64-unknown-linux-gnu";;
    aarch64) RUST_TARGET="aarch64-unknown-linux-gnu";;
esac

VERSION=$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | cut -d'"' -f2)

# ---------------------------------------------------------------------
# 1. Acquire the binary
# ---------------------------------------------------------------------
if [ -z "$BINARY" ]; then
    if [ "$RUST_TARGET" != "x86_64-unknown-linux-gnu" ] || [ "$(uname -m)" != "x86_64" ]; then
        if ! command -v cargo-zigbuild >/dev/null; then
            echo "ERROR: cross-target ($ARCH) needs cargo-zigbuild." >&2
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
# 2. Stage source tarball expected by the spec
# ---------------------------------------------------------------------
SRC_NAME="nexo-rs-${VERSION}"
SRC_DIR="$BUILD_DIR/$SRC_NAME"
mkdir -p "$SRC_DIR/target/release"

cp "$BINARY"                          "$SRC_DIR/target/release/nexo"
cp "$REPO_ROOT/README.md"             "$SRC_DIR/"
cp "$REPO_ROOT/LICENSE-APACHE"        "$SRC_DIR/"
cp "$REPO_ROOT/LICENSE-MIT"           "$SRC_DIR/"

(cd "$BUILD_DIR" && tar -czf "$SRC_NAME.tar.gz" "$SRC_NAME")

# ---------------------------------------------------------------------
# 3. rpmbuild tree
# ---------------------------------------------------------------------
RPM_TOP="$BUILD_DIR/rpmbuild"
mkdir -p "$RPM_TOP"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
cp "$BUILD_DIR/$SRC_NAME.tar.gz"      "$RPM_TOP/SOURCES/"
cp "$REPO_ROOT/packaging/debian/nexo-rs.service" "$RPM_TOP/SOURCES/"

# Inject the workspace version into the spec at build time so the
# checked-in spec can stay at a placeholder version that drifts
# without breaking releases.
sed "s/^Version:.*/Version:        $VERSION/" \
    "$SCRIPT_DIR/nexo-rs.spec" > "$RPM_TOP/SPECS/nexo-rs.spec"

if ! command -v rpmbuild >/dev/null; then
    echo "ERROR: rpmbuild missing. Install with 'dnf install rpm-build'." >&2
    exit 4
fi

rpmbuild --define "_topdir $RPM_TOP" \
         --target "$ARCH-unknown-linux-gnu" \
         -bb "$RPM_TOP/SPECS/nexo-rs.spec"

# ---------------------------------------------------------------------
# 4. Collect output
# ---------------------------------------------------------------------
mkdir -p "$DIST_DIR"
find "$RPM_TOP/RPMS" -name "*.rpm" -exec cp {} "$DIST_DIR/" \;

OUT=$(ls -t "$DIST_DIR"/nexo-rs-*.rpm | head -1)
echo "==> built $OUT ($(du -h "$OUT" | cut -f1))"
echo "==> install with:  sudo dnf install $OUT  (or yum / zypper)"
