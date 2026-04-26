# nexo-broker

> Event-bus abstraction for Nexo — `BrokerHandle` trait with NATS impl + zero-config local fallback (in-process `tokio::mpsc` + disk queue) so a daemon boots and works even before NATS is reachable.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`BrokerHandle` trait** — uniform `publish(topic, event)`,
  `subscribe(pattern) -> Subscription`, `request(topic, msg,
  timeout) -> Message` over both transports.
- **`NatsBroker`** — `async-nats = "0.35"` client wrapping
  NATS Core (no JetStream required for the basic event bus;
  Phase 32 multi-host work adds JetStream layered).
- **`LocalBroker`** — in-process `DashMap<pattern,
  tokio::mpsc::Sender>` fallback with topic-pattern matching
  (`agent.*.inbox`, `plugin.outbound.>`). Drops messages with
  a slow-consumer counter when a subscriber is backlogged
  (NATS-equivalent semantics).
- **`AnyBroker` enum** — `Local | Nats` discriminator the
  runtime holds; the rest of the codebase calls
  `BrokerHandle` methods polymorphically.
- **Disk queue fallback** — when configured for NATS but the
  remote is unreachable, outbound publishes spool to a SQLite
  disk queue and replay on reconnect (Phase 2.3).
- **Dead-letter queue (DLQ)** — `nexo dlq list / replay /
  drop` CLI surface; messages that fail max-retries land
  here for operator triage.
- **CircuitBreaker** — Phase 2.5 wraps NATS publishes;
  back-pressure on the publisher when the cluster is
  unhealthy.

## Public API

```rust
#[async_trait]
pub trait BrokerHandle: Send + Sync {
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError>;
    async fn subscribe(&self, pattern: &str) -> Result<Subscription, BrokerError>;
    async fn request(&self, topic: &str, msg: Message, timeout: Duration) -> Result<Message, BrokerError>;
}

pub enum AnyBroker {
    Local(LocalBroker),
    Nats(Arc<NatsBroker>),
}
```

## Topic naming convention

| Topic shape | Producer | Consumer |
|---|---|---|
| `agent.route.<agent_id>` | Agent-to-agent delegation | Target runtime |
| `agent.inbound.<agent_id>` | Channel plugins | Agent runtime |
| `plugin.inbound.<channel>[.<instance>]` | Channel plugins | Agent runtime |
| `plugin.outbound.<channel>[.<instance>]` | Agent runtime / send-tools | Channel plugin |
| `taskflow.resume` | TaskFlow callers | WaitEngine |
| `pair.codes.<code>` | Pairing handshake | Pairing gate |

## Install

```toml
[dependencies]
nexo-broker = "0.1"
```

## Documentation for this crate

- [Event bus (NATS)](https://lordmacu.github.io/nexo-rs/architecture/event-bus.html)
- [Fault tolerance](https://lordmacu.github.io/nexo-rs/architecture/fault-tolerance.html)
- [DLQ](https://lordmacu.github.io/nexo-rs/ops/dlq.html)
- [NATS with TLS + auth recipe](https://lordmacu.github.io/nexo-rs/recipes/nats-tls-auth.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
