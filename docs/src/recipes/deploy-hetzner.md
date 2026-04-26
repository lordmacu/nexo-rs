# Deploy on Hetzner Cloud (CX22)

A concrete recipe for a single-VPS production deploy. CX22 is the
Hetzner sweet spot — €3.79/mo, 2 vCPU, 4 GB RAM, 40 GB SSD, ARM64,
20 TB transfer included. Runs the Nexo daemon + an internal NATS
broker comfortably with headroom for the browser plugin (Chrome).

This recipe targets a **single-tenant personal-agent** deploy. For
multi-tenant or multi-process see Phase 32.

## What you end up with

- Nexo daemon under systemd, auto-start on boot
- NATS broker on the same host (`nats-server` from the official
  Debian package), auto-start
- Cloudflare Tunnel for inbound HTTPS without opening ports
- UFW firewall: only outbound + cloudflared
- Unattended security upgrades
- TLS handled by Cloudflare; no Let's Encrypt cert renewal to babysit

Estimated cost: **~€4/month** (CX22 only; Cloudflare Tunnel is free).

## 0. Prerequisites

- Hetzner Cloud account with API token
- Cloudflare account with a domain pointed at it
- SSH key uploaded to Hetzner (`hcloud ssh-key create --name ops --public-key-from-file ~/.ssh/id_ed25519.pub`)

## 1. Provision the VPS

Via Hetzner Cloud console: **New Server** → Location: any close to
your users → Image: `Debian 12` → Type: **CX22** (ARM64, shared
vCPU). Add your SSH key. Name it `nexo-1`.

CLI alternative:

```bash
hcloud server create \
  --name nexo-1 \
  --type cx22 \
  --image debian-12 \
  --ssh-key ops \
  --location nbg1
```

Wait ~30s, grab the IPv4 from the dashboard.

## 2. Initial hardening (one-time)

SSH in as root, then drop privileges to a sudo user:

```bash
ssh root@<ip>
adduser ops
usermod -aG sudo ops
rsync --archive --chown=ops:ops ~/.ssh /home/ops
exit

ssh ops@<ip>
sudo apt update && sudo apt full-upgrade -y
sudo apt install -y unattended-upgrades ufw fail2ban
sudo dpkg-reconfigure -p low unattended-upgrades

# Firewall: deny inbound, allow outbound + ssh from your IP only
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow from <your-home-ip> to any port 22 proto tcp
sudo ufw enable

# Disable root SSH + password auth
sudo sed -i 's/^#\?PermitRootLogin.*/PermitRootLogin no/' /etc/ssh/sshd_config
sudo sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config
sudo systemctl restart ssh
```

## 3. Install Nexo from the .deb

Once Phase 27.4 ships and a release exists with an arm64 .deb:

```bash
curl -LO https://github.com/lordmacu/nexo-rs/releases/latest/download/nexo-rs_arm64.deb

# Verify the signature first (Phase 27.3):
curl -LO https://github.com/lordmacu/nexo-rs/releases/latest/download/nexo-rs_arm64.deb.bundle
cosign verify-blob \
  --bundle nexo-rs_arm64.deb.bundle \
  --certificate-identity-regexp 'https://github.com/lordmacu/nexo-rs/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  nexo-rs_arm64.deb \
  || { echo "REFUSING TO INSTALL UNSIGNED PACKAGE"; exit 1; }

sudo apt install ./nexo-rs_arm64.deb
```

The post-install scaffolds the `nexo` user, owns
`/var/lib/nexo-rs/`, and prints next steps. **Does not auto-start
the service** — that comes after we wire config.

## 4. Install + enable NATS

```bash
# Hetzner Debian repo doesn't ship nats-server; use the upstream .deb
NATS_VERSION=2.10.20
curl -LO "https://github.com/nats-io/nats-server/releases/download/v${NATS_VERSION}/nats-server-v${NATS_VERSION}-linux-arm64.deb"
sudo apt install ./nats-server-v${NATS_VERSION}-linux-arm64.deb
sudo systemctl enable --now nats-server
```

NATS now listens on `127.0.0.1:4222` (loopback only) — exactly
what we want; only Nexo running on the same host should reach it.

## 5. Wire Nexo config

```bash
sudo -u nexo nexo setup
```

The wizard asks for:

- LLM provider keys (Anthropic / MiniMax / etc.) — paste them; they
  land in `/var/lib/nexo-rs/secret/` mode 0600 owned by `nexo:nexo`
- WhatsApp / Telegram pairing — defer if not needed yet
- Memory backend — pick `sqlite-vec` (default for single-host)

The wizard writes `/etc/nexo-rs/{agents,broker,llm,memory}.yaml`.
Verify `broker.yaml` points at `nats://127.0.0.1:4222`.

## 6. Cloudflare Tunnel for HTTPS

The Nexo admin port (8080) shouldn't be exposed directly. Use a
tunnel:

```bash
# Install cloudflared
curl -LO https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64.deb
sudo apt install ./cloudflared-linux-arm64.deb

# Authenticate (opens a browser link — visit it on your laptop)
cloudflared tunnel login

# Create tunnel
cloudflared tunnel create nexo-1

# Route a hostname
cloudflared tunnel route dns nexo-1 nexo.yourdomain.com

# Config
sudo mkdir -p /etc/cloudflared
sudo tee /etc/cloudflared/config.yml >/dev/null <<EOF
tunnel: nexo-1
credentials-file: /home/ops/.cloudflared/<UUID>.json

ingress:
  - hostname: nexo.yourdomain.com
    service: http://127.0.0.1:8080
  - service: http_status:404
EOF

# Run as a service
sudo cloudflared service install
sudo systemctl enable --now cloudflared
```

Now `https://nexo.yourdomain.com` reaches the Nexo admin via
Cloudflare's edge — TLS terminated at Cloudflare, no cert renewal,
DDoS protection bundled.

## 7. Start Nexo

```bash
sudo systemctl enable --now nexo-rs
sudo journalctl -u nexo-rs -f
```

You should see the boot sequence: config validated → broker
connected → agents loaded → ready.

## 8. Verify

```bash
# Local health check (over the loopback)
curl -fsSL http://127.0.0.1:8080/health

# External via the tunnel
curl -fsSL https://nexo.yourdomain.com/health

# Metrics endpoint
curl -fsSL http://127.0.0.1:9090/metrics | head -20
```

## 9. Backups

The state lives in `/var/lib/nexo-rs/`. Daily snapshot to S3 /
Backblaze:

```bash
# /etc/cron.daily/nexo-backup
#!/bin/sh
set -eu
TIMESTAMP=$(date -u +%Y%m%dT%H%M%SZ)
BACKUP="/tmp/nexo-${TIMESTAMP}.tar.zst"

# Pause the runtime briefly so SQLite isn't mid-write.
systemctl stop nexo-rs

tar -I 'zstd -19 -T0' \
    -cf "$BACKUP" \
    -C /var/lib/nexo-rs \
    --exclude='./queue/*.tmp' \
    .

systemctl start nexo-rs

# Upload — adjust to your storage backend
rclone copy "$BACKUP" remote:nexo-backups/
rm "$BACKUP"

# Retain last 30
rclone delete --min-age 30d remote:nexo-backups/
```

`chmod +x /etc/cron.daily/nexo-backup`.

For a sub-second pause-free backup, use SQLite's
`VACUUM INTO`-based hot backup — track Phase 36 (backup, restore,
migrations) for the upcoming `nexo backup` subcommand.

## 10. Updates

```bash
# Pull the latest .deb
curl -LO https://github.com/lordmacu/nexo-rs/releases/latest/download/nexo-rs_arm64.deb
# Verify (always)
cosign verify-blob ...
# Install (apt restarts the service automatically)
sudo apt install ./nexo-rs_arm64.deb
```

Or wire the apt repo (Phase 27.4 follow-up) and run
`apt upgrade nexo-rs` like any other system package.

## Limits + escape hatches

- **Browser plugin** uses ~300 MB RAM per Chrome process. CX22 has
  4 GB; budget 2 instances tops. Bump to **CX32** (€7/mo, 4 vCPU,
  8 GB) when you start hitting OOM.
- **NATS on the same host** is fine for single-tenant; for
  multi-host, run NATS on its own VM (CX12, €3.29/mo).
- **TLS at Cloudflare only** means traffic between Cloudflare's
  edge and your VPS is plain HTTP over the tunnel. The tunnel is
  encrypted at the transport layer (QUIC + mTLS to Cloudflare),
  so this is fine — but if you want defense-in-depth, terminate
  TLS again locally with caddy or nginx.

## Troubleshooting

- **Tunnel disconnects after reboot** — `systemctl status
  cloudflared`. The credentials file moved if you reinstalled
  cloudflared with a different `service install`. Re-run
  `cloudflared service install` after `cloudflared tunnel login`.
- **NATS refuses connections** — the upstream .deb binds
  `0.0.0.0:4222` by default. Edit
  `/etc/nats-server/nats-server.conf` to set
  `host: 127.0.0.1` and `systemctl restart nats-server`.
- **Nexo can't write to /var/lib/nexo-rs/** — `sudo chown -R
  nexo:nexo /var/lib/nexo-rs && sudo chmod 0750
  /var/lib/nexo-rs`.

## Related

- [Docker compose](../ops/docker.md) — single-machine but
  containerized (vs systemd-native here)
- [Native install](../getting-started/install-native.md) — the
  underlying mechanics of step 3 if you skip the .deb
- Phase 27.4 (Debian / RPM packages) — source of the `.deb`
  this recipe consumes
