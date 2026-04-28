# Building an MCP server extension

> **Phase 76.15** — operator-friendly walk-through for forking the
> [`template-mcp-server`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-mcp-server)
> skeleton into a domain-specific MCP server (e.g.
> `nexo-marketing`, `nexo-crm`). The companion chapter
> [HTTP+SSE transport](./mcp-server.md) documents the production
> knobs (auth, multi-tenant, rate-limit, audit, resume); this
> chapter is the developer's quickstart.

## When to build an MCP server extension

You want one when:

- You have a **domain** (marketing, CRM, billing, ops) with its
  own tools, types, and access policy that should NOT live inside
  the agent process.
- You want **separate deployment + auth** — for example, the
  marketing team owns the marketing MCP server and exposes it on
  their VPC; the agent process is shared infrastructure.
- You want **third-party access** — Claude Code, Cursor, custom
  scripts, or another agent connect over HTTP+SSE while the agent
  proxies through the same surface.

You DON'T want one when:

- The capability is shared by every agent in the workspace —
  ship it as a built-in tool inside `crates/core/`.
- The capability is a thin wrapper over one HTTP API — ship it as
  an [agent extension](./templates.md) (stdio JSON-RPC) instead.
  MCP servers carry per-call auth + rate-limit + audit overhead
  that's wasted on a single-tenant private endpoint.

## The skeleton — `extensions/template-mcp-server/`

```
template-mcp-server/
├── Cargo.toml             # depends on nexo-mcp (path dep in-tree, crates.io after copy)
├── plugin.toml            # extension manifest (id, capabilities)
├── config.example.yaml    # documented HTTP block ready to paste
├── README.md              # quickstart + production checklist
└── src/
    ├── main.rs            # boot stdio + optional HTTP via env var
    └── tools.rs           # one typed Echo tool using McpServerBuilder
```

The whole skeleton is **under 250 LOC of Rust**. It deliberately
stops short of multi-tenant + audit + rate-limit so the diff stays
readable; everything you need to enable those is in
`config.example.yaml` plus pointers to the operator chapter.

## The 5-step fork

### 1. Copy + rename

```bash
cp -r extensions/template-mcp-server ~/code/nexo-marketing
cd ~/code/nexo-marketing
```

### 2. `Cargo.toml`

- Bump `name` to `nexo-marketing` (or whatever).
- Drop `publish = false` if you intend to release.
- Switch the `nexo-mcp` path dep to a published version:

```toml
nexo-mcp = "0.1.1"   # was: { path = "../../crates/mcp" }
```

### 3. `plugin.toml`

- Bump `id`, `name`, `description`.
- List your tools under `[capabilities].tools`.
- Decide whether the agent's extension supervisor should fork the
  binary directly (keep `transport.command`) or whether you'll
  run it as a long-lived service (drop the line, register the URL
  in the agent's `mcp_server.http` block).

### 4. `src/tools.rs`

Replace the `Echo` tool with your domain logic. A typed tool is
**three structs + one `impl Tool`**:

```rust
#[derive(Deserialize, JsonSchema)]
pub struct SendEmailArgs {
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[derive(Serialize, JsonSchema)]
pub struct SendEmailOut {
    pub message_id: String,
}

pub struct SendEmail { /* config: API key, smtp, etc. */ }

#[async_trait]
impl Tool for SendEmail {
    type Args = SendEmailArgs;
    type Output = SendEmailOut;

    fn name(&self) -> &str { "send_email" }
    fn description(&self) -> &str { "Send a transactional email." }

    async fn call(&self, args: Self::Args, _ctx: ToolCtx<'_>) -> Result<Self::Output, McpError> {
        // call your provider — propagate errors as McpError variants.
        Ok(SendEmailOut { message_id: "...".into() })
    }
}
```

The schema for `SendEmailArgs` is **derived once at registration**
and cached; runtime cost per `tools/call` is one `BTreeMap::get`
on the tool name. The `_ctx: ToolCtx<'_>` parameter exposes
`tenant`, `correlation_id`, `session_id`, `progress`, `cancel` —
use them when you need multi-tenant routing or want to emit
`notifications/progress` for long-running calls.

### 5. Wire the agent

Two patterns:

**Pattern A — child process (simplest).** The agent's extension
supervisor forks your binary as a child and pipes stdio. Add to
the agent's `config/agents.yaml`:

```yaml
extensions:
  - id: nexo-marketing
    command: "/path/to/nexo-marketing"
    transport: stdio
```

**Pattern B — long-lived HTTP service.** Run the binary as a
systemd unit on its own host. Configure the agent's
`mcp_server.http` block in `config/mcp_server.yaml` to point at
it. This unlocks per-tenant auth, audit, rate-limit, and lets
non-agent clients (Claude Code, Cursor) hit the same server.

## Production checklist

Before exposing a forked server beyond loopback:

| Knob | Phase | Why |
|------|-------|-----|
| `auth.kind: static_token` or `bearer_jwt` | 76.3 | Loopback bind without auth refuses to boot |
| `allow_origins: [...]` (no `*`) | 76.1 | CORS hard-rejected on non-loopback bind |
| `audit_log.enabled: true` | 76.11 | Per-call durable trail; survives restart |
| `per_principal_rate_limit.enabled: true` | 76.5 | Cap noisy tenants before they exhaust paid APIs |
| `per_principal_concurrency.enabled: true` | 76.6 | Keep one tenant from starving others |
| `session_event_store.enabled: true` | 76.8 | SSE consumers can resume via `Last-Event-ID` |
| TLS in front (nginx/caddy/Traefik) | 76.13 (deferred) | Direct `rustls` parked; treat binary as cleartext |

Each row maps to a config block in
[`config.example.yaml`](https://github.com/lordmacu/nexo-rs/blob/main/extensions/template-mcp-server/config.example.yaml).
Uncomment and fill in.

## Quickstart smoke

```bash
# Fresh checkout, in-tree build:
cargo build -p template-mcp-server

# Stdio (what the agent's supervisor sees):
echo '{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}' | \
  ./target/debug/template-mcp-server

# HTTP for direct curl/claude-mcp testing:
MCP_TEMPLATE_HTTP_BIND=127.0.0.1:7676 \
  cargo run -p template-mcp-server &

curl -s -X POST http://127.0.0.1:7676/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}'
```

## What's NOT in the template

These are deliberately out of scope to keep the skeleton small:

- **Tool with `notifications/progress`** — see
  `crates/mcp/src/server/progress.rs` and Phase 76.7 docs.
- **Tool with `notifications/tools/list_changed` for hot-reload** —
  the runtime can broadcast it via
  `HttpServerHandle::notify_tools_list_changed()`, but the
  template doesn't ship a sample of when to fire it.
- **Resources / prompts surface** — only tools are wired. See
  `crates/mcp/src/bin/mock_mcp_server.rs` for the full
  `resources/list` + `resources/read` shape.
- **Custom error types** — the template returns `McpError` via
  `?`. Map your provider errors to specific JSON-RPC error codes
  (`-32602`, `-32603`, custom application codes ≥ -32000) when
  you need clients to distinguish them.

## Reference patterns mined for this template

- `claude-code-leak/src/entrypoints/mcp.ts:35-78` — minimal stdio
  MCP server in the leaked Claude Code source. The leak uses
  imperative `setRequestHandler(SchemaName, async (req) => …)`
  per spec method; we collapse that into one
  `McpServerBuilder::tool(impl Tool)` chain.
- `claude-code-leak/src/utils/computerUse/mcpServer.ts:60-78` —
  the leak's factory pattern returning a configured `Server`. Our
  `build_handler` closure plays the same role per transport.
- `crates/mcp/src/bin/mock_mcp_server.rs` — exhaustive in-tree
  reference for protocol corner cases (initialize, paginate,
  errors, notifications, resources, sampling). Read it when this
  template's surface stops being enough.
- `research/extensions/firecrawl/` — OpenClaw extension layout.
  Different model (TypeScript provider contracts) but informs the
  `plugin.toml` shape.
