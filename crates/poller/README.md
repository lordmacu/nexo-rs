# nexo-poller

> Generic polling runtime: cron schedules, retries, ack semantics.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Cron-style polling primitives — declare a schedule, get a callback on every tick.
- **Retry & backoff** for transient failures, **ack/nack** for at-least-once semantics.
- Pluggable `PollContext` so a poller can borrow runtime services (LLM client, broker, memory) without re-wiring them.
- Used to schedule LLM turns, sync external state, run reminders.

## Install

```toml
[dependencies]
nexo-poller = "0.1"
```

## Documentation for this crate

- [pollers.yaml](https://lordmacu.github.io/nexo-rs/config/pollers.html)
- [Recipe — Build a poller module](https://lordmacu.github.io/nexo-rs/recipes/build-a-poller.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
