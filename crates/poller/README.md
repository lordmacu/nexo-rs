# nexo-poller

> Generic polling runtime for Nexo — cron-style schedules, durable cursors, retry/DLQ semantics, single-instance leases. Powers gmail / rss / webhook / google-calendar / agent_turn built-ins plus extension-loaded pollers.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Cron-style scheduler** — each job has a cron expression +
  jitter; the runtime ticks every minute and dispatches due
  jobs to their declared poller.
- **Durable cursors** — every successful tick persists a per-
  job cursor (timestamp, history-id, message-id, etc.) so a
  restart resumes from where it left off.
- **Single-instance lease** — cross-process SQLite lease
  prevents two daemons from double-ticking the same job.
- **Retry / DLQ classification** — poller responses with
  `error_kind: "transient"` retry with exponential backoff;
  `permanent` go to a per-job DLQ; `skipped` count as
  successful no-ops.
- **Built-in pollers**:
  - `gmail` — Gmail history sync via Google API
  - `rss` — Atom + RSS feed polling with feed-aware caching
  - `webhook_poll` — GET-and-classify against a webhook
  - `google_calendar` — calendar event watching
  - `agent_turn` (Phase 20) — synthetic LLM turn delivered
    to a channel on cron schedule
- **Extension hook** — `nexo-poller-ext` lets third-party
  extensions register their own poller kinds via the
  `capabilities.pollers` manifest field.
- **`PollContext.llm_*`** — every poller has access to the
  agent's LLM client + tool registry so it can compose
  agent-driven reactions (e.g. agent_turn).

## Public API

```rust
pub trait Poller: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn tick(&self, ctx: &PollContext) -> Result<TickOutcome, PollerError>;
}

pub struct PollerRunner { /* … */ }

impl PollerRunner {
    pub fn new(state: Arc<dyn PollerStore>) -> Self;
    pub fn register(&self, poller: Arc<dyn Poller>);
    pub fn registered_kinds(&self) -> Vec<&'static str>;
    pub async fn run(&self, cfg: &PollersConfig);
}
```

## Configuration

```yaml
# config/pollers.yaml
pollers:
  jobs:
    - id: kate-rss-news
      kind: rss
      cron: "*/15 * * * *"
      enabled: true
      args:
        urls: ["https://example.com/rss"]
      delivery:
        agent: kate
        channel: whatsapp
        instance: primary
```

## Install

```toml
[dependencies]
nexo-poller = "0.1"
```

## Documentation for this crate

- [Pollers config](https://lordmacu.github.io/nexo-rs/config/pollers.html)
- [Build a poller module](https://lordmacu.github.io/nexo-rs/recipes/build-a-poller.html)
- [DLQ](https://lordmacu.github.io/nexo-rs/ops/dlq.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
