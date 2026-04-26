# Grafana dashboards

Bundled dashboards for a Nexo deployment. Each panel queries
metrics that Nexo's `/metrics` endpoint already emits — no
custom recording rules required.

## Files

| File | Title | What it covers |
|---|---|---|
| `nexo-overview.json` | **Nexo — Overview** | Tool throughput, LLM TTFT p50/p95/p99, web-search breaker state, tool cache hit ratio. Single-screen "is the system healthy" view. |
| `nexo-llm.json` | **Nexo — LLM** | Streaming TTFT quantiles by provider, chunk emission, link-understanding fetch latency + outcomes + cache. |
| `nexo-tools.json` | **Nexo — Tools & MCP** | Tool latency p95/p99 by tool, calls × outcome breakdown, MCP sampling activity, web-search calls + latency by provider. |

Three dashboards is the right number for a baseline:
**Overview** (executive), **LLM** (when the agent is slow), **Tools** (when an extension misbehaves). More slices land as new metrics ship.

## Importing

### Via Grafana UI

1. Grafana → **Dashboards** → **New** → **Import**.
2. Drop the `.json` or paste its contents.
3. Pick your Prometheus datasource from the dropdown
   (the dashboards declare a `${DS_PROMETHEUS}` variable that
   binds to whichever Prometheus you select).
4. Save.

### Via API

```bash
GRAFANA_URL=http://localhost:3000
GRAFANA_TOKEN=<service-account-token>

for f in nexo-overview.json nexo-llm.json nexo-tools.json; do
    body=$(jq -n --slurpfile dash "$f" \
              '{dashboard: $dash[0], overwrite: true}')
    curl -fsSL "$GRAFANA_URL/api/dashboards/db" \
        -H "Authorization: Bearer $GRAFANA_TOKEN" \
        -H 'Content-Type: application/json' \
        -d "$body"
done
```

### Via Grafana provisioning (recommended for production)

Drop a provisioning config at `/etc/grafana/provisioning/dashboards/nexo.yaml`:

```yaml
apiVersion: 1
providers:
  - name: nexo
    orgId: 1
    folder: 'Nexo'
    type: file
    disableDeletion: false
    updateIntervalSeconds: 60
    options:
      path: /var/lib/grafana/dashboards/nexo
```

Then mount the `ops/grafana/` directory at
`/var/lib/grafana/dashboards/nexo/`. Grafana picks them up,
keeps them read-only-from-UI, and reloads when the files change.

## Prometheus scrape config

The dashboards assume Prometheus scrapes `nexo`'s `/metrics`
endpoint (default port 9090):

```yaml
# prometheus.yml
scrape_configs:
  - job_name: nexo
    static_configs:
      - targets: ['nexo:9090']        # or 'localhost:9090' for native
    scrape_interval: 15s
    scrape_timeout: 5s
    metrics_path: /metrics
```

For a Docker-compose stack, see `docker-compose.yml` (the `nexo`
service exposes 9090 by default).

## Metric coverage

The dashboards consume exactly these series. Anything not on this
list is fair game for a follow-up dashboard:

| Series | Source crate | Phase |
|---|---|---|
| `nexo_tool_calls_total{tool,result}` | `nexo-core` | 12 (MCP) + 11 (extensions) |
| `nexo_tool_latency_ms_*` | `nexo-core` | same |
| `nexo_tool_cache_events_total{result}` | `nexo-core` | 18 (hot-reload) |
| `nexo_llm_stream_ttft_seconds_*` | `nexo-llm` | 3 (LLM integration) |
| `nexo_llm_stream_chunks_total{provider,kind}` | `nexo-llm` | same |
| `nexo_link_understanding_fetch_*` | `nexo-core` | 21 (link understanding) |
| `nexo_link_understanding_cache_total{hit}` | `nexo-core` | same |
| `nexo_mcp_sampling_*` | `nexo-mcp` | 12 |
| `nexo_web_search_*` | `nexo-web-search` | 25 |

Future dashboards on the radar (see `proyecto/PHASES.md` Phase 28):

- **Cost dashboard** — per-agent / per-binding token aggregation.
  Series TBD; Phase 28 deliverable.
- **TaskFlow status** — flow counts by state (`Running`,
  `Waiting`, `Failed`, …). Series TBD.
- **DLQ depth** — broker dead-letter queue size; alerting target.
- **Capability toggles** — gauge of `*_ALLOW_*` env vars armed.

## Editing dashboards

Round-trip:
1. Edit in Grafana UI.
2. Dashboard settings → **JSON Model** → copy.
3. Paste into the matching `.json` file.
4. **Strip the `id` field** before committing (Grafana sets it on
   import, varies per host) — `jq 'del(.id)' < dash.json > out.json`.
5. Bump the `version` field.
6. Commit.

A future CI step will validate `version` bump on changed
dashboards so PR reviewers don't have to remember.
