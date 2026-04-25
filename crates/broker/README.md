# nexo-broker

> Async NATS broker abstraction with local fallback and disk queue for Nexo.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Wraps `async-nats = 0.35` behind a `Broker` trait — agents publish and subscribe without coupling to NATS specifics.
- Automatic **local fallback**: when NATS is offline, traffic flows through a `tokio::mpsc` bus + on-disk queue, then drains to NATS on reconnect.
- **Circuit breaker** on every publish/subscribe so a flapping broker doesn't take down the agent.
- **Dead-letter queue** for messages that exceed retry budget.
- **Backpressure** via bounded channels so a slow subscriber can't OOM the producer.

## Install

```toml
[dependencies]
nexo-broker = "0.1"
```

## Documentation for this crate

- [Event bus (NATS)](https://lordmacu.github.io/nexo-rs/architecture/event-bus.html)
- [Fault tolerance](https://lordmacu.github.io/nexo-rs/architecture/fault-tolerance.html)
- [DLQ](https://lordmacu.github.io/nexo-rs/ops/dlq.html)
- [Recipe — NATS with TLS + auth](https://lordmacu.github.io/nexo-rs/recipes/nats-tls-auth.html)
- [broker.yaml](https://lordmacu.github.io/nexo-rs/config/broker.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
