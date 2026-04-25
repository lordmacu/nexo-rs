# nexo-plugin-email

> Email (IMAP/SMTP) channel plugin for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Generic IMAP inbound + SMTP outbound — works with Gmail, Outlook, Fastmail, self-hosted.
- Per-agent mailbox credentials via [`nexo-auth`](https://crates.io/crates/nexo-auth).
- Threading-aware: replies stay in the same conversation thread.

## Install

```toml
[dependencies]
nexo-plugin-email = "0.1"
```

## Documentation for this crate

- [Email plugin](https://lordmacu.github.io/nexo-rs/plugins/email.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
