#!/usr/bin/env bash
# Non-Docker bootstrap for nexo-rs. Works on Linux, macOS, and Termux.
#
# Verifies prerequisites, installs NATS (native or container), creates the
# runtime directory layout, stages example configs, and builds the agent
# binary. Every step is idempotent — re-run safely.
#
# Usage:
#     ./scripts/bootstrap.sh [--nats=native|docker|skip] [--skip-build]
#                            [--skip-setup] [--yes]
#
#   --nats=native   install nats-server to /usr/local/bin (default on Linux/mac)
#                   or $PREFIX/bin (Termux)
#   --nats=docker   run nats:2.10-alpine as a detached container (requires docker)
#   --nats=skip     don't touch NATS (BYO broker). Default on Termux, where
#                   the recommended setup is broker.type: local in-process.
#   --skip-build    don't run `cargo build --release`
#   --skip-setup    don't launch `agent setup` at the end
#   --yes           auto-confirm sudo + install prompts when possible
#
# Run from the repository root.

set -euo pipefail

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

NATS_MODE="auto"
SKIP_BUILD="0"
SKIP_SETUP="0"
ASSUME_YES="0"
NATS_VERSION="2.10.20"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

for arg in "$@"; do
  case "$arg" in
    --nats=native|--nats=docker|--nats=skip)
      NATS_MODE="${arg#--nats=}"
      ;;
    --nats=auto)
      NATS_MODE="auto"
      ;;
    --skip-build)
      SKIP_BUILD="1"
      ;;
    --skip-setup)
      SKIP_SETUP="1"
      ;;
    --yes|-y)
      ASSUME_YES="1"
      ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown flag: $arg" >&2
      exit 2
      ;;
  esac
done

cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# UI helpers
# ---------------------------------------------------------------------------

BOLD=$'\033[1m'; RESET=$'\033[0m'; GREEN=$'\033[32m'; YEL=$'\033[33m'; RED=$'\033[31m'
say()  { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
ok()   { printf '%s    ok%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%s    warn%s %s\n' "$YEL"  "$RESET" "$*"; }
err()  { printf '%s    error%s %s\n' "$RED" "$RESET" "$*"; }

confirm() {
  local prompt="$1"
  if [[ "$ASSUME_YES" == "1" ]]; then return 0; fi
  printf '%s [y/N] ' "$prompt"
  read -r ans
  [[ "$ans" == "y" || "$ans" == "Y" ]]
}

have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------------------
# OS detection
# ---------------------------------------------------------------------------

detect_os() {
  # Termux sits on top of Linux but needs a different install surface —
  # no sudo, no /usr/local/bin, no systemd, `pkg` instead of `apt`.
  if [[ -n "${TERMUX_VERSION:-}" ]] || [[ "${PREFIX:-}" == */com.termux/* ]]; then
    echo "termux"
    return
  fi
  case "$(uname -s)" in
    Linux*)   echo "linux"  ;;
    Darwin*)  echo "macos"  ;;
    *)        echo "other"  ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64)   echo "amd64" ;;
    aarch64|arm64)  echo "arm64" ;;
    *)              echo "other" ;;
  esac
}

OS="$(detect_os)"
ARCH="$(detect_arch)"

# ---------------------------------------------------------------------------
# 1. Check prerequisites
# ---------------------------------------------------------------------------

say "Checking prerequisites"

missing=()

if ! have git; then missing+=("git"); fi
if ! have curl; then missing+=("curl"); fi
if ! have tar; then missing+=("tar"); fi
if ! have sqlite3; then warn "sqlite3 CLI not found (runtime only needs libsqlite3; safe to ignore)"; fi

if [[ "${#missing[@]}" -gt 0 ]]; then
  err "missing core tools: ${missing[*]}"
  case "$OS" in
    linux)  err "try: sudo apt install -y ${missing[*]}" ;;
    macos)  err "try: brew install ${missing[*]}" ;;
    termux) err "try: pkg install -y ${missing[*]}" ;;
  esac
  exit 1
fi

# Rust
if ! have cargo; then
  warn "rust / cargo not installed"
  if [[ "$OS" == "termux" ]]; then
    # rustup doesn't exist for Termux; `pkg install rust` ships a native toolchain.
    if confirm "install rust via 'pkg install rust' now?"; then
      pkg install -y rust
      ok "rust installed via pkg"
    else
      err "cargo is required; rerun after: pkg install rust"
      exit 1
    fi
  else
    if confirm "install rust via rustup now?"; then
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
      # shellcheck disable=SC1091
      source "$HOME/.cargo/env"
      ok "rust installed"
    else
      err "cargo is required; rerun after installing rust"
      exit 1
    fi
  fi
else
  ok "rust $(rustc --version)"
fi

# rustup isn't available on Termux; component install is a no-op there.
if [[ "$OS" != "termux" ]]; then
  rustup component add rustfmt clippy >/dev/null 2>&1 || true
fi

# ---------------------------------------------------------------------------
# 2. NATS
# ---------------------------------------------------------------------------

install_nats_native() {
  local url target install_dir
  # NATS releases ship static Go binaries, so the linux-arm64 archive
  # works unchanged in Termux on aarch64 phones.
  if [[ "$OS" == "linux" && "$ARCH" == "amd64" ]]; then
    url="https://github.com/nats-io/nats-server/releases/download/v${NATS_VERSION}/nats-server-v${NATS_VERSION}-linux-amd64.tar.gz"
  elif [[ "$OS" == "linux" && "$ARCH" == "arm64" ]]; then
    url="https://github.com/nats-io/nats-server/releases/download/v${NATS_VERSION}/nats-server-v${NATS_VERSION}-linux-arm64.tar.gz"
  elif [[ "$OS" == "termux" && "$ARCH" == "arm64" ]]; then
    url="https://github.com/nats-io/nats-server/releases/download/v${NATS_VERSION}/nats-server-v${NATS_VERSION}-linux-arm64.tar.gz"
  elif [[ "$OS" == "termux" && "$ARCH" == "amd64" ]]; then
    url="https://github.com/nats-io/nats-server/releases/download/v${NATS_VERSION}/nats-server-v${NATS_VERSION}-linux-amd64.tar.gz"
  elif [[ "$OS" == "macos" ]]; then
    if have brew; then
      brew install nats-server
      return 0
    fi
    url="https://github.com/nats-io/nats-server/releases/download/v${NATS_VERSION}/nats-server-v${NATS_VERSION}-darwin-${ARCH}.tar.gz"
  else
    err "no prebuilt NATS for $OS/$ARCH; install manually and re-run with --nats=skip"
    return 1
  fi

  say "Downloading nats-server v${NATS_VERSION} for $OS/$ARCH"
  local tmp
  tmp="$(mktemp -d)"
  curl -L -o "$tmp/nats.tar.gz" "$url"
  tar -xzf "$tmp/nats.tar.gz" -C "$tmp"
  target="$(find "$tmp" -type f -name 'nats-server' | head -1)"
  if [[ -z "$target" ]]; then
    err "nats-server binary not found in archive"
    return 1
  fi
  if [[ "$OS" == "termux" ]]; then
    install_dir="${PREFIX:-/data/data/com.termux/files/usr}/bin"
  else
    install_dir="/usr/local/bin"
  fi
  if [[ "$OS" != "termux" ]] && have sudo; then
    sudo install -m 0755 "$target" "$install_dir/nats-server"
  else
    install -m 0755 "$target" "$install_dir/nats-server" 2>/dev/null || \
    install -m 0755 "$target" "$HOME/.local/bin/nats-server" 2>/dev/null || {
      err "cannot install nats-server — no sudo and no writable bindir"
      return 1
    }
  fi
  rm -rf "$tmp"
  ok "nats-server installed ($(nats-server --version 2>&1 | head -1))"
}

install_nats_docker() {
  if ! have docker; then
    err "--nats=docker requested but docker CLI is missing"
    return 1
  fi
  if docker ps --format '{{.Names}}' | grep -q '^nexo-nats$'; then
    ok "nexo-nats container already running"
    return 0
  fi
  if docker ps -a --format '{{.Names}}' | grep -q '^nexo-nats$'; then
    docker start nexo-nats >/dev/null
    ok "started existing nexo-nats container"
    return 0
  fi
  docker run -d --name nexo-nats --restart unless-stopped \
    -p 4222:4222 -p 8222:8222 nats:2.10-alpine >/dev/null
  ok "nexo-nats container running on :4222"
}

case "$NATS_MODE" in
  auto)
    if have nats-server; then
      ok "nats-server already installed ($(nats-server --version 2>&1 | head -1))"
    elif [[ "$OS" == "termux" ]]; then
      # On Termux, default to the in-process local broker — no NATS needed.
      # The operator can opt into --nats=native if they want a real broker.
      warn "Termux detected — defaulting to local broker (broker.type: local)"
      warn "re-run with --nats=native to download a NATS binary instead"
      NATS_MODE="skip"
    else
      install_nats_native || exit 1
    fi
    ;;
  native)
    install_nats_native || exit 1
    ;;
  docker)
    install_nats_docker || exit 1
    ;;
  skip)
    warn "skipping NATS install — bring your own broker at nats://localhost:4222"
    ;;
esac

# ---------------------------------------------------------------------------
# 3. Runtime directories
# ---------------------------------------------------------------------------

say "Preparing runtime directories"

for dir in data data/queue data/workspace data/media data/transcripts secrets; do
  if [[ ! -d "$dir" ]]; then
    mkdir -p "$dir"
    ok "created $dir"
  else
    ok "$dir exists"
  fi
done

# Wire git hooks so the docs-sync + fmt + clippy + tests gate runs
# on every commit. Idempotent: git config is a rewrite.
if [[ -d .githooks ]] && have git && [[ -d .git ]]; then
  current="$(git config --get core.hooksPath 2>/dev/null || true)"
  if [[ "$current" != ".githooks" ]]; then
    git config core.hooksPath .githooks
    ok "git hooks wired (core.hooksPath = .githooks)"
  else
    ok "git hooks already wired"
  fi
fi

chmod 700 secrets 2>/dev/null || true

# Termux-specific hint: the credentials gauntlet's mode-check doesn't
# map cleanly onto Android filesystem perms. Tell the operator to
# disable it.
if [[ "$OS" == "termux" ]]; then
  if ! grep -q "CHAT_AUTH_SKIP_PERM_CHECK" ~/.profile 2>/dev/null; then
    warn "Termux detected — add 'export CHAT_AUTH_SKIP_PERM_CHECK=1' to your shell init"
    warn "  (Android's filesystem permission model triggers false-positive gauntlet errors)"
  fi
fi

# ---------------------------------------------------------------------------
# 4. Stage gitignored example configs if missing
# ---------------------------------------------------------------------------

say "Staging example configs"

if compgen -G "config/agents.d/*.example.yaml" >/dev/null; then
  for ex in config/agents.d/*.example.yaml; do
    base="${ex%.example.yaml}.yaml"
    if [[ ! -f "$base" ]]; then
      if confirm "copy $(basename "$ex") -> $(basename "$base") (gitignored)?"; then
        cp "$ex" "$base"
        ok "staged $(basename "$base")"
      fi
    fi
  done
fi

# ---------------------------------------------------------------------------
# 5. Build
# ---------------------------------------------------------------------------

if [[ "$SKIP_BUILD" == "1" ]]; then
  warn "--skip-build: not running cargo build"
else
  say "Building agent (cargo build --release)"
  cargo build --release --bin agent
  ok "binary at ./target/release/agent"
fi

# ---------------------------------------------------------------------------
# 6. Optional: launch the setup wizard
# ---------------------------------------------------------------------------

if [[ "$SKIP_SETUP" == "1" ]]; then
  warn "--skip-setup: not launching agent setup"
else
  if [[ -x "./target/release/agent" ]]; then
    if confirm "launch interactive setup wizard now?"; then
      ./target/release/agent setup
    else
      warn "skipping setup. Re-run with: ./target/release/agent setup"
    fi
  fi
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

if [[ "$OS" == "termux" ]]; then
  cat <<EOF

${BOLD}Bootstrap complete (Termux mode).${RESET}

Next steps:

  1. (Optional) Keep the CPU awake even with the screen off:
       termux-wake-lock

  2. Make sure your broker is ready:
     - Local mode (default): set broker.type: local in config/broker.yaml
     - Native NATS:          nats-server -js &

  3. Run the agent:
       export CHAT_AUTH_SKIP_PERM_CHECK=1
       ./target/release/agent --config ./config

  4. Verify:
       curl localhost:8080/ready
       curl localhost:9090/metrics

  5. Background / auto-start options (termux-services, Termux:Boot)
     live in docs/src/getting-started/install-termux.md

EOF
else
  cat <<EOF

${BOLD}Bootstrap complete.${RESET}

Next steps:

  1. Make sure NATS is running:
       nats-server -js           # foreground
       # or: docker start nexo-nats

  2. Run the agent:
       ./target/release/agent --config ./config

  3. Verify health:
       curl localhost:8080/ready
       curl localhost:9090/metrics

  4. For service-manager installs (systemd / launchd) see
     docs/src/getting-started/install-native.md

EOF
fi
