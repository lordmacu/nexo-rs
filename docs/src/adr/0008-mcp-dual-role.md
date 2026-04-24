# ADR 0008 — MCP dual role: client and server

**Status:** Accepted
**Date:** 2026-03

## Context

Model Context Protocol is becoming the de facto integration surface
for LLM-driven tools. Two questions arose during the Phase 12
design:

1. Should the agent be an **MCP client** (consume external MCP
   servers as tools)?
2. Should the agent be an **MCP server** (expose its own tools to
   external MCP clients like Claude Desktop, Cursor, Zed)?

These are independent decisions. Picking one does not force the
other.

## Decision

**Do both.** Same process, same `ToolRegistry`, different transports.

- **Client** — `McpRuntimeManager` spawns stdio or HTTP MCP servers
  per session (with a shared "sentinel session" for servers that
  don't need per-session isolation). Their tools register into the
  per-session `ToolRegistry` with names like
  `{server_name}_{tool_name}` and are callable by the agent like any
  built-in
- **Server** — `agent mcp-server` subcommand reads JSON-RPC from
  stdin and writes responses to stdout. An `mcp_server.yaml`
  allowlist controls which tools are exposed. Configurable
  `auth_token_env` guards the `initialize` call when the server is
  exposed through a tunnel

Both sides speak MCP 2024-11-05 (streamable HTTP) with SSE fallback
for legacy servers.

## Consequences

**Positive**

- Being a client: any MCP-speaking tool ecosystem is reachable
  without writing a custom extension
- Being a server: the agent's tools + memory become available inside
  Claude Desktop / Cursor / Zed — cross-session memory, remote
  actions, etc.
- Interop with the broader MCP catalog is a configuration change,
  not a code change

**Negative**

- Two independent code paths to keep current as the MCP spec evolves
- `expose_proxies` configuration gotcha: enabling it on the server
  side makes every upstream MCP server transitively visible to the
  consuming client. Default is `false` and the docs call this out
  explicitly
- MCP spec churn (2024-11-05 vs future versions) needs staying power

## Related

- [MCP — introduction](../mcp/introduction.md)
- [MCP — client](../mcp/client.md)
- [MCP — agent as server](../mcp/server.md)
- [Recipes — MCP from Claude Desktop](../recipes/mcp-from-claude-desktop.md)
