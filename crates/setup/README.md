# nexo-setup

> Setup wizard, capability inventory, and doctor command for Nexo.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Interactive setup wizard** — pairs WhatsApp, configures LLM providers, writes `agents.yaml` from prompts.
- **Doctor command** (`nexo doctor capabilities`): inventory of every dangerous env toggle currently armed in the operator's shell.
- **Capability inventory** is the source of truth for the admin UI's capabilities tab — every new `*_ALLOW_*` / `*_REVEAL` env var registers here.

## Install

```toml
[dependencies]
nexo-setup = "0.1"
```

## Documentation for this crate

- [Setup wizard](https://lordmacu.github.io/nexo-rs/getting-started/setup-wizard.html)
- [Capability toggles](https://lordmacu.github.io/nexo-rs/ops/capabilities.html)
- [Quick start](https://lordmacu.github.io/nexo-rs/getting-started/quickstart.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
