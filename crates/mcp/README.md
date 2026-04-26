# nexo-mcp

> Model Context Protocol (MCP) implementation for Nexo — both **client** (consume external MCP servers as tools) and **server** (expose this agent's capabilities to other MCP clients like Claude Desktop / Cursor / Zed).

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

### MCP client (consume external servers)

- **stdio transport** — spawn an MCP server as a subprocess,
  communicate over JSON-RPC 2.0 + line-delimited stdio.
  Built on `nexo_extensions::runtime::wire`.
- **HTTP transport** — connect to a remote MCP server over
  HTTPS with bearer-auth. CircuitBreaker-wrapped.
- **Tool catalog** — fetched via `tools/list`, registered into
  the agent's tool registry under a namespace. The LLM sees
  them as regular `mcp.<server>.<tool>` calls.
- **Resources + sampling** — `resources/read`, `resources/list`,
  and the sampling protocol that lets an MCP server delegate
  LLM completions back to the agent.
- **Hot-reload `tools/list_changed`** — Phase 12.8 listens for
  the upstream notification and swaps the tool registry
  atomically.

### MCP server (expose this agent)

- **`agent mcp-server`** subcommand spins up the agent in
  server mode without the full runtime, so an external client
  (Claude Desktop, Cursor) can consume it as a single MCP
  endpoint.
- Exposes the agent's tools + a `recall` resource that returns
  long-term memory excerpts on demand.

## Configuration

```yaml
# config/mcp.yaml
mcp:
  servers:
    - name: filesystem
      transport: stdio
      command: npx
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/home/me"]
    - name: github
      transport: http
      url: https://mcp.example.com/github
      bearer_token_file: /run/secrets/mcp_github_token
```

```yaml
# config/mcp_server.yaml
mcp_server:
  bind: 127.0.0.1:7099
  agent_id: kate            # which agent this server represents
  tools_allowlist: [memory_recall, web_search]
```

## Install

```toml
[dependencies]
nexo-mcp = "0.1"
```

## Documentation for this crate

- [MCP introduction](https://lordmacu.github.io/nexo-rs/mcp/introduction.html)
- [MCP client (stdio / HTTP)](https://lordmacu.github.io/nexo-rs/mcp/client.html)
- [Agent as MCP server](https://lordmacu.github.io/nexo-rs/mcp/server.html)
- [MCP from Claude Desktop recipe](https://lordmacu.github.io/nexo-rs/recipes/mcp-from-claude-desktop.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
