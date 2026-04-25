# nexo-resilience

> Circuit breaker, retry, and rate limiter primitives for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Circuit breaker** with closed → open → half-open recovery, configurable failure thresholds and cooldowns.
- **Retry executor** with exponential backoff, jitter, and pluggable per-error-class policies (LLM 429 vs 5xx vs network).
- **Token-bucket rate limiter** for outbound LLM and HTTP calls.
- Building blocks reused by every external-call site in Nexo (LLM clients, NATS broker, plugins).

## Install

```toml
[dependencies]
nexo-resilience = "0.1"
```

## Documentation for this crate

- [Fault tolerance](https://lordmacu.github.io/nexo-rs/architecture/fault-tolerance.html)
- [LLM rate limiting & retry](https://lordmacu.github.io/nexo-rs/llm/retry.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
