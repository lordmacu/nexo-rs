# Docker

Production deployment as a compose stack: `nats` broker + `agent`
runtime, Docker secrets for credentials, persistent volumes for SQLite
data and the disk queue.

Source: `docker-compose.yml`, `Dockerfile`, `config/docker/`.

## Compose layout

```mermaid
flowchart LR
    subgraph STACK[docker-compose]
        NATS[nats:2.10<br/>:4222 client<br/>:8222 monitoring]
        AG[agent<br/>:8080 health<br/>:9090 metrics]
    end
    AG --> NATS

    VOL1[(./config RO)] --> AG
    VOL2[(./data RW)] --> AG
    VOL3[(./extensions RO)] --> AG
    SEC[/run/secrets/...] --> AG

    IDE[MCP clients] -.->|port 8080| AG
    PROM[Prometheus] -.->|port 9090| AG
```

## `docker-compose.yml`

Two services, healthchecks on both, shared volumes:

- **`nats`** — `nats:2.10-alpine`, exposes `:4222` for agent clients
  and `:8222` for monitoring (healthcheck hits `:8222/healthz`)
- **`agent`** — the main runtime
  - Ports: `:8080` (health), `:9090` (metrics)
  - Environment: `RUST_LOG=info`, `AGENT_ENV=production`
  - `shm_size: 1gb` — required for Chrome processes (browser plugin)
  - Bind mounts: `./config:/app/config:ro`, `./data:/app/data:rw`,
    `./extensions:/app/extensions:ro`
  - `depends_on: { nats: { condition: service_healthy } }`

## Dockerfile

Multi-stage:

1. **Builder** — Rust `cargo build --release --locked`
2. **Runtime** — `debian:bookworm-slim` with operational tools baked in:
   - `ca-certificates`, `libsqlite3-0`
   - Python + ffmpeg + tmux + yt-dlp + tesseract (for skills that need them)
   - **Google Chrome** on amd64 (OAuth + Widevine work); falls back to
     **Chromium** on arm64
   - `cloudflared` (downloaded per `TARGETARCH` at build time)
   - `dumb-init` as PID 1

Entry point: `/usr/local/bin/agent --config /app/config`.

Exposed ports: `8080`, `9090`.

## Config overrides — `config/docker/`

Mirrors the main config layout. The compose service mounts the
production overrides path:

```yaml
command: ["agent", "--config", "/app/config/docker"]
```

Key differences in the docker overrides:

- `broker.yaml` — NATS URL points at the Docker service name
  (`nats://nats:4222`); persistence at `/app/data/queue/broker.db`
- `llm.yaml` — reads API keys from `/run/secrets/<name>`
- Other files (`agents.yaml`, `memory.yaml`, `extensions.yaml`) override
  defaults for container paths

## Secrets

The compose file declares Docker secrets and the config overrides
reference them:

```yaml
services:
  agent:
    secrets:
      - minimax_api_key
      - minimax_group_id
      - google_client_id
      - google_client_secret
secrets:
  minimax_api_key:
    file: ./secrets/minimax_api_key.txt
  minimax_group_id:
    file: ./secrets/minimax_group_id.txt
  ...
```

Config reads them via the `${file:/run/secrets/...}` syntax. Secrets
appear as mode-0400 files inside the container — nothing ever touches
env vars.

See [Configuration — layout](../config/layout.md#env-vars-and-secrets-in-yaml).

## Operating the stack

```bash
docker compose up -d           # start
docker compose logs -f agent   # follow logs
docker compose exec agent agent ext list
docker compose exec agent agent dlq list
docker compose restart agent   # rolling reload (SIGTERM → 5 s grace)
docker compose down            # stop (preserves volumes)
```

## Scaling

- **Horizontal scaling needs an external NATS cluster.** Running the
  compose with two `agent` replicas pointed at a single NATS server
  works for isolated workloads but duplicate-delivery across agents
  on the same topic is not avoided by the compose itself — the
  single-instance lockfile (see [Fault tolerance](../architecture/fault-tolerance.md#single-instance-lockfile))
  assumes one agent process per data directory.
- For real scale: one NATS cluster + N agent processes, each with its
  own `./data/` volume.

## Health checks for orchestration

```yaml
services:
  agent:
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:8080/ready"]
      interval: 10s
      timeout: 3s
      retries: 3
      start_period: 30s
```

Readiness gate is `/ready` (covered in [metrics + health](./metrics.md#health-endpoints)).
`start_period` needs to cover first-boot extension discovery + all
agent runtimes attaching to their topics.

## Gotchas

- **Volume ownership.** Don't mount `./data` as root-owned if your
  container runs as non-root. The runtime will fail to write the
  SQLite files and you'll only see cryptic `readonly database`
  errors.
- **Chrome needs `/dev/shm` space.** The `shm_size: 1gb` is not
  optional when the browser plugin is active — Chrome processes
  silently corrupt their state if starved.
- **`config/docker/` is committed, secrets are not.** `./secrets/` is
  gitignored. Populate it before the first `compose up`.
