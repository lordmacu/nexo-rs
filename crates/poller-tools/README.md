# nexo-poller-tools

> Built-in tools and builders for the Nexo poller (LLM turn, channel ops).

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`agent_turn` builtin**: cron-driven LLM turn that writes its reply to a channel — the simplest way to make an agent proactive.
- **Channel-send** builders for direct cron-scheduled outbound messages.
- Reusable `with_llm` builder so custom pollers inherit the same LLM/rate-limit/retry stack.

## Install

```toml
[dependencies]
nexo-poller-tools = "0.1"
```

## Documentation for this crate

- [pollers.yaml](https://lordmacu.github.io/nexo-rs/config/pollers.html)
- [Recipe — Build a poller module](https://lordmacu.github.io/nexo-rs/recipes/build-a-poller.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
