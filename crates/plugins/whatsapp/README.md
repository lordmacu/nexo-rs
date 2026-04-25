# nexo-plugin-whatsapp

> WhatsApp channel plugin (wa-agent + Signal Protocol) for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- WhatsApp integration via the [`wa-agent`](https://crates.io/crates/wa-agent) crate — Signal Protocol, QR pairing, multi-device.
- **Inbound bridge**: incoming messages → agent runtime → LLM turn.
- **Outbound dispatch** with media (images, audio, video, documents).
- **Voice transcriber** so audio messages turn into text the LLM can read.
- **Pairing flow** with operator-friendly QR code rendering.
- Per-agent credentials via [`nexo-auth`](https://crates.io/crates/nexo-auth) — multi-tenant by design.

## Install

```toml
[dependencies]
nexo-plugin-whatsapp = "0.1"
```

## Documentation for this crate

- [WhatsApp plugin](https://lordmacu.github.io/nexo-rs/plugins/whatsapp.html)
- [Recipe — WhatsApp sales agent](https://lordmacu.github.io/nexo-rs/recipes/whatsapp-sales-agent.html)
- [ADR — WhatsApp via whatsapp-rs](https://lordmacu.github.io/nexo-rs/adr/0007-whatsapp-signal-protocol.html)
- [Pairing](https://lordmacu.github.io/nexo-rs/ops/pairing.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
