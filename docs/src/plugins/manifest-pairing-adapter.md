# `[plugin.pairing.adapter]` — manifest-driven pairing adapter

> Phase 81.33.b.real (Stage 1 of the [plugin auto-discovery
> design](../architecture/plugin-auto-discovery.md)). Status:
> shipped 2026-05-15.

Plugins that expose a channel with a DM-pairing flow declare the
broker dispatch contract for the daemon's
[`PairingChannelAdapter`](https://docs.rs/nexo-pairing) in
`nexo-plugin.toml`. The daemon constructs a
`GenericBrokerPairingAdapter` from the manifest and registers it
into the shared `PairingAdapterRegistry`. This replaces the
previous design where the daemon had to hardcode
`Arc::new(<plugin>PairingAdapter::new(broker))` blocks for every
canonical plugin.

## Manifest section

```toml
[plugin.pairing.adapter]
channel_id           = "whatsapp"
broker_topic_prefix  = "plugin.whatsapp"
# Optional knobs:
# format_challenge_text_kind = "broker"   # default: trait's built-in formatter
# normalize_cache_ttl_seconds = 3600      # default: unbounded
```

Required fields:

- `channel_id` — stable string id matching what the gate stores
  in `pairing_pending.channel` and `pairing_allow_from.channel`.
  The registry uses this as the key under `register()`. Plugins
  that also ship `[plugin.pairing]` UI (`kind = "qr" | "form" |
  "info" | "custom"`) should use the same `channel_id` so the
  registry + UI agree.
- `broker_topic_prefix` — broker subject prefix the daemon
  dispatches JSON-RPC requests under (see contract below).

Optional fields:

- `format_challenge_text_kind` — `"default"` (the value the
  trait already supplies) or `"broker"` (asks the plugin per
  challenge). Default is `default`; channels needing custom
  challenge formatting (e.g. Telegram MarkdownV2 escape) flip to
  `broker`.
- `normalize_cache_ttl_seconds` — TTL for `normalize_sender`
  cache entries. `None` (default) = unbounded (cache lives the
  daemon's lifetime). Set when the plugin can return different
  canonical forms for the same `raw` over time.

## Broker JSON-RPC dispatch contract

Daemon publishes JSON-RPC `request` messages on
`<broker_topic_prefix>.pairing.<method>`. Plugin subscribes,
handles, replies. All payloads are JSON.

### `<prefix>.pairing.normalize_sender`

**Request.** `{ "raw": "<raw-sender>" }`.

**Reply.** `{ "normalized": "<canonical>" }` to accept, or
`{ "normalized": null }` to reject (the gate treats reject as
`Decision::Drop`).

Examples:

| Plugin | Input | Output |
|---|---|---|
| whatsapp | `573001112222@c.us` | `+573001112222` |
| whatsapp | `573001112222@s.whatsapp.net` | `+573001112222` |
| telegram | `@User_Name` | `@user_name` |
| telegram | `12345678` | `12345678` |
| telegram | `not_a_handle` | `null` (reject) |

The daemon **caches** `(raw → normalized)` in memory. Cache hits
do NOT round-trip the broker. With `normalize_cache_ttl_seconds`
unset the cache grows bounded by distinct-sender count;
typically < 10⁴ entries.

### `<prefix>.pairing.send_reply`

**Request.** `{ "account": "<inst>", "to": "<sender>", "text": "<challenge>" }`.

**Reply.** `{ "ok": true }` or
`{ "ok": false, "error": "<message>" }`.

Delivers the pairing challenge text. `account` is the
multi-instance discriminator (operators set it via
`config/plugins/<channel>.yaml`'s `instance` field).

### `<prefix>.pairing.send_qr_image`

**Request.** `{ "account": "<inst>", "to": "<sender>", "png_base64": "<base64>" }`.

**Reply.** `{ "ok": true }` or
`{ "ok": false, "error": "<message>" }`.

Plugin decodes the base64 PNG and delivers it as a media
message. Plugins whose channel cannot send media should reply
`{ "ok": false, "error": "channel does not support media" }` —
the trait's default impl bails for plugins that haven't overridden,
but explicit failure with a clear error helps operators diagnose.

### `<prefix>.pairing.format_challenge_text` (only when `format_challenge_text_kind = "broker"`)

**Request.** `{ "code": "<setup-code>" }`.

**Reply.** `{ "text": "<formatted-challenge>" }`.

When the manifest sets `format_challenge_text_kind = "broker"`
the daemon issues this RPC instead of using the trait's
built-in formatter. Used for channels that need plugin-specific
escaping (Telegram MarkdownV2, Discord embed JSON, …).

## Migration path

Plugins migrate by adding the manifest section in their next
release. Until then the daemon's legacy hardcoded
`build_known_pairing_registry()` registration (gated by
`#[cfg(feature = "plugin-<id>")]`) continues to serve. When a
plugin ships the manifest section, the registry's `register()`
**overwrites by `channel_id`** so the generic adapter wins
without operator action.

### Canonical plugins

- `nexo-plugin-whatsapp` — pending. When shipped, the daemon's
  L1654 `Arc::new(WhatsappPairingAdapter::new(broker))` becomes
  dead code (still cfg-gated, removable in a follow-up).
- `nexo-plugin-telegram` — pending. Same shape, L1657.
- Future plugins (signal, matrix, sms, discord) — drop manifest
  in `plugins.discovery.search_paths`; no daemon edit needed.

## Implementing the broker side (plugin authors)

Subprocess plugins built on the `nexo-microapp-sdk` register the
three handlers (`normalize_sender`, `send_reply`, `send_qr_image`)
during `PluginAdapter::init`. Reference implementations land in
the next `nexo-plugin-whatsapp` + `nexo-plugin-telegram` release;
until then plugins can mirror the canonical
`<channel>PairingAdapter::normalize_sender` Rust logic into the
broker handler verbatim.

## Daemon-side wiring

`SubprocessNexoPlugin::build_pairing_adapter()`
(`crates/core/src/agent/nexo_plugin_registry/subprocess.rs`)
checks `self.cached_manifest.plugin.pairing.adapter`. When
present, returns
`Some(Arc::new(GenericBrokerPairingAdapter::from_manifest(broker, section)))`.

The daemon's boot loop (`src/main.rs:6416`) iterates every
loaded plugin handle and calls `build_pairing_adapter(broker.clone())`.
Same loop runs in the hot-spawn path (`src/main.rs:7224+`).
Dispatch-ctx mode (`boot_dispatch_ctx_if_enabled` /
autonomous-worker) uses only the legacy hardcoded registry today;
threading `plugin_handles_cell` into that function is a follow-up
if dispatch-ctx flows grow pairing-aware hooks.

## Validation

- `cargo build --release-fast --bin nexo` (default) — 3m09s.
- `cargo build --release-fast --bin nexo --no-default-features` —
  2m50s (`nexo-plugin-whatsapp` + `nexo-plugin-telegram` absent
  from the compile graph; manifest-section parsing still compiles
  because it's in `nexo-plugin-manifest`).
- `cargo nextest run --workspace` — 6280/6280 (4 new tests for
  `GenericBrokerPairingAdapter`).
- `cargo nextest run -p nexo-pairing` — 67/67.
- `cargo nextest run --no-default-features -p nexo-rs` — 105/105.

## Trade-offs

| Concern | Decision |
|---|---|
| Sync `normalize_sender` blocking on broker | `tokio::task::block_in_place` + `Handle::block_on`. Requires multi-threaded runtime (every daemon hot path qualifies). Cache makes the cost a one-time miss per unique sender. |
| Cache eviction | Default unbounded; TTL knob if needed. Pairing-only volume keeps unbounded safe at scale. |
| Custom challenge text per channel | Optional `format_challenge_text_kind = "broker"`. Default uses the trait's built-in formatter (covers 90% of channels). |
| `channel_id` `'static` lifetime | `Box::leak` at construction. One-time leak per plugin per daemon run; bounded by plugin count. |
| Manifest schema change without daemon recompile | New optional fields use `#[serde(default)]`; backward compatible. New required fields = manifest schema version bump. |
