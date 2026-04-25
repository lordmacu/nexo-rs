# nexo-plugin-google

> Google APIs (Gmail, Calendar, Drive) tool plugin for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- LLM-callable tools backed by Google Workspace APIs:
-   - **Gmail**: read, search, send, draft.
-   - **Calendar**: list, create, update, free/busy.
-   - **Drive**: list, read, upload.
- OAuth2 with per-agent scopes and refresh-token rotation via [`nexo-auth`](https://crates.io/crates/nexo-auth).

## Install

```toml
[dependencies]
nexo-plugin-google = "0.1"
```

## Documentation for this crate

- [Google plugin](https://lordmacu.github.io/nexo-rs/plugins/google.html)
- [Per-agent credentials](https://lordmacu.github.io/nexo-rs/config/credentials.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
