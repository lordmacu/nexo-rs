# `[plugin.metrics]` — Prometheus scrape contribution

> Phase 81.33.b.real Stage 5 (Layer 11 of the
> [plugin auto-discovery design](../architecture/plugin-auto-discovery.md)).
> Status: shipped 2026-05-15.

Plugins exposing Prometheus metrics declare a broker topic the
daemon scrapes on every `/metrics` HTTP request. The plugin's
subprocess handles the scrape, returns Prometheus text, and the
daemon concatenates it into the aggregate response.

Replaces the previous pattern where each plugin's metrics call
was hardcoded inside `src/main.rs::run_metrics_server` (e.g. the
legacy `nexo_plugin_email::metrics::render_prometheus(...)`
direct call).

## Manifest section

```toml
[plugin.metrics]
prometheus          = true
broker_topic_prefix = "plugin.email"
# Optional:
# timeout_seconds = 5
```

Fields:

- `prometheus` (default `false`) — opt the plugin into the
  `/metrics` scrape loop.
- `broker_topic_prefix` (required when `prometheus = true`) —
  daemon publishes to `<broker_topic_prefix>.metrics.scrape`.
- `timeout_seconds` (optional, default 5s) — per-scrape broker
  RPC timeout. Scrapes happen per `/metrics` HTTP request so the
  daemon-side latency budget is tight; plugins exceeding the
  timeout warn-log + contribute nothing for that scrape.

## Broker JSON-RPC contract

Daemon → plugin on `<broker_topic_prefix>.metrics.scrape`:

```json
{}
```

Plugin replies:

```json
{ "text": "# HELP <metric> ...\n# TYPE ...\n<metric> <value>\n..." }
```

Empty / missing `text` is treated as a successful scrape with no
metrics. The daemon does NOT validate Prometheus text shape —
plugin owns the surface entirely. Adding a trailing newline is
optional; the daemon appends `\n` if missing.

## Daemon-side aggregation

`run_metrics_server` (`src/main.rs:15097+`) concatenates from:

1. `nexo_core::telemetry::render_prometheus(nats_open)` — daemon-internal counters.
2. `nexo_llm::telemetry::render_prometheus()` — LLM provider stats.
3. `nexo_mcp::telemetry::render_prometheus()` + server-side dispatch metrics.
4. `nexo_poller::telemetry::render_prometheus()` — Gmail / generic poller counters.
5. `nexo_plugin_email::metrics::render_prometheus(...)` — **legacy direct call**, kept until email plugin migrates to broker scrape.
6. `nexo_tunnel_quick::metrics::render_prometheus_for(...)` — tunnel supervisor counters.
7. **Phase 5: `nexo_pairing::plugin_metrics::scrape_all(...)`** — every plugin that declared `[plugin.metrics] prometheus = true`.

Order matters for Prometheus scrape — duplicate metric names
across sources are not deduplicated; the LAST occurrence wins
when the scraper rebuilds its state. Plugins should namespace
their metrics with a prefix (`my_plugin_<metric>`) to avoid
collisions.

## Failure isolation

One slow / unresponsive plugin does NOT stall the `/metrics`
response. Each scrape has its own timeout (default 5s). On
failure (timeout, broker error, malformed reply) the daemon:

1. Logs a warn-level event with plugin id + error string.
2. Contributes empty string for that plugin in the aggregate.
3. Continues with the remaining plugins.

This trades immediate observability of plugin metric outages for
operator UX — a watchdog scraping every 15s sees gaps when a
plugin is unhealthy, but the daemon's own metrics (CPU, memory,
LLM, MCP, tunnels) keep flowing.

## Implementing the plugin side

Subprocess plugins subscribe to
`<broker_topic_prefix>.metrics.scrape` and reply:

```rust
// Sketch (final SDK helpers ship with the next plugin release):
ctx.broker
    .subscribe("plugin.<id>.metrics.scrape")
    .await?
    .for_each(|msg| async {
        let text = my_metrics_module::render_prometheus(...);
        broker.publish(
            &msg.reply_to.unwrap(),
            json!({ "text": text }),
        ).await
    });
```

Reference impl lands with the next `nexo-plugin-email` release;
until then the daemon's legacy hardcoded call keeps email
metrics flowing.

## Migration status

- `nexo-plugin-email` — NOT migrated. Legacy in-process call at
  `src/main.rs:15295` continues to serve. When email ships the
  manifest section, BOTH paths fire (legacy direct call AND
  broker scrape) until the legacy call is retired in a
  follow-up.
- Other canonical plugins — none currently expose Prometheus
  metrics. New plugins opting into metrics declare the manifest
  section directly with no legacy fallback to maintain.

## Validation

- `cargo build --release-fast --bin nexo` (default) — 3m clean.
- `cargo build --release-fast --bin nexo --no-default-features`
  — 3m01s clean.
- `cargo nextest run --workspace` — 6321/6321 (5 new tests in
  `plugin_metrics::tests` covering descriptor construction,
  empty-descriptors short-circuit, failure isolation across
  multiple plugins, broker error path).
- `mdbook build docs` clean.

## Trade-offs

| Concern | Decision |
|---|---|
| Sequential vs concurrent scrape | Sequential. Concurrent would shave latency for `n > 3` plugins but adds a `futures` dep edge. Acceptable at current scale (≤10 plugins typical). |
| Per-scrape timeout | 5s default. Plugins exceeding this contribute empty (warn-log). Trades immediate visibility for daemon `/metrics` SLO. |
| Duplicate metric name collisions | Daemon does NOT deduplicate. Plugins namespace with `my_plugin_<metric>` prefix per Prometheus convention. |
| Plugin reply shape | `{ text: String }`. Simple envelope; daemon appends newline if missing. Adding `labels` / `timestamps` would be a follow-up if a plugin needs them. |
| Email migration fallback | Legacy `nexo_plugin_email::metrics::render_prometheus` call kept. When email ships manifest section both paths run until cleanup follow-up retires the legacy. |
