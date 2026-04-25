# nexo-pairing

> Setup-code pairing store and DM-challenge gate for Nexo channel plugins.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Setup-code generation + verification (HMAC-backed, constant-time compare).
- Persistent pairing store (SQLite via `sqlx`) for paired peers, challenges, and revocations.
- DM-challenge gate that channel plugins call before forwarding traffic from an unpaired peer.
- QR-code rendering (text + optional PNG via the `qr-png` feature) for setup-code handoff.

## Install

```toml
[dependencies]
nexo-pairing = "0.1"
```

Disable the PNG renderer if you only need text QR:

```toml
nexo-pairing = { version = "0.1", default-features = false }
```

## Documentation for this crate

- [Pairing protocol](https://lordmacu.github.io/nexo-rs/ops/pairing.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
