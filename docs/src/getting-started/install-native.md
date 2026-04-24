# Native install (no Docker)

If you'd rather run nexo-rs directly on a Linux / macOS host —
development loop, single-machine deploy, restricted container
environment — this page walks through every step and names the
bootstrap script that automates it.

## Fast path

```bash
git clone git@github.com:lordmacu/nexo-rs.git
cd nexo-rs
./scripts/bootstrap.sh
```

`scripts/bootstrap.sh` verifies prerequisites, installs a local
NATS, creates the runtime directories, stages example configs, and
builds the `agent` binary. Re-runnable — each step is idempotent.

Keep reading for what it actually does (and what to do when a step
needs manual intervention).

## Prerequisites

| Tool | Required for | Notes |
|------|--------------|-------|
| **Rust** (stable, edition 2021) | building the binaries | `rust-toolchain.toml` pins the channel |
| **Git** | cloning + per-agent workspace-git | default on most hosts |
| **NATS** ≥ 2.10 | the broker | binary or dev docker container is fine |
| **SQLite** ≥ 3.38 | memory + broker disk queue | ships with most distros |
| **Chrome / Chromium** | browser plugin (optional) | skip if you don't use the browser plugin |
| **ffmpeg + ffprobe** | media-related skills (optional) | skip if you don't ship those skills |
| **yt-dlp / tesseract / tmux / ssh** | individual skills (optional) | each skill declares its `requires.bins` |

On Ubuntu / Debian:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libsqlite3-dev git curl
```

On macOS:

```bash
xcode-select --install
brew install sqlite git
```

## Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup component add rustfmt clippy
```

The repo's `rust-toolchain.toml` pins the channel; no manual version
pick is needed.

## Install NATS

Pick one path:

### Option A — native NATS server

```bash
# Linux x86_64
curl -L -o /tmp/nats.tar.gz \
  https://github.com/nats-io/nats-server/releases/download/v2.10.20/nats-server-v2.10.20-linux-amd64.tar.gz
tar -xzf /tmp/nats.tar.gz -C /tmp
sudo mv /tmp/nats-server-*/nats-server /usr/local/bin/
```

For macOS: `brew install nats-server`.

Start it:

```bash
nats-server -js                      # foreground
nats-server -js -D                   # foreground with debug
# or, as a systemd service: see below
```

### Option B — dev throwaway via Docker

Even on a "no-Docker" box, a single short-lived container for the
broker is often fine:

```bash
docker run -d --name nexo-nats --restart unless-stopped \
  -p 4222:4222 -p 8222:8222 nats:2.10-alpine
```

This is the same broker the compose stack would use; only the
broker itself runs in a container.

### Systemd unit (Linux, production)

`/etc/systemd/system/nats-server.service`:

```ini
[Unit]
Description=NATS Server
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/nats-server -js
Restart=on-failure
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now nats-server
```

## Build nexo-rs

```bash
git clone git@github.com:lordmacu/nexo-rs.git
cd nexo-rs
cargo build --release
```

The output is `./target/release/agent`. Symlink it into `$PATH` if
you want:

```bash
sudo ln -sf "$(pwd)/target/release/agent" /usr/local/bin/agent
```

## Prepare runtime directories

```bash
mkdir -p ./data/{queue,workspace,media,transcripts}
mkdir -p ./secrets          # gitignored; holds API keys, nkey files, etc.
chmod 700 ./secrets         # restrictive — the credential gauntlet checks this
```

## Stage config

The repo ships `config/*.yaml` with safe defaults. Override whatever
you need:

```bash
# Optional: copy the ana sales agent template into the gitignored dir
cp config/agents.d/ana.example.yaml config/agents.d/ana.yaml

# Add an API key:
export MINIMAX_API_KEY=...
export MINIMAX_GROUP_ID=...
# or write to secrets/ files referenced from config/llm.yaml via ${file:...}
```

See [Configuration — layout](../config/layout.md) for the full
reference.

## Pair channels and set secrets

```bash
./target/release/agent setup
```

The wizard pairs WhatsApp / Telegram / Google / LLM credentials
interactively. See [Setup wizard](./setup-wizard.md).

## First run

```bash
./target/release/agent --config ./config
```

Watch the startup summary — it tells you exactly which plugins
loaded, which extensions were skipped and why, and whether the
broker is reachable. If anything's missing, the log line names the
specific file or env var to fix.

## Run as a systemd service

`/etc/systemd/system/nexo-rs.service`:

```ini
[Unit]
Description=nexo-rs agent
Requires=nats-server.service
After=nats-server.service

[Service]
Type=simple
User=nexo
Group=nexo
WorkingDirectory=/srv/nexo-rs
Environment=RUST_LOG=info
Environment=AGENT_ENV=production
ExecStart=/usr/local/bin/agent --config /srv/nexo-rs/config
Restart=on-failure
RestartSec=5
# Optional: restrict where the agent can write
ReadWritePaths=/srv/nexo-rs/data /srv/nexo-rs/secrets

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd -r -s /bin/false -d /srv/nexo-rs nexo
sudo chown -R nexo:nexo /srv/nexo-rs
sudo systemctl daemon-reload
sudo systemctl enable --now nexo-rs
```

Logs:

```bash
journalctl -u nexo-rs -f
```

## macOS launchd

`~/Library/LaunchAgents/dev.nexo-rs.agent.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>          <string>dev.nexo-rs.agent</string>
  <key>WorkingDirectory</key><string>/Users/you/nexo-rs</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/you/nexo-rs/target/release/agent</string>
    <string>--config</string><string>/Users/you/nexo-rs/config</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>RUST_LOG</key><string>info</string>
  </dict>
  <key>RunAtLoad</key>      <true/>
  <key>KeepAlive</key>      <true/>
</dict>
</plist>
```

```bash
launchctl load -w ~/Library/LaunchAgents/dev.nexo-rs.agent.plist
launchctl start dev.nexo-rs.agent
```

## Verify

```bash
agent status                    # lists running agents
curl localhost:8080/ready       # readiness
curl localhost:9090/metrics     # Prometheus metrics
```

See [Metrics + health](../ops/metrics.md).

## Upgrading

```bash
cd nexo-rs
git pull
cargo build --release
sudo systemctl restart nexo-rs      # Linux
# or: launchctl kickstart -k gui/$UID/dev.nexo-rs.agent   # macOS
```

The [graceful shutdown sequence](../architecture/agent-runtime.md#graceful-shutdown)
drains in-flight work and persists the disk queue before exit.

## Uninstalling

```bash
sudo systemctl disable --now nexo-rs nats-server
sudo rm /etc/systemd/system/{nexo-rs,nats-server}.service
sudo rm /usr/local/bin/{agent,nats-server}
sudo userdel nexo
rm -rf /srv/nexo-rs
```

## See also

- [Quick start](./quickstart.md) — the five-minute dev loop
- [Docker](../ops/docker.md) — container path for comparison
- [Setup wizard](./setup-wizard.md)
- [Configuration](../config/layout.md)
