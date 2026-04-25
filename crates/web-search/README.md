# nexo-web-search

> Multi-provider web search client (Brave, Serper, etc.) for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Single `WebSearch` trait with multiple provider implementations: **Brave Search**, **Serper**, more.
- Per-agent / per-binding policy: pick the provider, set quotas, cache results.
- Wired into the `web_search` agent tool so the LLM can issue searches as part of a turn.

## Install

```toml
[dependencies]
nexo-web-search = "0.1"
```

## Documentation for this crate

- [Web search](https://lordmacu.github.io/nexo-rs/ops/web-search.html)
- [Link understanding](https://lordmacu.github.io/nexo-rs/ops/link-understanding.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
