# Termux (Android) install

Run nexo-rs directly on an Android phone under [Termux](https://termux.dev/).
No Docker, no server — a self-hosted agent in your pocket.

Use this path for a **personal agent** (one phone, one WhatsApp,
one Telegram). For multi-tenant / multi-process deployments the
regular Linux setup on a server is the right shape.

## What works

| Area | Status |
|------|--------|
| Core runtime, memory, TaskFlow, dreaming | ✅ full |
| Broker: `type: local` (in-process) or native NATS Go binary | ✅ full |
| LLM providers (MiniMax / Anthropic / OpenAI-compat / Gemini) | ✅ all rustls-based |
| WhatsApp plugin (pure Rust + Signal Protocol) | ✅ pairing via Unicode QR |
| Telegram plugin | ✅ Bot API over HTTP |
| Gmail / Google plugin + gmail-poller | ✅ OAuth over HTTP |
| Extensions (stdio + NATS) | ✅ spawn works |
| Skills: fetch-url, dns-tools, rss, weather, wikipedia, pdf-extract, brave-search, wolfram-alpha, summarize, translate | ✅ pure Rust |
| MCP client + server | ✅ stdio + HTTP |
| Health / metrics / admin HTTP servers (8080 / 9090 / 9091) | ✅ unprivileged ports |

## What needs a tweak

| Thing | Workaround |
|-------|------------|
| Service manager (no systemd) | [termux-services](https://wiki.termux.com/wiki/Termux-services) (runit) or `tmux` + `nohup` |
| Run at boot | install the **Termux:Boot** app + drop a script in `~/.termux/boot/` |
| Survives screen-off | `termux-wake-lock` (from the **Termux:API** add-on) before running the agent |
| Browser plugin (Chrome/Chromium) | use `cdp_url:` to a chromium you start manually with `--no-sandbox --disable-dev-shm-usage`; or `disabled: [browser]` if you don't need it |
| Secrets file permission gauntlet | `export CHAT_AUTH_SKIP_PERM_CHECK=1` (Android filesystem perms model differs) |
| WhatsApp public tunnel (cloudflared) | skip the public tunnel; pair locally via Unicode QR rendered on the terminal |
| Docker / compose | use `broker.type: local` or native NATS binary — no containers involved |

## Prerequisites

From a fresh Termux install:

```bash
pkg update
pkg install -y rust git curl sqlite openssl clang pkg-config
```

Optional (enables specific skills):

```bash
pkg install -y ffmpeg tesseract yt-dlp tmux openssh
```

Optional (browser plugin):

```bash
pkg install -y tur-repo
pkg install -y chromium
```

Optional (run in background without the terminal session alive):

```bash
pkg install -y termux-services termux-api
# install the companion app "Termux:API" from F-Droid
```

## Fast path — bootstrap script

The repo's `scripts/bootstrap.sh` auto-detects Termux and picks the
right defaults:

```bash
git clone https://github.com/lordmacu/nexo-rs
cd nexo-rs
./scripts/bootstrap.sh --yes
```

What it does on Termux:

1. Verifies `rust`, `git`, `curl`, `sqlite` from `pkg`
2. Downloads the static `nats-server` Go binary (arm64), drops it in
   `$PREFIX/bin/` — **or** skip with `--nats=skip` to use the local
   broker
3. Creates `./data/**` and `./secrets/` (with Termux-compatible perms)
4. Stages `config/agents.d/*.example.yaml` → `*.yaml` if missing
5. Runs `cargo build --release` (grab a coffee — ~20–40 min on
   phone hardware)
6. Optionally launches `agent setup` to pair channels

Expect a ~60–100 MB final binary.

## Manual install

### 1. Install Rust and deps

```bash
pkg install -y rust git curl sqlite openssl clang pkg-config
```

### 2. Clone and build

```bash
git clone https://github.com/lordmacu/nexo-rs
cd nexo-rs
cargo build --release --bin agent
```

### 3. Broker

**Option A — local (simplest):**

```yaml
# config/broker.yaml
broker:
  type: local
  persistence:
    enabled: true
    path: ./data/queue
```

No NATS binary needed. All pub/sub stays in-process.

**Option B — native NATS binary:**

```bash
curl -L -o /tmp/nats.tar.gz \
  https://github.com/nats-io/nats-server/releases/download/v2.10.20/nats-server-v2.10.20-linux-arm64.tar.gz
tar -xzf /tmp/nats.tar.gz -C /tmp
install -m 0755 "$(find /tmp -name nats-server -type f | head -1)" \
  $PREFIX/bin/nats-server
nats-server -js &
```

Go binaries are static and work on Termux without libc surprises.

### 4. Runtime directories and secrets

```bash
mkdir -p ./data/{queue,workspace,media,transcripts} ./secrets
```

Termux stores files under `/data/data/com.termux/files/home` by
default. Avoid pointing config paths at `/sdcard` — Android's
scoped-storage model breaks directory permissions there.

### 5. Relax the credentials perm check

Android's filesystem doesn't support the same permission bits as
Linux in the same way. The credentials gauntlet would refuse to
boot with false-positive warnings:

```bash
export CHAT_AUTH_SKIP_PERM_CHECK=1
```

Add it to `~/.termux/termux.properties` or a wrapper shell script
so it's set every time.

### 6. Launch the wizard

```bash
./target/release/agent setup
```

For the WhatsApp pairing step, the wizard renders the QR as Unicode
blocks directly in the terminal — scan from the phone's WhatsApp app
(Settings → Linked Devices). No public tunnel needed.

### 7. Run the agent

```bash
termux-wake-lock                # keep CPU awake even with screen off
./target/release/agent --config ./config
```

## Staying alive in the background

Android's aggressive task killing is the biggest operational
surprise. Pick one:

### A — `termux-wake-lock` + foreground notification

```bash
termux-wake-lock
# agent in foreground:
./target/release/agent --config ./config
```

The wake-lock persists until you run `termux-wake-unlock` or kill
the session. Minimum friction, most reliable.

### B — `termux-services` (runit)

```bash
pkg install -y termux-services
sv-enable termux-services
mkdir -p ~/.config/service/nexo-rs
cat > ~/.config/service/nexo-rs/run <<'EOF'
#!/data/data/com.termux/files/usr/bin/sh
cd /data/data/com.termux/files/home/nexo-rs
export CHAT_AUTH_SKIP_PERM_CHECK=1
exec ./target/release/agent --config ./config 2>&1
EOF
chmod +x ~/.config/service/nexo-rs/run
sv up nexo-rs
sv status nexo-rs
```

### C — Termux:Boot (start on device boot)

Install the **Termux:Boot** app from F-Droid, then:

```bash
mkdir -p ~/.termux/boot
cat > ~/.termux/boot/start-agent <<'EOF'
#!/data/data/com.termux/files/usr/bin/sh
termux-wake-lock
cd /data/data/com.termux/files/home/nexo-rs
export CHAT_AUTH_SKIP_PERM_CHECK=1
exec ./target/release/agent --config ./config
EOF
chmod +x ~/.termux/boot/start-agent
```

## Disabling the browser plugin

If you don't need headless browser control (most phone-hosted
agents don't), drop it from `config/extensions.yaml`:

```yaml
extensions:
  disabled: [browser]
```

Or, if you have `tur-repo` chromium installed and want to try:

```yaml
# config/plugins/browser.yaml
browser:
  # Start chromium yourself with:
  #   chromium --headless --no-sandbox --disable-dev-shm-usage \
  #            --disable-gpu --remote-debugging-port=9222 &
  cdp_url: http://127.0.0.1:9222
```

## Verify

```bash
curl localhost:8080/ready
curl localhost:9090/metrics
./target/release/agent status
```

## Upgrading

```bash
cd ~/nexo-rs
git pull
cargo build --release
# restart under whichever method you picked (wake-lock / runit / Boot)
```

Android's [graceful shutdown](../architecture/agent-runtime.md#graceful-shutdown)
still runs on SIGTERM — closing the Termux session or killing the
process drains the disk queue cleanly.

## See also

- [Installation](./installation.md) — shared prerequisites
- [Native install (no Docker)](./install-native.md) — the
  Linux/macOS path the Termux recipe is a sibling of
- [Plugins — Browser](../plugins/browser.md)
- [Config — broker.yaml](../config/broker.md)
