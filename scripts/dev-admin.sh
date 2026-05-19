#!/usr/bin/env bash
# dev-admin.sh — local dev cycle: daemon + admin plugin (+ optional
# whatsapp / telegram subprocess plugins, optional cloudflared
# tunnel).
#
# Phase 81.30 follow-up #2 (SoT único): the script no longer ships
# heredoc'd `plugin.toml` content. Each plugin's manifest is read
# directly from its repo's `nexo-plugin.toml`, then the script
# rewrites ONLY the `[plugin.entrypoint] command = "..."` line so
# the dev-state binary path matches the local rebuild. Every other
# field (id, version, pairing, extends, capabilities, instructions)
# stays consistent with what the binary's `include_str!` ships.
#
#   nexo-rs daemon  ──spawns──▶  admin plugin   (extension, AdminRPC)
#                           ├──▶  whatsapp plugin (subprocess)
#                           └──▶  telegram plugin (subprocess)
#
# Usage:
#   ./scripts/dev-admin.sh                      # build + launch one-shot
#   ./scripts/dev-admin.sh --watch              # cargo-watch auto-rebuild
#   ./scripts/dev-admin.sh --reset-state        # purge .dev-state/
#   ./scripts/dev-admin.sh --no-tunnel          # loopback only
#   ./scripts/dev-admin.sh --no-plugins         # skip whatsapp/telegram
#   ./scripts/dev-admin.sh --with-cody          # install Cody persona

set -euo pipefail

# ── Paths ──────────────────────────────────────────────────────
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADMIN_REPO="${ADMIN_REPO:-/home/familia/chat/nexo-rs-plugin-admin}"
WA_REPO="${WA_REPO:-/home/familia/chat/nexo-rs-plugin-whatsapp}"
TG_REPO="${TG_REPO:-/home/familia/chat/nexo-rs-plugin-telegram}"
PERSONA_CODY_REPO="${PERSONA_CODY_REPO:-/home/familia/chat/nexo-persona-cody}"
STATE="$ROOT/.dev-state"
CONFIG="$STATE/config"
PLUGINS_DIR="$STATE/plugins"
LOG_DIR="$ROOT/.logs"
PIDS=()

# ── Defaults ───────────────────────────────────────────────────
ADMIN_PORT="${ADMIN_PORT:-18000}"
TOKEN="${TOKEN:-dev-$(date +%s)}"
RESET_STATE=0
USE_TUNNEL=1
INSTALL_PLUGINS=1
WITH_CODY=0
WATCH=0

for arg in "$@"; do
  case "$arg" in
    --reset-state)  RESET_STATE=1 ;;
    --tunnel)       USE_TUNNEL=1 ;;
    --no-tunnel)    USE_TUNNEL=0 ;;
    --no-plugins)   INSTALL_PLUGINS=0 ;;
    --with-cody)    WITH_CODY=1 ;;
    --watch)        WATCH=1 ;;
    --token=*)      TOKEN="${arg#--token=}" ;;
    --help|-h)
      sed -n '2,30p' "$0"
      exit 0
      ;;
    *) echo "unknown flag: $arg" >&2; exit 1 ;;
  esac
done

mkdir -p "$LOG_DIR"
> "$LOG_DIR/daemon.log"
> "$LOG_DIR/tunnel.log"
> "$LOG_DIR/watch-rust.log"
> "$LOG_DIR/watch-admin.log"
> "$LOG_DIR/watch-frontend.log"

# ── Cleanup trap ───────────────────────────────────────────────
cleanup() {
  echo
  echo "[stop] tearing down..."
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  fuser -k "$ADMIN_PORT/tcp" 2>/dev/null || true
  pkill -f "cloudflared tunnel --url http://127.0.0.1:$ADMIN_PORT" 2>/dev/null || true
  exit 0
}
trap cleanup INT TERM EXIT

# ── 0. reset state ─────────────────────────────────────────────
[[ $RESET_STATE -eq 1 ]] && rm -rf "$STATE"
mkdir -p "$CONFIG" "$PLUGINS_DIR"

# Skills symlink — FsSkillsStore scans <config>/skills/__global__
mkdir -p "$CONFIG/skills"
[[ ! -e "$CONFIG/skills/__global__" ]] && ln -s "$ROOT/skills" "$CONFIG/skills/__global__"

# ── 1. build daemon ────────────────────────────────────────────
echo "[daemon] cargo build --profile release-fast --bin nexo"
( cd "$ROOT" && cargo build --profile release-fast --bin nexo )
DAEMON_BIN="$ROOT/target/release-fast/nexo"

# ── 2. build admin plugin ──────────────────────────────────────
echo "[admin] frontend + cargo build --release"
( cd "$ADMIN_REPO/frontend" && npm run build )
( cd "$ADMIN_REPO" && cargo build --release )
ADMIN_BIN="$ADMIN_REPO/target/release/nexo-plugin-admin"

# ── 2b. build subprocess plugins ───────────────────────────────
if [[ $INSTALL_PLUGINS -eq 1 ]]; then
  if [[ ! -x "$WA_REPO/target/release/nexo-plugin-whatsapp" || $RESET_STATE -eq 1 ]]; then
    echo "[whatsapp] cargo build --release"
    ( cd "$WA_REPO" && cargo build --release )
  fi
  if [[ ! -x "$TG_REPO/target/release/nexo-plugin-telegram" || $RESET_STATE -eq 1 ]]; then
    echo "[telegram] cargo build --release"
    ( cd "$TG_REPO" && cargo build --release )
  fi
fi

# ── 3. install admin plugin as extension ───────────────────────
# Distinct from subprocess plugins: admin lives under
# $STATE/extensions/admin and is wired via extensions.yaml so it
# gets a full AdminRpcDispatcher.
rm -rf "$PLUGINS_DIR/admin"
ADMIN_EXT_DIR="$STATE/extensions/admin"
mkdir -p "$ADMIN_EXT_DIR/bin"
cp -f "$ADMIN_BIN" "$ADMIN_EXT_DIR/bin/nexo-plugin-admin"

ADMIN_VERSION=$(grep -m1 '^version\s*=' "$ADMIN_REPO/Cargo.toml" | sed -E 's/.*"(.+)".*/\1/')
ADMIN_VERSION="${ADMIN_VERSION:-0.1.0}"
# Admin uses the extension-format manifest (different schema than
# subprocess plugins). Synthesised inline because the admin repo
# doesn't ship a top-level plugin.toml in this format.
cat > "$ADMIN_EXT_DIR/plugin.toml" <<TOML
[plugin]
id          = "nexo-plugin-admin"
version     = "$ADMIN_VERSION"
name        = "Nexo Admin"
description = "Admin UI for nexo-rs."

[transport]
kind    = "stdio"
command = "./bin/nexo-plugin-admin"

[capabilities.admin]
required = [
    "agents_crud", "skills_crud", "llm_keys_crud", "channels_crud",
    "pairing_initiate", "transcripts_read", "operator_intervention",
    "escalations_read", "escalations_resolve", "audit_read",
    "memory_query", "plugin_doctor", "tenants_crud", "mcp_crud",
]
optional = [
    "auth_rotate", "secrets_write", "agent_events_subscribe_all",
    "credentials_crud", "plugin_restart",
]

[capabilities.http_server]
port        = $ADMIN_PORT
bind        = "127.0.0.1"
token_env   = "NEXO_ADMIN_TOKEN"
health_path = "/healthz"
TOML

# ── 3b. install subprocess plugins (whatsapp + telegram) ───────
# SoT único: copy the repo's `nexo-plugin.toml` verbatim, then
# rewrite ONLY the `[plugin.entrypoint] command` line so it
# matches the dev-state binary path. Every other field (pairing,
# extends, capabilities, instructions) comes straight from the
# repo manifest — no heredoc to drift against.
install_subprocess_plugin() {
  local plugin_id="$1"
  local repo_dir="$2"
  local bin_name="$3"
  local repo_manifest="$repo_dir/nexo-plugin.toml"
  local plugin_dir="$PLUGINS_DIR/$plugin_id"

  [[ -f "$repo_manifest" ]] || { echo "::error::$repo_manifest missing" >&2; exit 1; }
  mkdir -p "$plugin_dir"
  cp -f "$repo_dir/target/release/$bin_name" "$plugin_dir/$bin_name"
  chmod +x "$plugin_dir/$bin_name"

  # Copy manifest then rewrite the entrypoint command. The repo
  # manifest typically ships `[plugin.entrypoint] command = "./<bin>"`
  # (relative); the daemon needs an absolute path under dev-state.
  cp -f "$repo_manifest" "$plugin_dir/plugin.toml"
  # Strip any existing [plugin.entrypoint] block and append a
  # dev-state-pointing one. awk handles the multi-line strip.
  awk '
    BEGIN { skip = 0 }
    /^\[plugin\.entrypoint\]/ { skip = 1; next }
    /^\[/ && skip { skip = 0 }
    !skip { print }
  ' "$plugin_dir/plugin.toml" > "$plugin_dir/plugin.toml.stripped"
  mv "$plugin_dir/plugin.toml.stripped" "$plugin_dir/plugin.toml"
  cat >> "$plugin_dir/plugin.toml" <<TOML

# Phase 81.30 follow-up #2 — entrypoint rewritten at install time
# to point at the dev-state binary; everything else above came
# straight from $repo_manifest.
[plugin.entrypoint]
command = "$plugin_dir/$bin_name"
TOML
  echo "[install] $plugin_dir/{$bin_name,plugin.toml}"
}

if [[ $INSTALL_PLUGINS -eq 1 ]]; then
  install_subprocess_plugin "whatsapp" "$WA_REPO" "nexo-plugin-whatsapp"
  install_subprocess_plugin "telegram" "$TG_REPO" "nexo-plugin-telegram"
fi

# ── 3c. install Cody persona (optional) ────────────────────────
# Personas are TEMPLATES, not live agents. We provision the
# persona pack at the framework's standard install_root so
# `persona discovery` registers it; we do NOT copy its
# `agents.d/cody.yaml` into `<config>/agents.d/` because the
# daemon would then merge it into `cfg.agents` and treat the
# template as an active agent. The admin wizard reads the
# template via `nexo/admin/personas/template_get` at create-
# agent time.
if [[ $WITH_CODY -eq 1 && -d "$PERSONA_CODY_REPO" ]]; then
  echo "[persona/cody] installing..."
  mkdir -p "$STATE/secrets" "$CONFIG/data/workspace/cody"
  TOKEN_FILE="$STATE/secrets/cody_nexo_bot_telegram_token.txt"
  if [[ ! -f "$TOKEN_FILE" ]] || [[ ! -s "$TOKEN_FILE" ]]; then
    echo "placeholder-not-a-real-token" > "$TOKEN_FILE"
    chmod 600 "$TOKEN_FILE"
  fi
  if [[ $RESET_STATE -eq 1 ]] || [[ ! -f "$CONFIG/data/workspace/cody/PHASES.md" ]]; then
    cp -r "$PERSONA_CODY_REPO/data/workspace/cody/." "$CONFIG/data/workspace/cody/"
  fi
fi

# ── 4. seed YAML configs ───────────────────────────────────────
echo "[config] seeding $CONFIG/*.yaml"

cat > "$CONFIG/extensions.yaml" <<YAML
extensions:
  enabled: true
  search_paths:
    - "$STATE/extensions"
  disabled: []
  allowlist: []
  ignore_dirs: [target, .git, node_modules, dist, .build]
  max_depth: 3
  transport_defaults:
    nats:
      subject_prefix: ext
      call_timeout: 30s
      handshake_timeout: 10s
      heartbeat_interval: 15s
      heartbeat_grace_factor: 3
      shutdown_grace: 2s
  entries:
    nexo-plugin-admin:
      capabilities_grant:
        - agents_crud
        - skills_crud
        - llm_keys_crud
        - channels_crud
        - pairing_initiate
        - transcripts_read
        - operator_intervention
        - escalations_read
        - escalations_resolve
        - audit_read
        - memory_query
        - plugin_doctor
        - tenants_crud
        - mcp_crud
        - auth_rotate
        - secrets_write
        - agent_events_subscribe_all
        - credentials_crud
        - plugin_restart
schema_version: 11
YAML

cat > "$CONFIG/broker.yaml" <<YAML
broker:
  type: local
  url: ""
  persistence:
    enabled: true
    path: $STATE/data/broker.db
  limits:
    max_payload: 4MB
    max_pending: 10000
schema_version: 11
YAML

# llm.yaml + agents.yaml are operator-managed (admin UI / manual
# edits). Only seed empty defaults on first boot or after
# --reset-state to avoid wiping mid-session config.
if [[ ! -f "$CONFIG/llm.yaml" ]]; then
  cat > "$CONFIG/llm.yaml" <<'YAML'
providers: {}
schema_version: 11
YAML
fi

if [[ ! -f "$CONFIG/agents.yaml" ]]; then
  cat > "$CONFIG/agents.yaml" <<'YAML'
agents: []
schema_version: 11
YAML
fi

mkdir -p "$STATE/data"
cat > "$CONFIG/memory.yaml" <<YAML
short_term:
  max_history_turns: 50
  session_ttl: 24h
long_term:
  backend: sqlite
  sqlite:
    path: $STATE/data/memory.db
vector:
  enabled: false
schema_version: 11
YAML

# Plugin discovery + per-plugin configs.
mkdir -p "$CONFIG/plugins"
cat > "$CONFIG/plugins/discovery.yaml" <<YAML
discovery:
  search_paths:
    - "$PLUGINS_DIR"
  disabled: []
  allowlist: []
  follow_symlinks: false
schema_version: 11
YAML

# whatsapp.yaml is wizard-managed. The admin UI's
# `nexo/admin/pairing/start` fires the
# `WhatsappPersister.on_pair_success` hook which upserts the
# instance entry with the operator-chosen label. We previously
# seeded a default entry without an `instance:` field here for
# legacy single-account dev convenience; that default leaked
# back over the wizard's entry on every dev-admin restart and
# the plugin published on the wrong topic.

# telegram.yaml is wizard-managed (admin UI's
# `nexo/admin/credentials/register` calls TelegramPersister which
# upserts the instance entry). We previously seeded a `cody_nexo_bot`
# default here so the legacy persona could boot without any UI
# interaction; that default collided with the wizard's pair-and-
# create-agent flow (binding pointed at `nexotest1_bot`, telegram.yaml
# only had `cody_nexo_bot`, plugin published on the wrong topic).
# Now the wizard owns this file end-to-end.

mkdir -p "$CONFIG/personas"

# ── 5. free admin port ─────────────────────────────────────────
if fuser "$ADMIN_PORT/tcp" >/dev/null 2>&1; then
  fuser -k "$ADMIN_PORT/tcp" 2>/dev/null || true
  sleep 1
fi

# ── 5b. watch loops (optional) ─────────────────────────────────
if [[ $WATCH -eq 1 ]]; then
  if ! command -v cargo-watch >/dev/null 2>&1; then
    echo "::error::cargo-watch not installed. Install with:" >&2
    echo "  cargo install cargo-watch --locked" >&2
    exit 1
  fi
  echo "[watch] cargo-watch active for daemon + admin plugin + Vite frontend"

  # Admin plugin Rust watch → rebuild + copy + respawn child.
  ADMIN_REBUILD=$(cat <<HOOK
set -e
echo "[watch-admin] Rust change — rebuilding…"
cd "$ADMIN_REPO"
cargo build --release 2>&1 | tail -3
cp -f "$ADMIN_BIN" "$ADMIN_EXT_DIR/bin/nexo-plugin-admin"
chmod +x "$ADMIN_EXT_DIR/bin/nexo-plugin-admin"
pkill -f "$ADMIN_EXT_DIR/bin/nexo-plugin-admin" 2>/dev/null || true
HOOK
)
  (
    cd "$ADMIN_REPO"
    cargo watch --quiet \
      --watch "$ADMIN_REPO/src" \
      --watch "$ADMIN_REPO/Cargo.toml" \
      -s "$ADMIN_REBUILD"
  ) >"$LOG_DIR/watch-admin.log" 2>&1 &
  PIDS+=($!)

  # Frontend Vite watch — rebuild dist/ then re-embed into the
  # plugin binary + respawn child. Hash-based change detection
  # avoids spamming rebuilds on Vite incremental writes.
  (
    cd "$ADMIN_REPO/frontend"
    npx vite build --watch 2>&1
  ) >"$LOG_DIR/watch-frontend.log" 2>&1 &
  PIDS+=($!)
  (
    LAST=""
    while :; do
      sleep 2
      CUR=$(md5sum "$ADMIN_REPO/frontend/dist/index.html" 2>/dev/null | cut -d' ' -f1 || echo "")
      if [[ "$CUR" != "$LAST" && -n "$CUR" ]]; then
        LAST="$CUR"
        echo "[watch-frontend] dist changed @ \$(date +%H:%M:%S) — re-embedding"
        ( cd "$ADMIN_REPO" && cargo build --release 2>&1 | tail -2 ) && \
          cp -f "$ADMIN_BIN" "$ADMIN_EXT_DIR/bin/nexo-plugin-admin" && \
          chmod +x "$ADMIN_EXT_DIR/bin/nexo-plugin-admin" && \
          pkill -f "$ADMIN_EXT_DIR/bin/nexo-plugin-admin" 2>/dev/null || true
      fi
    done
  ) >>"$LOG_DIR/watch-frontend.log" 2>&1 &
  PIDS+=($!)

  # Daemon + SDK watch — rebuild + kill daemon → respawn loop
  # below relaunches it.
  DAEMON_REBUILD=$(cat <<HOOK
set -e
echo "[watch-rust] change detected — rebuilding nexo…"
cd "$ROOT"
cargo build --profile release-fast --bin nexo 2>&1 | tail -3
pkill -f "$DAEMON_BIN" 2>/dev/null || true
HOOK
)
  (
    cd "$ROOT"
    cargo watch --quiet \
      --watch "$ROOT/src" \
      --watch "$ROOT/Cargo.toml" \
      --watch "$ROOT/crates/core/src" \
      --watch "$ROOT/crates/microapp-sdk/src" \
      --watch "$ROOT/crates/microapp-http/src" \
      --watch "$ROOT/crates/config/src" \
      --watch "$ROOT/crates/broker/src" \
      --watch "$ROOT/crates/setup/src" \
      --watch "$ROOT/crates/auth/src" \
      --watch "$ROOT/crates/extensions/src" \
      --watch "$ROOT/crates/tool-meta/src" \
      --watch "$ROOT/crates/plugin-manifest/src" \
      -s "$DAEMON_REBUILD"
  ) >"$LOG_DIR/watch-rust.log" 2>&1 &
  PIDS+=($!)
fi

# ── 6. launch daemon ───────────────────────────────────────────
echo "[daemon] starting (config=$CONFIG, admin_port=$ADMIN_PORT)"
pkill -f "nexo.*--config $CONFIG" 2>/dev/null || true
sleep 0.5

if [[ $WATCH -eq 1 ]]; then
  # Respawn loop — relaunches daemon when watch hook kills it.
  (
    cd "$STATE"
    while :; do
      NEXO_ADMIN_TOKEN="$TOKEN" "$DAEMON_BIN" --config "$CONFIG" || true
      echo "[daemon-respawn] exited @ $(date +%H:%M:%S) — relaunching in 1s"
      sleep 1
    done
  ) >"$LOG_DIR/daemon.log" 2>&1 &
  PIDS+=($!)
else
  (
    cd "$STATE"
    NEXO_ADMIN_TOKEN="$TOKEN" "$DAEMON_BIN" --config "$CONFIG"
  ) >"$LOG_DIR/daemon.log" 2>&1 &
  PIDS+=($!)
fi

# ── 7. wait for admin HTTP ─────────────────────────────────────
echo "[wait] admin HTTP on $ADMIN_PORT (≤30s)..."
for i in {1..60}; do
  if (echo > "/dev/tcp/127.0.0.1/$ADMIN_PORT") >/dev/null 2>&1; then
    echo "[wait] admin HTTP up"
    break
  fi
  sleep 0.5
  if [[ $i -eq 60 ]]; then
    echo "[wait] FAILED — admin HTTP did not bind" >&2
    tail -60 "$LOG_DIR/daemon.log" >&2
    exit 1
  fi
done

# ── 8. cloudflared tunnel (optional) ───────────────────────────
TUNNEL_URL=""
if [[ $USE_TUNNEL -eq 1 ]] && command -v cloudflared >/dev/null 2>&1; then
  echo "[tunnel] starting cloudflared → :$ADMIN_PORT"
  cloudflared tunnel --no-autoupdate \
      --url "http://127.0.0.1:$ADMIN_PORT" \
      >"$LOG_DIR/tunnel.log" 2>&1 &
  PIDS+=($!)
  for i in {1..40}; do
    TUNNEL_URL=$(grep -oE "https://[a-z0-9-]+\.trycloudflare\.com" "$LOG_DIR/tunnel.log" | head -1 || true)
    [[ -n "$TUNNEL_URL" ]] && break
    sleep 0.5
  done
fi

# ── 9. summary ─────────────────────────────────────────────────
ADMIN_PASSWORD=""
for i in {1..20}; do
  ADMIN_PASSWORD=$(grep -oP 'admin password:\s*\K\S+' "$LOG_DIR/daemon.log" | tail -1 || true)
  [[ -n "$ADMIN_PASSWORD" ]] && break
  sleep 0.5
done

cat <<EOF

═══════════════════════════════════════════════════════════════
  nexo-rs daemon + admin plugin LIVE
═══════════════════════════════════════════════════════════════

  Admin UI (local)         →  http://127.0.0.1:$ADMIN_PORT
EOF
[[ -n "$TUNNEL_URL" ]] && echo "  Admin UI (public)        →  $TUNNEL_URL"
cat <<EOF
  Admin password           →  ${ADMIN_PASSWORD:-"(buscando en logs...)"}
  Bearer token             →  $TOKEN

  State dir                →  $STATE
  Daemon log               →  $LOG_DIR/daemon.log
EOF
if [[ $WATCH -eq 1 ]]; then
  cat <<EOF

  Watch logs:
    daemon + SDK    →  $LOG_DIR/watch-rust.log
    admin plugin    →  $LOG_DIR/watch-admin.log
    frontend Vite   →  $LOG_DIR/watch-frontend.log
EOF
fi
cat <<EOF

  Ctrl-C para parar todo.
═══════════════════════════════════════════════════════════════
EOF

wait
