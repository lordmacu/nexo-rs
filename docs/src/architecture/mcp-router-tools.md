# `ListMcpResources` + `ReadMcpResource` (Phase 79.11 — MVP)

Router-shaped MCP tools: a single discovery surface for agents
talking to many MCP servers, instead of registering N×2 per-server
tools (which still ship via the Phase 12.5 catalog).

Lift from `claude-code-leak/src/tools/ListMcpResourcesTool/` +
`ReadMcpResourceTool/`. The leak ships these as the LLM-driven
introspection layer over the per-server `McpClient` trait.

## Diff vs Phase 12.5 per-server tools

| Aspect | Phase 12.5 per-server | Phase 79.11 router |
|--------|----------------------|--------------------|
| Tool surface | `mcp__<server>__list_resources` per server | Single `ListMcpResources { server: Option<String> }` |
| Token cost | O(2 × N servers) tools in prompt | 2 fixed tools |
| Use when | 1–2 MCP servers connected | 3+ MCP servers, surface gets noisy |

Both surfaces coexist — operators can keep the per-server tools
for the agents that need them and let the router-shaped tools
handle wide deployments.

## Tool shapes

### `ListMcpResources`

```json
{ "server": "github", "max": 100 }
```

`server` is optional — omit to enumerate every connected server.
`max` overrides the default 200-entry cap. Returns:

```json
{
  "resources": [
    { "server": "github", "uri": "...", "name": "...", "description": "...", "mime_type": "..." }
  ],
  "truncated": false,
  "errors": [],
  "count": 12
}
```

`errors` carries per-server failures so a single bad server
doesn't drop the whole call. `truncated: true` flags that the cap
was hit before all resources were enumerated.

### `ReadMcpResource`

```json
{ "server": "github", "uri": "github://owner/repo/file", "max_bytes": 65536 }
```

`server` + `uri` required. `max_bytes` overrides the default
256 KiB cap. Returns:

```json
{
  "server": "github",
  "uri": "...",
  "contents": [
    { "uri": "...", "mime_type": "text/plain", "text": "...", "blob_length": null, "blob": null }
  ],
  "truncated": false
}
```

Truncation respects UTF-8 char boundaries — never splits a
multi-byte sequence. Binary `blob` bodies are returned verbatim
(base64) without mid-string truncation; the `blob_length` field
is reported so the model can decide whether to ask for a smaller
slice.

## Plan-mode classification

Both tools are `ReadOnly` — they query but never mutate, so they
stay callable while plan mode is on.

## Out of scope (deferred)

- **`McpAuth`.** The `McpClient` trait does not expose a refresh
  hook (refresh is currently transparent inside the client).
  Tracked in `FOLLOWUPS.md::Phase 79.11` — once the trait grows
  the method (lift from
  `claude-code-leak/src/services/mcp/oauthPort.ts`), a third
  tool wires into the same module.
- **Operator overrides for the caps.** Today the caps are module
  constants; a follow-up can read them from the MCP YAML
  (`mcp.list_max_resources`, `mcp.resource_max_bytes`).

## References

- **PRIMARY**: `claude-code-leak/src/tools/ListMcpResourcesTool/`,
  `claude-code-leak/src/tools/ReadMcpResourceTool/`,
  `claude-code-leak/src/services/mcp/MCPConnectionManager.tsx`.
- **SECONDARY**: OpenClaw `research/` — no equivalent
  router-shaped MCP tool.
- Plan + spec: `proyecto/PHASES.md::79.11`.
