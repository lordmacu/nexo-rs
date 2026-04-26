# nexo-poller-ext

> Bridge between the [`nexo-poller`](https://github.com/lordmacu/nexo-rs/tree/main/crates/poller) runtime and the [`nexo-extensions`](https://github.com/lordmacu/nexo-rs/tree/main/crates/extensions) system — lets a third-party extension declare its own poller kind and have it discovered + scheduled by the runtime.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`capabilities.pollers` manifest field** — extensions declare
  `[[capabilities.pollers]]` in their `plugin.toml` with a
  `kind` and JSON-schema for args.
- **Discovery + registration** — at boot, the runtime walks
  every loaded extension, collects declared poller kinds, and
  registers them alongside the built-in pollers (gmail, rss,
  webhook_poll, google_calendar, agent_turn).
- **Wire protocol** — extension-loaded pollers receive their
  scheduled tick via stdio / NATS the same way regular extension
  tools do; the poller runtime translates `tick` events into
  the extension's standard message envelope.
- **Outcome classification** — extension responses with `error_kind:
  "transient"` retry; `permanent` go to DLQ. Anything else is
  treated as transient.

## Why a separate crate

`nexo-poller` doesn't depend on `nexo-extensions` (avoiding a
dependency cycle: `extensions` already pulls in transport
primitives, and `poller` is built to be transport-agnostic).
This bridge is the only thing that knows about both, so the
runtime imports `nexo-poller-ext` once at boot wiring time.

## Install

```toml
[dependencies]
nexo-poller-ext = "0.1"
```

## Documentation for this crate

- [Pollers](https://lordmacu.github.io/nexo-rs/config/pollers.html)
- [Build a poller module](https://lordmacu.github.io/nexo-rs/recipes/build-a-poller.html)
- [Extensions manifest](https://lordmacu.github.io/nexo-rs/extensions/manifest.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
