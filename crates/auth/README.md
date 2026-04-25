# nexo-auth

> Per-agent credential resolver and gauntlet validation for Nexo channels.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Per-agent credential stores for WhatsApp, Telegram, and Google APIs — each agent ships with its own keys.
- **Boot gauntlet**: validates required scopes / pairing state before the runtime accepts traffic.
- Resolver API used by channel plugins to fetch the right credentials for the right agent at runtime.
- Telemetry hooks (counters, audit logs) so operators can see which agent used which credential when.

## Install

```toml
[dependencies]
nexo-auth = "0.1"
```

## Documentation for this crate

- [Per-agent credentials](https://lordmacu.github.io/nexo-rs/config/credentials.html)
- [Capability toggles](https://lordmacu.github.io/nexo-rs/ops/capabilities.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
