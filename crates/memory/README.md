# nexo-memory

> Short-term, long-term (SQLite), and vector (sqlite-vec) memory layers for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Short-term**: ring-buffer of recent turns, fed straight into the LLM context window.
- **Long-term**: SQLite-backed transcript store with FTS5 full-text search and redaction.
- **Vector recall**: `sqlite-vec` for semantic search — *zero extra infrastructure*, runs in the same SQLite file.
- **Memory tool**: LLM-callable tool to write, recall, and forget — the agent decides what to keep.
- Recall signal scoring (recency × relevance × salience) to decide what enters the prompt.

## Install

```toml
[dependencies]
nexo-memory = "0.1"
```

## Documentation for this crate

- [Short-term](https://lordmacu.github.io/nexo-rs/memory/short-term.html)
- [Long-term (SQLite)](https://lordmacu.github.io/nexo-rs/memory/long-term.html)
- [Vector search](https://lordmacu.github.io/nexo-rs/memory/vector.html)
- [memory.yaml](https://lordmacu.github.io/nexo-rs/config/memory.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
