# nexo-config

> YAML configuration loader with env var resolution for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- Loads layered YAML files (`agents.yaml`, `llm.yaml`, `broker.yaml`, `memory.yaml`, `plugins/*.yaml`).
- Resolves `${ENV_VAR}` placeholders so secrets stay out of YAML — values read from env or Docker secrets at boot.
- Schema validation with friendly errors (which field, which file, what was expected).
- Hot-reload via `notify`: subscribe to `RuntimeSnapshot` updates and react atomically with `ArcSwap`.

## Install

```toml
[dependencies]
nexo-config = "0.1"
```

## Documentation for this crate

- [Config layout](https://lordmacu.github.io/nexo-rs/config/layout.html)
- [agents.yaml](https://lordmacu.github.io/nexo-rs/config/agents.html)
- [Hot-reload](https://lordmacu.github.io/nexo-rs/ops/hot-reload.html)
- [Drop-in agents](https://lordmacu.github.io/nexo-rs/config/drop-in.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
