# nexo-llm

> LLM provider clients (MiniMax, OpenAI-compat, Anthropic, Gemini) with rate limiter and tool registry.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Pluggable `LlmClient` trait — swap providers without touching the agent runtime.
- Built-in providers: **MiniMax M2.5** (primary), **OpenAI-compatible** (any OpenAI-format endpoint), **Anthropic Claude** (API key *and* subscription/OAuth flow), **Gemini**, **DeepSeek**.
- **Tool registry** with JSON-schema validation, per-binding tool gating, parallel tool calls.
- **Rate limiter + retry** wired through `nexo-resilience` (LLM 429 → 5 attempts 1s→60s exp, 5xx → 3 attempts 2s→30s).
- Streaming responses, prompt caching helpers (Anthropic), and reasoning-mode passthrough.

## Install

```toml
[dependencies]
nexo-llm = "0.1"
```

## Documentation for this crate

- [MiniMax M2.5](https://lordmacu.github.io/nexo-rs/llm/minimax.html)
- [Anthropic / Claude](https://lordmacu.github.io/nexo-rs/llm/anthropic.html)
- [OpenAI-compatible](https://lordmacu.github.io/nexo-rs/llm/openai.html)
- [DeepSeek](https://lordmacu.github.io/nexo-rs/llm/deepseek.html)
- [Rate limiting & retry](https://lordmacu.github.io/nexo-rs/llm/retry.html)
- [llm.yaml](https://lordmacu.github.io/nexo-rs/config/llm.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
