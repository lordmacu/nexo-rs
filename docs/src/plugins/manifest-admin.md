# `[plugin.admin]` — daemon-forwarded admin RPC commands

> Phase 81.33.b.real Stage 4 (Layer 9 of the
> [plugin auto-discovery design](../architecture/plugin-auto-discovery.md)).
> Status: shipped 2026-05-15.

Plugins that expose admin RPC commands (bot inspectors, account
listers, channel-specific control planes) declare a method
prefix in their `nexo-plugin.toml`. The daemon's admin
dispatcher matches every incoming method against the registered
prefixes and forwards matches to the plugin's subprocess via
broker JSON-RPC. The plugin handles internal dispatch.

Replaces the previous pattern where each plugin needed a
hardcoded `.with_<plugin>_handle(Arc<dyn XxxHandle>)` builder
method on the dispatcher (e.g. the legacy `with_wa_bot_handle`
integration for `nexo/admin/whatsapp/bot/*`).

## Manifest section

```toml
[plugin.admin]
method_prefix       = "nexo/admin/whatsapp/"
broker_topic_prefix = "plugin.whatsapp.admin"
# Optional:
# timeout_seconds = 30
```

Fields:

- `method_prefix` (required) — admin RPC method prefix the
  plugin owns. Must start with `nexo/admin/` and end with `/`.
  Example: `nexo/admin/whatsapp/`.
- `broker_topic_prefix` (required) — broker subject prefix the
  daemon forwards under. Example: `plugin.whatsapp.admin`.
- `timeout_seconds` (optional) — per-method broker RPC timeout.
  Default 30s.

## Broker JSON-RPC contract

Daemon → plugin on `<broker_topic_prefix>.<verb>`:

```json
{
  "method": "nexo/admin/whatsapp/bot/list",
  "params": { "agent_id": "kate" }
}
```

Plugin replies:

```json
{ "ok": true,  "result": { "bots": [...] } }
{ "ok": false, "error": "session not connected" }
```

The daemon emits the broker subject by stripping `method_prefix`
from the incoming method, replacing `/` with `.`, and appending
to `broker_topic_prefix`. So
`nexo/admin/whatsapp/bot/list` →
`bot/list` → `bot.list` →
`plugin.whatsapp.admin.bot.list`.

## Reserved prefixes

The router refuses to register any plugin whose `method_prefix`
collides with daemon-internal admin handlers. The reserved list:

- `nexo/admin/agents/`
- `nexo/admin/credentials/`
- `nexo/admin/pairing/`
- `nexo/admin/llm/`
- `nexo/admin/channels/`
- `nexo/admin/tenants/`
- `nexo/admin/memory/`
- `nexo/admin/sessions/`
- `nexo/admin/snapshots/`
- `nexo/admin/policy/`

Comparison is bidirectional — `nexo/admin/agents/sneaky/`
(subpath of reserved) AND `nexo/admin/` (super-prefix that
would shadow reserved) are both rejected. Plugins choosing
non-reserved prefixes (`nexo/admin/whatsapp/`,
`nexo/admin/signal/`, `nexo/admin/oauth/`) register normally.

Rejected registrations log at warn level; the daemon's
internal handlers continue to serve uninterrupted.

## Capability enforcement

Plugin admin methods that match the router are gated by the
existing `channels_crud` capability (reused so operators
already granted channel admin can call plugin admin without a
fresh capability). Per-plugin capability grants are a follow-up
when finer control is needed.

## Error rendering

| Failure | Status |
|---|---|
| Broker timeout (plugin slow / unresponsive) | `AdminRpcError::Internal("plugin admin forward failed: broker error: ...")` → JSON-RPC error code `-32603` |
| Plugin replied with malformed JSON | same |
| Plugin replied with `ok: false` | `AdminRpcError::Internal("<plugin error message>")` |

The daemon does NOT translate plugin error strings; the typed
error surfaces verbatim through the JSON-RPC response.

## Migration status

The legacy `with_wa_bot_handle` builder on
`AdminRpcDispatcher` is preserved. Until `nexo-plugin-whatsapp`
ships a manifest revision with `[plugin.admin]`, the daemon's
hardcoded `nexo/admin/whatsapp/bot/list` + `bot/send` match
arms continue to serve. When whatsapp ships the section, the
router matches BEFORE the typed match arms (generic forward
wins) and the legacy block becomes dead code (cfg-gated,
removable in follow-up).

## Implementing the plugin side

Subprocess plugins built on `nexo-microapp-sdk` subscribe to
`<broker_topic_prefix>.>` and dispatch:

```rust
// Sketch (final SDK helpers ship with the next plugin release):
ctx.broker
    .subscribe("plugin.whatsapp.admin.>")
    .await?
    .for_each(|msg| async {
        let topic = msg.topic.strip_prefix("plugin.whatsapp.admin.").unwrap();
        match topic {
            "bot.list" => handle_bot_list(msg.payload).await,
            "bot.send" => handle_bot_send(msg.payload).await,
            _ => reply_err(msg, "method not found"),
        }
    });
```

## Validation

- `cargo build --release-fast --bin nexo` (default) — 3m10s clean.
- `cargo build --release-fast --bin nexo --no-default-features` —
  2m53s clean.
- `cargo nextest run --workspace` — 6312/6312 (9 new tests in
  `plugin_admin::tests`).
- `cargo nextest run -p nexo-pairing` — covers router matching,
  reserved-prefix rejection, subpath/super-prefix safety,
  method-to-broker-suffix translation, broker error path.
- `cargo nextest run --no-default-features -p nexo-rs` — 105/105.
- `cargo nextest run --no-default-features -p nexo-setup` — 317/317.

## Trade-offs

| Concern | Decision |
|---|---|
| Per-plugin capability gates | Reuse `channels_crud`. Follow-up if finer grant needed. |
| Plugin reply schema | `{ ok: bool, result: Value, error: String }`. Single envelope; plugin owns the `result` shape per method. |
| Method-to-topic translation | `/` → `.` mechanical. Plugin's broker handler design is straightforward. |
| Reserved-prefix safety | Bidirectional collision check (both subpath AND super-prefix rejected). |
| Interior-mutability router | `Arc<PluginAdminRouter>` with internal `RwLock<Vec<Route>>` so daemon can populate AFTER `wire_plugin_registry` returns without rebuilding the dispatcher. |
