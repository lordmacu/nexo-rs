# template-mcp-server

Skeleton MCP server for Nexo. Copy → rename → swap the `Echo` tool
for your domain logic. Ships **stdio always**, **HTTP optional**
via env var, and shows the
[`McpServerBuilder`](../../crates/mcp/src/server/builder.rs)
ergonomic path with typed `Args` + `Output` (Phase 76.9).

## What this template is

A **starting point** for building a domain-specific MCP server
(e.g. `nexo-marketing`, `nexo-crm`). It demonstrates:

- One typed tool (`Echo`) using `#[derive(JsonSchema)]` — the
  schema is computed once at registration, never hand-rolled.
- Stdio transport — what the agent's extension supervisor pipes
  to the child process.
- Optional HTTP+SSE transport — for direct testing with `curl`,
  the official `claude mcp` CLI, or any browser client.
- Graceful SIGTERM handling.

What this template is **not**: a fully wired multi-tenant
production server. It deliberately stops short of multi-tenant +
audit + rate-limit so the diff stays under 250 LOC. The
production knobs are documented in `config.example.yaml` and the
[operator guide](../../docs/src/extensions/mcp-server.md).

## Quickstart

```bash
# In-tree dev (this repo's workspace):
cargo run -p template-mcp-server

# With HTTP on:
MCP_TEMPLATE_HTTP_BIND=127.0.0.1:7676 cargo run -p template-mcp-server

# With static bearer token:
MCP_TEMPLATE_HTTP_BIND=127.0.0.1:7676 \
MCP_TEMPLATE_HTTP_TOKEN=secret \
  cargo run -p template-mcp-server
```

Smoke test the HTTP transport:

```bash
curl -s -X POST http://127.0.0.1:7676/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}'
```

The response includes `Mcp-Session-Id:` in the headers — pass it
on subsequent calls to keep the session alive.

## Forking the template

1. **Copy the directory out of this repo:**
   ```bash
   cp -r extensions/template-mcp-server ~/code/nexo-marketing
   cd ~/code/nexo-marketing
   ```

2. **Edit `Cargo.toml`** — bump `name`, drop `publish = false`,
   and switch the `nexo-mcp` path dep to a published version:
   ```toml
   nexo-mcp = "0.1.1"   # was: { path = "../../crates/mcp" }
   ```
   Once Phase 27 (release pipeline) ships, the published version
   tracks the version pinned in this repo's
   [`PHASES.md`](../../PHASES.md) header.

3. **Edit `plugin.toml`** — bump `id`, `name`, add your tools to
   `[capabilities].tools`. Drop the `transport.command` line if
   the agent doesn't fork the binary itself.

4. **Edit `src/tools.rs`** — replace `Echo` with your domain
   tools. Each tool is `impl Tool` with typed `Args` + `Output`.
   The builder caches the JSON Schema once; runtime cost is a
   `BTreeMap::get` per call.

5. **Wire the agent** — add the new server to your agent's
   `config/mcp_server.yaml` (or `config/agents.yaml` if it should
   be spawned as a child). The
   [operator guide](../../docs/src/extensions/mcp-server.md)
   covers auth + multi-tenant + rate-limit + audit + resume.

## Production checklist

Before exposing a forked server to the internet:

- [ ] **Bind off-loopback** only with auth configured. The HTTP
      runtime refuses to boot a non-loopback bind without auth +
      a non-`*` `allow_origins` list.
- [ ] **Audit log on**: uncomment the `audit_log:` block in
      `config.example.yaml` (Phase 76.11). Survives daemon
      restart, append-only, SQLite-backed.
- [ ] **Per-principal rate-limit on** (Phase 76.5) for any tool
      that calls a paid API or hits a third-party rate limit.
- [ ] **Per-principal concurrency cap** (Phase 76.6) for tools
      that stream long-running output.
- [ ] **Session event store on** (Phase 76.8) if your clients
      hold long-lived SSE connections — lets them resume via
      `Last-Event-ID` after a reconnect instead of
      re-`initialize`-ing.
- [ ] **TLS in front**: terminate at nginx / caddy / Traefik.
      Direct `rustls` is parked as Phase 76.13; for now treat
      the binary as cleartext-on-loopback by design.

## Reference patterns mined for this template

- `claude-code-leak/src/entrypoints/mcp.ts:35-78` — minimal
  stdio MCP server in the leaked Claude Code source. The leak
  uses imperative `setRequestHandler(SchemaName, async (req) => …)`
  per spec method; we collapse that into one
  `McpServerBuilder::tool(impl Tool)` chain.
- `claude-code-leak/src/utils/computerUse/mcpServer.ts:60-78` —
  factory pattern returning the configured `Server`. Our
  `build_handler()` closure plays the same role.
- `crates/mcp/src/bin/mock_mcp_server.rs` — exhaustive
  reference for protocol corner cases. Read it when this
  template's surface stops being enough.
- `research/extensions/firecrawl/` — OpenClaw extension
  reference. Different model (provider contracts, TypeScript),
  but informs the file layout we settled on for `plugin.toml`.

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `cargo build` fails on `nexo-mcp` resolve | Template path dep points inside this repo | Either build inside the repo's workspace, or swap the dep to `nexo-mcp = "0.1.1"` in Cargo.toml |
| HTTP returns 404 + `-32001` mid-session | Daemon restarted; in-mem session is gone | Client must re-`initialize`. If using `session_event_store`, events + subs persist on disk and replay on the next session id |
| HTTP 503 with `Retry-After` header | Hit `max_sessions` cap | Raise `max_sessions` in the agent's `mcp_server.http` block or reduce `session_idle_timeout_secs` |
| `tools/call` returns `code: -32602` (invalid params) | Args don't match the tool's typed `Args` schema | Call `tools/list` and inspect `input_schema`; the generated schema reflects the `JsonSchema` derive |
