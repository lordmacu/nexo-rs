#!/usr/bin/env bash
# nexo-rs installer — served at https://lordmacu.github.io/nexo-rs/install.sh
#
# Usage:
#   curl -fsSL https://lordmacu.github.io/nexo-rs/install.sh | bash
#   curl -fsSL https://lordmacu.github.io/nexo-rs/install.sh | bash -s -- --install-dir ~/bin
#
# What it does:
#   1. Detects your OS + arch and downloads the matching pre-built
#      binary tarball from the latest GitHub release, verifies its
#      sha256, extracts `nexo`, and drops it on your PATH.
#   2. Falls back to `cargo install nexo-rs` (crates.io) if there's no
#      pre-built binary for your platform, then to `cargo install --git`
#      if crates.io is unreachable.
#   3. Installs the bundled channel plugins (whatsapp, telegram, email,
#      browser), `nexo-plugin-admin` (the admin web UI behind
#      `nexo admin`), and a default persona pack. Each plugin uses a
#      prebuilt GitHub Release tarball when one matches the host arch,
#      otherwise falls back to `cargo install <crate>` from crates.io.
#      Best-effort — a failed plugin never aborts the install. Skip the
#      whole step with --no-plugins, or just the persona with --no-persona.
#
# Pre-built targets: Linux x86_64 / aarch64 (static musl), macOS
# Intel / Apple Silicon, Windows x86_64 (MSVC). On Windows run this
# from Git Bash, OR use the native PowerShell installer:
#   irm https://lordmacu.github.io/nexo-rs/install.ps1 | iex
# (WSL also works — it then sees Linux.) Termux: `pkg install` the
# aarch64 .deb from the same release page.
#
# Flags:
#   --install-dir <dir>   where to put the `nexo` binary
#                         (default: $CARGO_HOME/bin if cargo is on
#                          PATH, else ~/.local/bin)
#   --from-source         skip the binary download, go straight to cargo
#   --no-plugins          install only `nexo` — skip the bundled
#                         plugins and the default persona
#   --no-persona          install plugins but skip the default persona
#
# Override the install dir with NEXO_INSTALL_DIR=... too.
#
# Exit codes: 0 ok · 1 unsupported platform / no fallback · 2 download
# or extract failed · 3 cargo fallback failed.

set -eu

REPO="lordmacu/nexo-rs"
RELEASES="https://github.com/${REPO}/releases"
INSTALL_DIR="${NEXO_INSTALL_DIR:-}"
FROM_SOURCE=0
INSTALL_PLUGINS=1

# Bundled plugins installed by default (channel plugins ship GitHub
# Release tarballs; the admin UI ships on crates.io). Override the
# channel set with NEXO_PLUGINS="a b c"; use --no-plugins to skip all.
PLUGINS="${NEXO_PLUGINS:-nexo-plugin-whatsapp nexo-plugin-telegram nexo-plugin-email nexo-plugin-browser}"

# Default persona pack — a ready-to-run agent so the daemon does
# something useful on first boot. Set NEXO_PERSONA="" or pass
# --no-persona to skip; NEXO_PERSONA="owner/repo" to pick another.
PERSONA="${NEXO_PERSONA-lordmacu/nexo-persona-cody}"

while [ $# -gt 0 ]; do
    case "$1" in
        --install-dir) INSTALL_DIR="$2"; shift 2 ;;
        --install-dir=*) INSTALL_DIR="${1#*=}"; shift ;;
        --from-source) FROM_SOURCE=1; shift ;;
        --no-plugins) INSTALL_PLUGINS=0; PERSONA=""; shift ;;
        --no-persona) PERSONA=""; shift ;;
        -h|--help) sed -n '2,39p' "$0" 2>/dev/null || true; exit 0 ;;
        *) echo "warning: ignoring unknown flag '$1'" >&2; shift ;;
    esac
done

banner() {
    cat <<'EOF'

  ███╗   ██╗███████╗██╗  ██╗ ██████╗
  ████╗  ██║██╔════╝╚██╗██╔╝██╔═══██╗
  ██╔██╗ ██║█████╗   ╚███╔╝ ██║   ██║
  ██║╚██╗██║██╔══╝   ██╔██╗ ██║   ██║
  ██║ ╚████║███████╗██╔╝ ██╗╚██████╔╝
  ╚═╝  ╚═══╝╚══════╝╚═╝  ╚═╝ ╚═════╝

  agent framework · installer · https://lordmacu.github.io/nexo-rs/
─────────────────────────────────────────────────────────────
EOF
}

have() { command -v "$1" >/dev/null 2>&1; }

# --- pick the release asset for this host -----------------------------
detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)
            case "$arch" in
                x86_64|amd64)   echo "x86_64-unknown-linux-musl" ;;
                aarch64|arm64)  echo "aarch64-unknown-linux-musl" ;;
                *) echo "" ;;
            esac ;;
        Darwin)
            case "$arch" in
                x86_64|amd64)   echo "x86_64-apple-darwin" ;;
                arm64|aarch64)  echo "aarch64-apple-darwin" ;;
                *) echo "" ;;
            esac ;;
        # Git Bash / MSYS2 / Cygwin shells on Windows.
        MINGW*|MSYS*|CYGWIN*|Windows_NT)
            case "$arch" in
                x86_64|amd64)   echo "x86_64-pc-windows-msvc" ;;
                *) echo "" ;;
            esac ;;
        *) echo "" ;;
    esac
}

# Archive extension + binary name for a target triple. Windows ships a
# `.zip` with `nexo.exe`; everything else a `.tar.xz` with `nexo`.
asset_ext()  { case "$1" in *windows*) echo "zip" ;; *) echo "tar.xz" ;; esac; }
asset_bin()  { case "$1" in *windows*) echo "nexo.exe" ;; *) echo "nexo" ;; esac; }

# --- where does the binary go ----------------------------------------
resolve_install_dir() {
    if [ -n "$INSTALL_DIR" ]; then echo "$INSTALL_DIR"; return; fi
    if have cargo; then
        echo "${CARGO_HOME:-$HOME/.cargo}/bin"; return
    fi
    echo "$HOME/.local/bin"
}

download() {
    # download <url> <dest>
    if have curl; then
        curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error -o "$2" "$1"
    elif have wget; then
        wget -q -O "$2" "$1"
    else
        echo "error: need curl or wget on PATH" >&2; return 1
    fi
}

sha256_of() {
    if have sha256sum; then sha256sum "$1" | awk '{print $1}'
    elif have shasum; then shasum -a 256 "$1" | awk '{print $1}'
    else echo ""; fi
}

install_from_binary() {
    local target ext binname tarball url tmp bin dir
    target="$(detect_target)"
    [ -n "$target" ] || return 1
    ext="$(asset_ext "$target")"
    binname="$(asset_bin "$target")"

    tarball="nexo-rs-${target}.${ext}"
    url="${RELEASES}/latest/download/${tarball}"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN

    echo "→ downloading ${tarball} from the latest release …"
    download "$url" "$tmp/$tarball" || { echo "  download failed" >&2; return 1; }

    # sha256 sidecar — verify if we can; warn (don't fail) if no hasher.
    if download "${url}.sha256" "$tmp/${tarball}.sha256" 2>/dev/null; then
        local want got
        want="$(awk '{print $1}' "$tmp/${tarball}.sha256")"
        got="$(sha256_of "$tmp/$tarball")"
        if [ -n "$got" ] && [ "$want" != "$got" ]; then
            echo "error: sha256 mismatch for ${tarball}" >&2
            echo "  expected $want" >&2
            echo "  got      $got"  >&2
            return 2
        fi
        [ -n "$got" ] && echo "✓ sha256 verified"
    fi

    echo "→ extracting …"
    case "$ext" in
        zip)
            if have unzip; then
                unzip -q "$tmp/$tarball" -d "$tmp" || { echo "error: unzip failed" >&2; return 2; }
            elif tar -xf "$tmp/$tarball" -C "$tmp" 2>/dev/null; then
                : # bsdtar (Windows 10+ `tar.exe`, macOS) handles zip
            else
                echo "error: cannot extract .zip (need 'unzip' or a zip-aware 'tar')" >&2; return 2
            fi ;;
        tar.xz)
            if ! tar -xJf "$tmp/$tarball" -C "$tmp" 2>/dev/null; then
                have xz || { echo "error: cannot extract .tar.xz (need 'xz' or an xz-aware tar)" >&2; return 2; }
                xz -dc "$tmp/$tarball" | tar -x -C "$tmp" || { echo "error: extract failed" >&2; return 2; }
            fi ;;
    esac
    bin="$(find "$tmp" -maxdepth 3 -type f -name "$binname" | head -n1)"
    [ -n "$bin" ] || { echo "error: no '$binname' inside the archive" >&2; return 2; }

    dir="$(resolve_install_dir)"
    mkdir -p "$dir"
    install -m 0755 "$bin" "$dir/$binname" 2>/dev/null \
        || { cp "$bin" "$dir/$binname" && chmod 0755 "$dir/$binname" 2>/dev/null || true; }
    echo "✓ installed: $dir/$binname  ($("$dir/$binname" --version 2>/dev/null || echo nexo))"

    case ":$PATH:" in
        *":$dir:"*) ;;
        *) echo
           echo "  ⚠ $dir is not on your PATH. Add it:"
           echo "      echo 'export PATH=\"$dir:\$PATH\"' >> ~/.bashrc   # or ~/.zshrc"
           echo "      export PATH=\"$dir:\$PATH\"" ;;
    esac
    return 0
}

install_from_cargo() {
    have cargo || {
        cat >&2 <<EOF
error: no pre-built binary for $(uname -s)/$(uname -m) and \`cargo\` is not on PATH.

Either install the Rust toolchain —
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
then re-run this script — or grab a package directly:
    .deb / .rpm / Termux .deb   ${RELEASES}/latest
    container                   docker pull ghcr.io/lordmacu/nexo-rs:latest
EOF
        return 3
    }
    echo "→ cargo install nexo-rs   (from crates.io)"
    if cargo install nexo-rs; then return 0; fi
    echo "  crates.io path failed — trying the git source …" >&2
    if cargo install --git "https://github.com/${REPO}" nexo-rs; then return 0; fi
    cat >&2 <<EOF
error: cargo install failed.
  Missing system deps (libssl, pkg-config, clang)? see
    https://github.com/${REPO}#from-source
  Old Rust? \`rustup update stable\`
EOF
    return 3
}

# --- where the `nexo` binary landed ----------------------------------
# Prefer the one we just installed (resolve_install_dir/nexo) over
# whatever happens to be first on PATH.
nexo_bin() {
    local d; d="$(resolve_install_dir)"
    [ -x "$d/nexo.exe" ] && { echo "$d/nexo.exe"; return 0; }
    [ -x "$d/nexo" ]     && { echo "$d/nexo"; return 0; }
    command -v nexo.exe 2>/dev/null && return 0
    command -v nexo     2>/dev/null && return 0
    return 1
}

# Install one plugin: try the fast prebuilt path first (a GitHub
# Release tarball matching the daemon's target triple, seconds), and
# fall back to building from crates.io (`cargo install`, any arch but
# needs the Rust toolchain). Best-effort — a failure never aborts.
install_one_plugin() {
    local nexo="$1" id="$2"
    echo "→ ${id}"
    if "$nexo" plugin install "lordmacu/${id}" </dev/null; then return 0; fi
    if have cargo; then
        echo "  (no prebuilt tarball for this arch — building from crates.io …)"
        cargo install "${id}" </dev/null && return 0
    fi
    echo "  ⚠ skipped ${id} — retry later:  nexo plugin install lordmacu/${id}   (or: cargo install ${id})" >&2
    return 0
}

# Install the bundled plugins + a default persona. Best-effort.
install_plugins() {
    [ "$INSTALL_PLUGINS" -eq 1 ] || return 0
    local nexo p
    nexo="$(nexo_bin)" || { echo "  ⚠ can't find the freshly-installed 'nexo' — skipping plugins" >&2; return 0; }

    echo
    echo "─────────────────────────────────────────────────────────────"
    echo "  Installing bundled plugins + persona"
    echo "─────────────────────────────────────────────────────────────"

    # Channel plugins + the admin web UI. Each one: prebuilt tarball
    # (fast) → crates.io build (any arch, needs Rust) → warn.
    for p in $PLUGINS nexo-plugin-admin; do
        install_one_plugin "$nexo" "$p"
    done

    # Default persona pack — a ready-to-run agent on first boot.
    # Personas are noarch YAML packs; the GitHub Release tarball works
    # everywhere, so no crates.io fallback is needed.
    if [ -n "$PERSONA" ]; then
        echo "→ persona ${PERSONA}"
        "$nexo" persona install "$PERSONA" </dev/null \
            || echo "  ⚠ skipped persona ${PERSONA} — retry later:  nexo persona install ${PERSONA}" >&2
    fi
}

next_steps() {
    cat <<'EOF'

Next:
  1. Boot the daemon — zero config required:
       nexo            # foreground
       nexo start      # background (nexo stop / nexo restart to manage it)

  2. Open the admin web UI (auto-installs nexo-plugin-admin if missing):
       nexo admin --open
       nexo admin --tunnel    # + a free public Cloudflare URL

  3. (Optional) Scaffold 19 documented sample YAMLs:
       nexo init

  More plugins / personas / re-run a skipped one:
       nexo plugin install lordmacu/nexo-plugin-whatsapp
       # also: nexo-plugin-{telegram,email,browser}
       nexo persona install lordmacu/nexo-persona-cody

  Update later:
       nexo update

Docs: https://lordmacu.github.io/nexo-rs/
EOF
}

main() {
    banner
    if [ "$FROM_SOURCE" -eq 1 ]; then
        install_from_cargo || exit $?
    else
        if install_from_binary; then :; else
            rc=$?
            if [ "$rc" -eq 2 ]; then exit 2; fi   # download/extract/sha256 hard error
            echo "→ no pre-built binary for this platform — falling back to cargo …"
            install_from_cargo || exit $?
        fi
    fi
    install_plugins
    next_steps
}

main "$@"
