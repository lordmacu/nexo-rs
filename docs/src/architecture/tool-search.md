# `ToolSearch` (Phase 79.2 — MVP)

`ToolSearch` is the discovery surface for **deferred** tools — tools
whose full JSONSchema lives behind a single `ToolSearch(...)` lookup
instead of inline in the system prompt. The savings are real once the
tool surface gets wide (40+ tools after Phase 13 + 77 + 79); the MVP
shipped here lays the foundation.

Lift from
`upstream agent CLI`
(input schema, `select:` prefix, keyword search with `+token`
required prefix, scoring weights for name parts vs description vs
searchHint).

## How a tool becomes "deferred"

The runtime does not infer this from the tool itself. Callers opt in
when registering:

```rust
use nexo_core::agent::tool_registry::{ToolMeta, ToolRegistry};

registry.register_with_meta(
    MyTool::tool_def(),
    MyTool,
    ToolMeta::deferred_with_hint("send a slack message"),
);
```

`ToolMeta` has two fields:

| Field | Default | Effect |
|-------|---------|--------|
| `deferred: bool` | `false` | When `true`, surfaces only via `ToolSearch` |
| `search_hint: Option<String>` | `None` | Curated phrase used by keyword ranking — beats raw description scoring |

Existing `register(...)` calls keep working unchanged — the side
channel is opt-in.

## MVP caveat

The four LLM provider clients (`anthropic`, `minimax`, `gemini`,
`openai_compat`) still emit every registered tool's full schema in
the request body. The actual token-cost savings land when a follow-up
wires those four clients to consult
`ToolRegistry::deferred_tools()` and filter accordingly. Tracked in
`FOLLOWUPS.md::Phase 79.2`. Until then, `ToolSearch` is useful as a
discovery API the model can use today, and the registration path is
correct so the upgrade is a four-file change.

## Query forms

Lifted verbatim from the upstream CLI:

```
select:Read,Edit,Grep      → fetch these exact tools by name (comma-separated)
notebook jupyter           → keyword search, up to max_results best matches
+slack send                → require "slack" in name/desc/hint, rank by remaining terms
```

Tokens prefixed with `+` are required: tools that don't match all
required tokens are filtered out before scoring. Other tokens are
optional but still contribute to the score.

## Scoring (mirror of the upstream CLI)

| Match site | Non-MCP score | MCP score |
|------------|---------------|-----------|
| Exact name part | 10 | 12 |
| Substring within name part | 5 | 6 |
| `search_hint` contains term | +4 | +4 |
| `description` contains term | +2 | +2 |

Matches with `score == 0` are dropped. Results sorted by score desc
then name asc; truncated to `max_results` (default 5, hard cap 25).

`mcp__server__action`-shaped names are tokenised by splitting on
`__` and `_`; CamelCase names split on case boundaries; dotted
plugin names (`whatsapp.send`) split on `.`. So a query `"slack"`
matches `mcp__slack__send_message` on the exact server-name part.

## Response shape

```json
{
  "query": "...",
  "query_kind": "select" | "keyword",
  "total_deferred_tools": 7,
  "matches": [
    {
      "name": "FileEdit",
      "score": 12,            // omitted in `select` responses
      "description": "...",
      "parameters": { ... }   // full JSONSchema
    }
  ],
  "missing": ["UnknownTool"]   // only present in `select` responses
}
```

The model can read the schema directly out of `parameters` and call
the tool on the next turn. When the runtime starts filtering deferred
tools out of the request body (follow-up), the matched tool will
also be auto-injected into that turn's available tool list.

## References

- **PRIMARY**:
  `upstream agent CLI`,
  `upstream agent CLI,62-108`
  (`isDeferredTool`).
- **SECONDARY**: OpenClaw `research/` — no equivalent. Single-process
  TS reference does not face the wide-surface MCP token cost that
  motivates this tool.
- Plan + spec: `proyecto/PHASES.md::79.2`.
