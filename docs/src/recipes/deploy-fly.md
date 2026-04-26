# Deploy on Fly.io

Recipe for a single-region Fly.io deploy. Fly's strengths fit
Nexo well: persistent volumes (for the SQLite state), health
checks, free TLS, easy multi-region scale-out, and a generous
free tier (up to 3 shared-1x VMs free) that covers a personal
agent.

## What you end up with

- Nexo daemon + bundled local NATS broker on a single Fly machine
- Persistent volume mounted at `/var/lib/nexo-rs/`
- Free TLS via `fly.io` subdomain (custom domain optional)
- Auto-redeploy on every git push to `main` (via Fly GitHub Action)
- Fly's built-in metrics + log streaming

Estimated cost: **$0–$5/mo** (free tier covers shared-1x VM +
small volume; bigger Chrome workloads = $5-15/mo on a
performance-1x).

## 0. Prerequisites

```bash
# Install flyctl
curl -L https://fly.io/install.sh | sh
fly auth login
fly auth signup     # if first time

# Confirm:
fly version
```

## 1. Initialize the app

From the repo root:

```bash
fly launch \
  --name nexo-yourname \
  --region <closest-region>  \
  --vm-cpu-kind shared       \
  --vm-cpus 1                \
  --vm-memory 1024           \
  --no-deploy
```

`--no-deploy` lets us tweak the generated `fly.toml` before the
first build.

## 2. `fly.toml`

Replace the auto-generated `fly.toml` with this:

```toml
app = "nexo-yourname"
primary_region = "ams"           # or whichever closest

# Use the published GHCR image instead of building per-deploy.
[build]
  image = "ghcr.io/lordmacu/nexo-rs:latest"

# Persistent state — Fly volumes survive restarts and are
# mounted into the VM. SQLite + transcripts + secret/ live here.
[mounts]
  source = "nexo_data"
  destination = "/app/data"

# Override the container CMD so config + state align with the
# fly volume layout. NEXO_HOME defaults to /app/data so
# everything writable lands on the volume.
[env]
  RUST_LOG = "info"
  NEXO_HOME = "/app/data"

# `services` block tells Fly which container ports to expose.
[[services]]
  internal_port = 8080
  protocol = "tcp"
  auto_stop_machines = false   # keep the agent running 24/7
  auto_start_machines = true
  min_machines_running = 1

  [[services.ports]]
    port = 80
    handlers = ["http"]
    force_https = true

  [[services.ports]]
    port = 443
    handlers = ["tls", "http"]

  [services.concurrency]
    type = "connections"
    soft_limit = 200
    hard_limit = 250

  [[services.tcp_checks]]
    interval = "15s"
    timeout = "2s"
    grace_period = "30s"

# Metrics endpoint — Fly scrapes Prometheus-style automatically.
[metrics]
  port = 9090
  path = "/metrics"

# VM sizing — bump to performance-1x when the browser plugin is on.
[[vm]]
  cpu_kind = "shared"
  cpus = 1
  memory_mb = 1024
```

## 3. Create the volume

```bash
fly volumes create nexo_data --region ams --size 3
```

3 GB covers SQLite + a few months of transcripts. Bump as needed.

## 4. Set secrets

Fly's secret store injects them as env vars at runtime. Reference
them from `config/llm.yaml` via `${ENV_VAR}` placeholders:

```bash
fly secrets set ANTHROPIC_API_KEY=sk-ant-...
fly secrets set MINIMAX_API_KEY=...
fly secrets set MINIMAX_GROUP_ID=...
# Anything else your llm.yaml references via ${...}
```

The Nexo config loader resolves `${ANTHROPIC_API_KEY}` placeholders
from the process env — works the same whether the env vars come
from `/run/secrets/`, `~/.bashrc`, or Fly secrets.

## 5. Pre-bake the config

Fly mounts `/app/data` from the volume but `/app/config` lives
inside the image. Two options:

**Option A — bake config into a custom image (recommended).** Wrap
the GHCR image in a tiny Dockerfile:

```dockerfile
# Dockerfile.fly
FROM ghcr.io/lordmacu/nexo-rs:latest

# Copy your operator config tree into the image. Adjust to
# whatever your setup needs — just don't ship secrets here, use
# fly secrets for those.
COPY ./config/fly /app/config

# fly.toml's CMD already passes `--config /app/config`.
```

Then change `fly.toml`:

```toml
[build]
  dockerfile = "Dockerfile.fly"
```

**Option B — write config to the volume on first boot.** Use a
Fly machine `init` script that runs `nexo setup --non-interactive
--from-env` once, then exits.

## 6. Deploy

```bash
fly deploy
```

First deploy spins up the volume + machine. Subsequent deploys
hot-swap the image with zero-downtime rolling restart.

## 7. Verify

```bash
# Health
fly status
curl https://nexo-yourname.fly.dev/health

# Metrics (over the Fly internal network)
fly proxy 9090:9090 -a nexo-yourname &
curl http://127.0.0.1:9090/metrics | head -20

# Logs
fly logs

# SSH in if something looks off
fly ssh console
```

## 8. Custom domain

```bash
fly certs add nexo.yourdomain.com
# Add the CNAME to your DNS as instructed
fly certs check nexo.yourdomain.com
```

## 9. Continuous deploy on push

Drop this into `.github/workflows/fly-deploy.yml`:

```yaml
name: fly-deploy
on:
  push:
    branches: [main]
permissions:
  contents: read
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: superfly/flyctl-actions/setup-flyctl@master
      - run: flyctl deploy --remote-only
        env:
          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN }}
```

Get a token: `fly tokens create deploy -x 999999h`. Drop in repo
secrets as `FLY_API_TOKEN`.

## 10. Backups

```bash
# Manual snapshot
fly volumes snapshots create nexo_data
fly volumes snapshots list  nexo_data

# Restore (creates a new volume from the snapshot)
fly volumes create nexo_data_restored \
  --snapshot-id vs_xxxxxxxxxxxx \
  --region ams
```

For automated backups, set up a daily Fly cron machine that runs
`fly volumes snapshots create` against the data volume.

## Limits + escape hatches

- **Free tier shared-1x has 1 vCPU + 256 MB RAM** — too small for
  the browser plugin. Disable Chrome (`plugins.browser.enabled:
  false`) on shared-1x; or bump to **performance-1x** ($15/mo,
  1 vCPU + 2 GB).
- **Single-region by default** — Fly has a multi-region story
  but the broker (NATS) doesn't speak Fly's distributed
  primitives. For multi-region, run NATS on a dedicated VM with
  `NatsBroker` cluster mode and pin Nexo machines to the same
  region as their broker.
- **Volume snapshots cost $0.15/GB/month** — small but adds up if
  you keep many. Auto-prune via the snapshot cron.

## Troubleshooting

- **Volume mount fails on machine start** — `fly volumes list`
  must show the volume in the same region as the machine. Mismatch
  = create the volume in the right region or move the machine.
- **Out of memory + machine cycles** — most likely the browser
  plugin loaded Chrome on a shared-1x. Check `fly logs` for OOM
  killer messages; bump VM size or disable the browser plugin.
- **Secrets not picked up after deploy** — Fly redacts them in
  logs but they're in the env. SSH in (`fly ssh console`), run
  `printenv | grep ANTHROPIC` to verify.

## Related

- [Docker GHCR](../ops/docker.md) — same image Fly pulls
- [Hetzner deploy](./deploy-hetzner.md) — bare-VM alternative
  if you outgrow Fly's free tier or want full control
- Phase 27.5 (Docker GHCR) — source of the image this recipe
  pulls
