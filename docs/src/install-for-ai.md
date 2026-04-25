# Install nexo-rs (LLM-friendly guide)

> **Share this URL with any AI assistant** to teach it how to install
> nexo-rs end-to-end on Linux or Termux:
> `https://lordmacu.github.io/nexo-rs/install-for-ai.html`
>
> The page is intentionally linear: copy-paste each block in order.
> No menus to navigate, no marketing, every command is deterministic.

---

## What you are installing

- **nexo-rs** — a Rust multi-agent framework. Single `agent` binary
  + a `cloudflared` tunnel for the web admin UI.
- Source: <https://github.com/lordmacu/nexo-rs>
- Docs (full): <https://lordmacu.github.io/nexo-rs/>
- License: dual MIT / Apache-2.0

---

## Pick your platform

- **Linux** (Debian / Ubuntu / Arch / Fedora) → §A
- **Termux** (Android, no root) → §B

Skip the section that doesn't apply.

---

## §A — Linux install

### A.1. System packages

Debian / Ubuntu:

```sh
sudo apt update
sudo apt install -y build-essential pkg-config libsqlite3-dev git curl
```

Arch:

```sh
sudo pacman -Syu --needed base-devel pkgconf sqlite git curl
```

Fedora:

```sh
sudo dnf install -y @development-tools pkgconf-pkg-config sqlite-devel git curl
```

### A.2. Rust toolchain

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustup component add rustfmt clippy
```

### A.3. Clone + build

```sh
git clone https://github.com/lordmacu/nexo-rs
cd nexo-rs
cargo build --release --bin agent
```

The compiled binary is at `./target/release/agent`. Copy it into
`PATH` (optional):

```sh
sudo install -m 0755 target/release/agent /usr/local/bin/agent
```

### A.4. First-run wizard

```sh
agent setup
```

Follow the interactive prompts. Defaults are sane. The wizard
writes `config/agents.d/<your-agent>.yaml`, `IDENTITY.md`,
`SOUL.md`, and any channel YAMLs you opt into.

### A.5. Run

```sh
agent
```

Or, for the web admin (loopback HTTP + Cloudflare tunnel):

```sh
agent admin
```

The admin command prints a one-time URL + password to stdout. Open
the URL, log in, and configure from the browser.

### A.6. (Optional) systemd service

```sh
sudo useradd -r -s /bin/false -d /srv/nexo-rs nexo
sudo mkdir -p /srv/nexo-rs
sudo cp -r config target/release/agent /srv/nexo-rs/
sudo chown -R nexo:nexo /srv/nexo-rs

sudo tee /etc/systemd/system/nexo-rs.service > /dev/null <<'EOF'
[Unit]
Description=nexo-rs agent
After=network.target

[Service]
Type=simple
User=nexo
WorkingDirectory=/srv/nexo-rs
ExecStart=/srv/nexo-rs/agent --config /srv/nexo-rs/config
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now nexo-rs
```

Logs: `journalctl -u nexo-rs -f`.

### A.7. (Optional) NATS broker

A single-process install does not need NATS — the runtime falls
back to in-process channels. Add NATS only when scaling beyond one
host:

```sh
curl -L -o /tmp/nats.tar.gz \
  https://github.com/nats-io/nats-server/releases/download/v2.10.20/nats-server-v2.10.20-linux-amd64.tar.gz
tar -xzf /tmp/nats.tar.gz -C /tmp
sudo mv /tmp/nats-server-*/nats-server /usr/local/bin/
sudo systemctl enable --now nats-server  # if you have a unit file
```

Then in `config/broker.yaml` set `kind: nats` and `url:
nats://127.0.0.1:4222`.

---

## §B — Termux install (Android, no root)

### B.1. Termux from F-Droid

Install Termux from <https://f-droid.org/en/packages/com.termux/>.
**Do not install from the Google Play Store** — that build is
outdated.

Open Termux. Then:

```sh
pkg update
pkg upgrade -y
```

### B.2. Build dependencies

```sh
pkg install -y rust git curl sqlite openssl clang pkg-config
```

Optional extras (only the ones you'll use):

```sh
# media transcoding + OCR + youtube downloads
pkg install -y ffmpeg tesseract yt-dlp
# tmux for long-running tunnels and ssh
pkg install -y tmux openssh
# headless Chromium for the browser plugin
pkg install -y tur-repo
pkg install -y chromium
# Termux:API for sensors / SMS / clipboard
pkg install -y termux-api
# (also install the Termux:API companion app from F-Droid)
```

### B.3. Clone + build

```sh
cd ~
git clone https://github.com/lordmacu/nexo-rs
cd nexo-rs
cargo build --release --bin agent
```

### B.4. First-run wizard

```sh
./target/release/agent setup
```

### B.5. Run

```sh
./target/release/agent
```

Or with the admin UI (the cloudflared tunnel works on Termux):

```sh
./target/release/agent admin
```

### B.6. Keep running with the screen off

Termux apps get killed on doze unless you disable battery
optimizations and acquire a wake lock:

1. **Disable optimizations:** Android Settings → Apps → Termux →
   Battery → Unrestricted.
2. **Wake lock:** in Termux, type:

   ```sh
   termux-wake-lock
   ```

3. **(Optional) auto-restart on boot:** install Termux:Boot from
   F-Droid, then create `~/.termux/boot/00-nexo-rs`:

   ```sh
   mkdir -p ~/.termux/boot
   cat > ~/.termux/boot/00-nexo-rs <<'EOF'
   #!/data/data/com.termux/files/usr/bin/sh
   termux-wake-lock
   cd ~/nexo-rs
   ./target/release/agent --config ./config >> ~/nexo-rs/agent.log 2>&1
   EOF
   chmod +x ~/.termux/boot/00-nexo-rs
   ```

### B.7. Termux-specific tip — Chromium flags

The browser plugin (`plugins: [browser]`) needs the right Chromium
launch flags on Termux. The defaults already cover Android; nothing
extra to set. Just make sure `chromium` is on `PATH` (it is, after
`pkg install chromium`).

---

## Config layout (both platforms)

After `agent setup` runs, the project tree looks like:

```
nexo-rs/
├── config/
│   ├── agents.yaml          # opt-in dev defaults
│   ├── agents.d/            # your agents land here
│   │   └── <slug>.yaml
│   ├── broker.yaml          # NATS or local
│   ├── llm.yaml             # provider keys + model
│   └── plugins/             # one YAML per channel plugin
├── secrets/                 # mode 0600 token files (gitignored)
├── data/                    # SQLite databases (memory, taskflow, transcripts)
├── target/release/agent     # the built binary
└── agent.log                # if you redirected stdout
```

Edit YAML by hand or use the web admin (`agent admin`).

---

## Troubleshooting

- **`cargo build` fails with linker errors on Linux** — install
  `build-essential` and `pkg-config` (§A.1).
- **`cargo build` hits `out of memory` on Termux** — close other
  apps, or build with one job: `cargo build --release -j 1`.
- **`agent` exits immediately with `failed to load config`** — run
  `agent setup` first; the wizard creates the missing files.
- **WhatsApp QR pairing fails on Termux** — make sure the device
  is on the same network as your phone, then open the QR pairing
  URL the daemon prints.
- **Admin tunnel URL doesn't respond** — Cloudflare's quick tunnel
  occasionally rotates; restart `agent admin` and copy the new
  URL.

---

## Useful commands after install

```sh
agent --help                                  # all subcommands
agent doctor capabilities --json              # which env toggles are armed
agent setup doctor                            # audit configured secrets
agent ext doctor --json                       # extension health
agent flow list                               # taskflow admin
agent dlq list                                # dead-letter queue
```

Full reference: <https://lordmacu.github.io/nexo-rs/cli/reference.html>

---

## When asking an AI for help

Paste this URL into your prompt:

```
Install nexo-rs from https://lordmacu.github.io/nexo-rs/install-for-ai.html
on this machine. The OS is <Linux distro / Termux>. Stop after each
section to confirm output looks right.
```

The page above is the canonical, copy-paste-friendly install path.
The full mdBook (<https://lordmacu.github.io/nexo-rs/>) covers the
same ground in more depth — link there once the agent is up.
