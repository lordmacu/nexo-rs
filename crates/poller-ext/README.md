# nexo-poller-ext

> Extension integration for the Nexo poller runtime.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Glue between the Nexo extension system and the poller runtime — extensions can register their own pollers via the manifest.
- Extension-declared pollers run with the same retry/ack semantics as built-ins.

## Install

```toml
[dependencies]
nexo-poller-ext = "0.1"
```

## Documentation for this crate

- [pollers.yaml](https://lordmacu.github.io/nexo-rs/config/pollers.html)
- [Extensions manifest](https://lordmacu.github.io/nexo-rs/extensions/manifest.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
