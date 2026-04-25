# Metrics & health

Prometheus metrics on `:9090/metrics`, health/readiness on `:8080`,
admin console on `127.0.0.1:9091`. Everything an operator or
orchestrator needs to decide "is the agent healthy?" without reading
logs.

Source: `crates/core/src/telemetry.rs`, `src/main.rs`.

## Ports at a glance

| Port | Binding | Purpose |
|------|---------|---------|
| `:9090` | `0.0.0.0` | Prometheus `/metrics` scrape |
| `:8080` | `0.0.0.0` | Health `/health`, readiness `/ready`, WhatsApp pairing pages |
| `:9091` | `127.0.0.1` | Admin console (loopback only) |

Ports are not configurable yet — if you need to remap, port-forward
outside the agent (Docker, k8s service).

## `/metrics` (Prometheus)

Exposed metrics:

| Name | Type | Labels | What |
|------|------|--------|------|
| `llm_requests_total` | counter | `agent`, `provider`, `model` | Every LLM completion request |
| `llm_latency_ms` | histogram | `agent`, `provider`, `model` | Buckets 50, 100, 250, 500, 1000, 2500, 5000, 10000 ms |
| `messages_processed_total` | counter | `agent` | Inbound messages that reached an agent |
| `nexo_extensions_discovered` | counter | `status={ok,disabled,invalid}` | Emitted on every discovery sweep |
| `nexo_tool_calls_total` | counter | `agent`, `outcome={ok,error,blocked,unknown}`, `tool` | Tool invocations |
| `nexo_tool_cache_events_total` | counter | `agent`, `event={hit,miss,put,evict}`, `tool` | Tool-level memoization |
| `nexo_tool_latency_ms` | histogram | `agent`, `tool` | Per-tool latency |
| `circuit_breaker_state` | gauge | `breaker` | `0 = Closed`, `1 = Open`; always includes `nats` |
| `credentials_accounts_total` | gauge | `channel` | Per-channel labelled instance count (Phase 17) |
| `credentials_bindings_total` | gauge | `agent`, `channel` | `1` when the agent has a credential bound, `0` otherwise |
| `channel_account_usage_total` | counter | `agent`, `channel`, `direction={inbound,outbound}`, `instance` | Every credential use |
| `channel_acl_denied_total` | counter | `agent`, `channel`, `instance` | Outbound calls rejected by `allow_agents` |
| `credentials_resolve_errors_total` | counter | `channel`, `reason` | Resolver failures (`unbound`, `not_found`, `not_permitted`) |
| `credentials_breaker_state` | gauge | `channel`, `instance` | `0=closed`, `1=half-open`, `2=open`. Per-(channel, instance) circuit breaker — a 429 from one number cannot trip the breaker for a sibling account. |
| `credentials_boot_validation_errors_total` | counter | `kind` | Gauntlet errors by kind at boot |
| `credentials_insecure_paths_total` | gauge | — | Credential files with lax permissions at boot |
| `credentials_google_token_refresh_total` | counter | `account_fp`, `outcome={ok,err}` | Google OAuth refresh attempts (fp = sha256[..8], not raw email) |
| `pairing_inbound_challenged_total` | counter | `channel`, `result={delivered_via_adapter,delivered_via_broker,publish_failed,no_adapter_no_broker_topic}` | DM-challenge dispatch attempts (Phase 26.x) |
| `pairing_approvals_total` | counter | `channel`, `result={ok,expired,not_found}` | `nexo pair approve` outcomes (Phase 26.y) |
| `pairing_codes_expired_total` | counter | — | Setup codes pruned past TTL or rejected as expired on approve |
| `pairing_bootstrap_tokens_issued_total` | counter | `profile` | Bootstrap tokens minted by `BootstrapTokenIssuer::issue` |
| `pairing_requests_pending` | gauge | `channel` | Pending pairing requests (push-tracked; `PairingStore::refresh_pending_gauge` exposed for drift recovery after a daemon restart) |

Circuit-breaker state for the `nats` breaker is sampled **at scrape
time** from broker readiness, so a stalled publish path shows up in
the next scrape without needing an eager push.

The `credentials_*` and `channel_*` series are documented with full
schema examples in [`config/credentials.md`](../config/credentials.md).
`account_fp` is always an 8-byte sha256 fingerprint of the account id,
never the raw JID or email, so scraped metrics stay safe to share.

## Useful alerts

### LLM provider flapping

```yaml
- alert: LlmError5xxHigh
  expr: sum(rate(llm_requests_total{outcome="error"}[5m])) by (provider) > 0.1
  for: 5m
```

### NATS circuit open

```yaml
- alert: NatsBreakerOpen
  expr: circuit_breaker_state{breaker="nats"} == 1
  for: 1m
```

### Tool call failures

```yaml
- alert: ToolErrorSpike
  expr: |
    sum(rate(nexo_tool_calls_total{outcome="error"}[5m])) by (tool) > 0.5
  for: 10m
```

## Health endpoints

```mermaid
flowchart LR
    GET1[GET /health] --> OK[200 OK<br/>always<br/>{status:ok}]
    GET2[GET /ready] --> CHK{broker ready<br/>AND agents > 0?}
    CHK -->|yes| RDY[200 OK<br/>{status:ready,<br/>agents_running:N}]
    CHK -->|no| NOT[503 Service Unavailable<br/>{status:not_ready,<br/>broker_ready,<br/>agents_running}]
```

- **`GET /health`** — liveness probe. Returns 200 as long as the
  process is accepting connections. Don't use this as a traffic
  gate.
- **`GET /ready`** — readiness probe. Returns 200 **only** when the
  broker is ready **and** at least one agent runtime is attached to
  inbound topics. Returns 503 during boot, shutdown, or broker
  outage.
- **`GET /whatsapp/*`** — QR pairing pages and the `/whatsapp/pair`
  tunnel endpoint; see [WhatsApp plugin](../plugins/whatsapp.md).

### Kubernetes probes

```yaml
livenessProbe:
  httpGet: { path: /health, port: 8080 }
  initialDelaySeconds: 10
  periodSeconds: 10
readinessProbe:
  httpGet: { path: /ready, port: 8080 }
  initialDelaySeconds: 30
  periodSeconds: 5
```

`initialDelaySeconds: 30` for readiness covers extension discovery
and every agent runtime attaching its subscriptions.

## Admin console (`:9091`)

Loopback-only. Exposes:

| Path | Purpose |
|------|---------|
| `/admin/agents` | Agent directory with live status, session counts |
| `/admin/tool-policy` | Query the tool-policy registry |

The `agent status [--endpoint URL] [--agent-id ID] [--json]` CLI
subcommand hits this endpoint and prints a table or JSON; good for
scripting ops without grepping logs.

Remote access requires an explicit tunnel — the port is never
exposed publicly by default.

## Scrape config sample

```yaml
# prometheus.yml
scrape_configs:
  - job_name: nexo-rs
    scrape_interval: 15s
    static_configs:
      - targets: ['agent:9090']
```

For Docker compose: the service name is `agent`. For k8s: use the
service DNS.

## Gotchas

- **`circuit_breaker_state` only labels per-breaker, not per-provider.**
  Multiple LLM providers each have their own breaker instance, but
  they surface as distinct `breaker` label values. If you expected
  `{provider="anthropic"}` you'll need a label rename in your Prometheus
  relabel config.
- **Histograms are non-configurable.** Buckets are compiled in. If
  your SLO requires fine-grained buckets below 50 ms, it is worth
  opening an issue.
- **`/ready` 503 during shutdown is expected.** Don't alert on 5 s
  of 503 bursts — alert on `rate(> 30 s)`.
