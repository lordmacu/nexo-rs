# nexo-core

> Agent runtime: event bus, sessions, plugin trait, heartbeat, A2A delegation.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Agent trait** + dispatcher: turns inbound events into LLM turns, tool calls, and channel responses.
- **Plugin trait**: contract every channel plugin implements (`on_event`, `on_send`, lifecycle hooks).
- **SessionManager**: per-conversation state, transcript persistence, FTS-indexed search, redaction.
- **Heartbeat runtime** for proactive turns (reminders, periodic checks, external-state polling).
- **Agent-to-agent (A2A) routing**: `agent.route.{target_id}` topic + `correlation_id` for request/response between agents.
- Per-binding capability overrides — restrict skills, tools, or LLM models per agent×channel pair.

## Install

```toml
[dependencies]
nexo-core = "0.1"
```

## Documentation for this crate

- [Architecture overview](https://lordmacu.github.io/nexo-rs/architecture/overview.html)
- [Agent runtime](https://lordmacu.github.io/nexo-rs/architecture/agent-runtime.html)
- [Transcripts](https://lordmacu.github.io/nexo-rs/architecture/transcripts.html)
- [Recipe — Agent-to-agent delegation](https://lordmacu.github.io/nexo-rs/recipes/agent-to-agent.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
