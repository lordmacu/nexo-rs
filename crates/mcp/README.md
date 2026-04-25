# nexo-mcp

> MCP (Model Context Protocol) client and server runtime for Nexo agents.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main project:** <https://github.com/lordmacu/nexo-rs>
- **Documentation:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **MCP client** for both stdio and HTTP transports — connect to any MCP server and surface its tools to the agent.
- **Tool catalog** with `tools/list_changed` hot-reload (the catalog updates without restarting the agent).
- **Agent-as-MCP-server**: expose any Nexo agent as an MCP server so other clients (Claude Desktop, IDEs) can call it.
- **MCP-in-extensions**: extensions can ship their own MCP server and the agent picks it up automatically.
- Resource subscriptions, prompts, and sampling support.

## Install

```toml
[dependencies]
nexo-mcp = "0.1"
```

## Documentation for this crate

- [MCP introduction](https://lordmacu.github.io/nexo-rs/mcp/introduction.html)
- [Client (stdio / HTTP)](https://lordmacu.github.io/nexo-rs/mcp/client.html)
- [Agent as MCP server](https://lordmacu.github.io/nexo-rs/mcp/server.html)
- [Recipe — MCP from Claude Desktop](https://lordmacu.github.io/nexo-rs/recipes/mcp-from-claude-desktop.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
