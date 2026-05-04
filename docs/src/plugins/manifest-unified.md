# Plugin manifest (Phase 81.13 unified)

Phase 81.13 unified the framework's two manifest parsers
(`nexo-extensions::manifest` Phase 11 + `nexo-plugin-manifest`
Phase 31.5+) into a single source of truth. Plugin authors now
ship **one** TOML manifest at the plugin root that declares
both the legacy contributions (tools / hooks / channels /
providers / pollers) AND the modern admin RPC + HTTP server
capabilities.

## Filename

The canonical filename is **`plugin.toml`**. The framework also
accepts `nexo-plugin.toml` as a legacy fallback for one
deprecation cycle so existing plugins keep loading without an
immediate rename. When both files are present in the same plugin
root, `plugin.toml` wins and the daemon emits a warning.

Plugins authored after 81.13 should ship `plugin.toml` only.

## Versioning

The TOML root may carry a `manifest_version` integer:

- **omitted** or `1` → legacy v1 shape (flat `[capabilities]`,
  `[transport]`, `[meta]`, `[mcp_servers]`, `[outbound_bindings]`,
  `[context]`, `[requires]`). The parser auto-translates to v2 in
  memory and emits a one-shot deprecation warn per plugin.
- `2` → canonical Phase 81.13 shape. New plugins should set this
  explicitly to opt out of the deprecation warn.

Unknown values produce a clear parse error.

## ID regex

Plugin ids match `^[a-z][a-z0-9_-]{0,63}$` (lowercase, starts with
letter, body of letters/digits/underscores/hyphens, length 64).
Both `agent_creator` and `agent-creator` styles are valid; the
framework normalises neither so plugin authors get to pick.

Reserved ids that no plugin can claim (defended at boot):
`agent`, `browser`, `core`, `email`, `heartbeat`, `memory`,
`telegram`, `whatsapp`.

## Where the legacy fields land

Pre-81.13 plugins kept their `plugin.toml` flat (Phase 11 shape).
Those still parse — the compat layer translates each section as
follows:

| v1 location | v2 location |
|-------------|-------------|
| `[plugin]` | `[plugin]` (renames `min_agent_version` → `min_nexo_version`) |
| `[capabilities]` | `[plugin.capabilities]` |
| `[capabilities.admin]` | `[plugin.capabilities.admin]` |
| `[capabilities.http_server]` | `[plugin.capabilities.http_server]` |
| `[transport]` (`kind = "stdio"`) | `[plugin.entrypoint]` |
| `[transport]` (`kind = "nats"|"http"`) | DROPPED with warn (Phase 81.13.b will preserve) |
| `[meta]` | `[plugin.meta]` |
| `[requires]` (`bins`/`env`) | DROPPED with warn (preserved in 81.13.b) |
| `[mcp_servers]` (top-level) | DROPPED with warn |
| `[outbound_bindings]` (top-level) | DROPPED with warn |
| `[context]` | DROPPED with warn |
| `[plugin] priority` | DROPPED with warn |
| `[capabilities] tools/hooks/channels/providers/pollers` | DROPPED with warn |

The "DROPPED" entries don't break boot — the parser logs the list
of legacy fields it saw + skipped per plugin. Consumers that
needed those fields keep reading them via the legacy
`nexo-extensions::manifest::ExtensionManifest::from_path` path,
which still parses the v1 shape directly.

## Single-file canonical example

```toml
manifest_version = 2

[plugin]
id               = "agent_creator"
version          = "0.0.35"
name             = "Agent Creator"
description      = "Operator UI microapp."
min_nexo_version = ">=0.1.0"

[plugin.entrypoint]
command = "./agent-creator"
args    = []

[plugin.capabilities.admin]
required = ["agents_crud", "skills_crud", "llm_keys_crud"]
optional = ["channels_crud", "auth_rotate", "secrets_write"]

[plugin.capabilities.http_server]
port        = 8765
bind        = "127.0.0.1"
token_env   = "AGENT_CREATOR_TOKEN"
health_path = "/healthz"

[plugin.meta]
author     = "Cristian García"
license    = "MIT OR Apache-2.0"
homepage   = "https://example.com"
repository = "https://github.com/x/y"
```

## Pre-81.13 example (still valid via compat)

```toml
[plugin]
id          = "agent-creator"
version     = "0.0.34"
name        = "Agent Creator"
description = "Operator UI microapp."

[capabilities]
tools = ["agent_list", "agent_get"]
hooks = ["before_message"]

[capabilities.admin]
required = ["agents_crud"]
optional = ["channels_crud"]

[transport]
kind    = "stdio"
command = "./agent-creator"

[meta]
author = "Cristian"
```

The framework parses this as `manifest_version = 1` (auto-
detected), translates to v2 in memory + emits a deprecation warn
once at boot. Operator can migrate at their own pace.

## Deferred (sub-phase 81.13.b)

- Preserve legacy `mcp_servers` / `outbound_bindings` /
  `context` / `requires.bins+env` /
  `capabilities.tools+hooks+channels+providers+pollers` /
  `transport.kind=nats|http` / `plugin.priority` in the canonical
  v2 shape so the migrator stops dropping them.
- Hard removal of `nexo-plugin.toml` filename + `manifest_version
  = 1` mode (target: 0.2.0).
- JSON-Schema export for editor autocomplete (mirrors OpenClaw's
  `openclaw.plugin.json`).
