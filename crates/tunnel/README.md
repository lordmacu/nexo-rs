# nexo-tunnel

> Tunneling helpers for Nexo agents (Termux and local exposure).

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Helpers to expose local Nexo services for development — useful when running on Termux (Android) and needing inbound webhooks.
- Wraps common tunnel CLIs and surfaces their public URL so config can wire it into webhook providers.

## Install

```toml
[dependencies]
nexo-tunnel = "0.1"
```

## Documentation for this crate

- [Termux install](https://lordmacu.github.io/nexo-rs/getting-started/install-termux.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
