# nexo-poller-tools

> Reusable poller building blocks — `OutboundDelivery`, `LlmTurnBuilder`, `MirroredFlow` — that built-in pollers (gmail, rss, agent_turn, google_calendar, webhook_poll) and extension-loaded pollers compose to deliver scheduled work to agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`OutboundDelivery`** — uniform "ship this to channel X with
  this body" primitive every poller eventually emits. Renders
  to the same `plugin.outbound.<channel>` envelope a manual
  send-tool emits.
- **`LlmTurnBuilder`** — composes a synthetic LLM turn from a
  pollable event (e.g. an inbound RSS item) so the agent
  reasons over it the same way it would a user message.
  Phase 20 `agent_turn` poller builds on this.
- **`MirroredFlow`** — keeps a TaskFlow record in sync when a
  poller produces work that should also be tracked durably.
  The host sees a tick, asks `MirroredFlow` to record + then
  the runtime's TaskFlow tools see the same flow ID.
- **Result classification helpers** — `classify_transient`,
  `classify_permanent`, `classify_skipped` for poller authors
  who want consistent retry semantics.

## Why a separate crate

`nexo-poller` is the core scheduler + cursor + DLQ runtime;
`nexo-poller-tools` is what built-ins + extensions use to
*produce* events. Splitting them lets the scheduler stay
transport-agnostic while the tools side carries the
opinionated "this is what an inbound poller event looks like"
shape.

## Install

```toml
[dependencies]
nexo-poller-tools = "0.1"
```

## Documentation for this crate

- [Pollers](https://lordmacu.github.io/nexo-rs/config/pollers.html)
- [Build a poller module](https://lordmacu.github.io/nexo-rs/recipes/build-a-poller.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
