# `[plugin.pairing.trigger]` — manifest-driven pairing trigger

> Phase 81.20.x Stage 7 Phase 2. Status: introduced 2026-05-16.

Plugins that own a QR-pairing pump (e.g. WhatsApp `wa-agent`, future
Signal QR-link) declare which admin RPC methods the daemon should
forward `pairing/start` and `pairing/cancel` to. The daemon
constructs a `BrokerPairingTrigger` (in `nexo-pairing`) from the
manifest and inserts it into the dispatcher's
`PairingChannelTriggers` map under the same `channel_id` as the
sibling `[plugin.pairing.adapter]` section.

This replaces the previous design where the daemon had to import
`nexo_plugin_whatsapp::pairing_trigger::WhatsappPairingTrigger` and
call `from_configs(...)` — daemon no longer needs to link the
plugin crate to drive its pump.

## Manifest section

```toml
[plugin.pairing]
kind  = "qr"
label = "WhatsApp"

[plugin.pairing.trigger]
start_method  = "nexo/admin/whatsapp/pairing/start"
cancel_method = "nexo/admin/whatsapp/pairing/cancel"
# Optional knob:
# timeout_seconds = 120   # default: PAIRING_DEFAULT_TIMEOUT (180s)
```

Required fields:

- `start_method` — full admin RPC method name the daemon forwards
  the pump-start request to. MUST live under the plugin's own
  `[plugin.admin] method_prefix` (e.g.
  `"nexo/admin/<plugin_id>/pairing/start"`). Forwarded via
  `PluginAdminRouter` so the plugin's existing admin subscriber
  pipeline serves it.
- `cancel_method` — full admin RPC method name for pump
  cancellation. Same routing rules as `start_method`.

Optional fields:

- `timeout_seconds` — per-call broker forward timeout. Defaults to
  `PAIRING_DEFAULT_TIMEOUT` (180s), the upper bound for the whole
  pairing handshake.

## Compatibility constraints

- `[plugin.pairing.trigger]` is **only valid with
  `kind = "qr"`**. Form and Info kinds are operator-driven and need
  no remote pump. Custom kinds use their own
  `nexo/notify/<rpc_namespace>/status_changed` channel.
  Manifest validation rejects mismatched combinations at boot.
- `start_method` and `cancel_method` MUST be non-empty.

## Broker JSON-RPC dispatch contract

The daemon's `BrokerPairingTrigger.start(ctx)`:

1. Forwards `start_method` via `PluginAdminRouter`, passing
   `{ challenge_id, agent_id, instance }` as params.
2. Plugin's admin handler spawns its pump (wa-agent QR pump for
   whatsapp). The handler returns immediately — the pump runs
   in the subprocess.
3. The plugin publishes QR and state updates on
   `plugin.inbound.<channel_id>.<instance>.pairing.qr` and
   `.../pairing.state` (new contract). Daemon's single generic
   subscriber updates `ctx.store.update_qr` + `notify_status`.
4. On `pairing/cancel`, daemon forwards `cancel_method` via the
   same router. Plugin tears down the pump cleanly.

### Inbound topics (plugin → daemon)

```text
plugin.inbound.<channel_id>.<instance>.pairing.qr
plugin.inbound.<channel_id>.<instance>.pairing.state
```

QR payload: `{ "challenge_id": "<uuid>", "png_base64": "<base64>", "rotates_at": "<rfc3339>" }`.

State payload:
`{ "challenge_id": "<uuid>", "state": "Linked" | "Error" | "Pending", "error": "<msg-if-Error>" }`.

## Migration path

Plugins migrate by adding the manifest section in their next
release. Until a plugin ships the section, the daemon falls back
to its legacy hardcoded `WhatsappPairingTrigger` import (gated by
`#[cfg(feature = "plugin-whatsapp")]`). Once the plugin manifests
the section, the boot loop's generic registration overwrites by
`channel_id`, so the broker trigger wins without operator action.

### Canonical plugins

- `nexo-plugin-whatsapp` — pending v0.4.4. When shipped, the
  daemon's `WhatsappPairingTrigger::from_configs` registration in
  `src/main.rs` (cfg-gated) becomes dead code; removable once
  v0.4.4 lands on crates.io.
- Future pairing channels (signal, matrix, sms with QR-link) —
  drop manifest into `plugins.discovery.search_paths`; no daemon
  edit needed.

## Implementing the broker side (plugin authors)

Subprocess plugins built on the `nexo-microapp-sdk` register the
`start_method` and `cancel_method` handlers via their existing
`[plugin.admin]` subscriber (the broker topic prefix is
`<plugin.admin.broker_topic_prefix>.<suffix>`, where `<suffix>` is
the trailing portion of the admin method after the plugin's
prefix).

Reference impl lands in `nexo-plugin-whatsapp` v0.4.4. The handler
should:

1. Read `{ challenge_id, agent_id, instance }` from params.
2. Spawn an async task that drives the pump (wa-agent's
   `pair_with_callback`).
3. As QR frames rotate, publish to
   `plugin.inbound.whatsapp.<instance>.pairing.qr`.
4. On connect success, publish state `Linked`. On terminal error,
   publish state `Error` with the error message.
5. Return `{ "ok": true }` from the admin RPC reply (the start was
   accepted; success/failure flows through the inbound topics).

Cancellation: the daemon forwards `cancel_method` with
`{ challenge_id }`. Handler aborts the spawned pump task and
returns `{ "ok": true }`.

## Validation

- `cargo nextest run -p nexo-plugin-manifest` — manifest schema
  + validator tests.
- `cargo nextest run -p nexo-pairing` — `BrokerPairingTrigger`
  unit tests.
- `cargo nextest run -p nexo-rs` — daemon boot-loop integration.

## Trade-offs

| Concern | Decision |
|---|---|
| Daemon import of plugin crate for trigger | Eliminated — the broker trigger forwards via existing `PluginAdminRouter`. |
| Reuse of `[plugin.admin]` topic vs. dedicated `[plugin.pairing.trigger]` topic | Reuse. Cleaner: one less manifest section, one less subscriber to maintain in the plugin. Admin handler already exists in canonical plugins. |
| Plugin needs to push QR back to daemon | New `plugin.inbound.<channel>.<inst>.pairing.{qr,state}` broker topics. Daemon's single generic subscriber consumes both. |
| Backwards compat for plugins without the section | Daemon falls back to the legacy hardcoded trigger registration. Removed after the canonical plugin ships the new manifest. |
