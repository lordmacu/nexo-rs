# nexo-tool-meta

Wire-shape types shared between the [Nexo](https://github.com/lordmacu/nexo-rs)
agent runtime and any third-party microapp that consumes its
events.

Provider-agnostic by construction: no transport (no `axum`,
no `tokio`), no broker, no agent runtime. Pulling this crate
in is a four-dependency, sub-second compile and exposes only
the data shapes a downstream consumer needs.

## Install

```toml
[dependencies]
nexo-tool-meta = "0.1"
```

## What's inside

- **`BindingContext`** — `(channel, account_id, agent_id,
  session_id, binding_id)` tuple stamped on every tool call so
  a microapp knows which inbound binding the call came from.
- **`build_meta_value`** / **`parse_binding_from_meta`** — the
  inverse pair around the dual-write `_meta` payload nexo emits
  on every JSON-RPC `tools/call`.
- **`WebhookEnvelope`** — typed JSON envelope nexo publishes
  to NATS after every accepted webhook request.
- **`format_webhook_source`** — Phase 72 turn-log marker
  helper.

## Quick example

```rust
use nexo_tool_meta::{parse_binding_from_meta, BindingContext};

// A microapp receives this `_meta` payload on every tool call.
let meta = serde_json::json!({
    "agent_id": "ana",
    "session_id": "00000000-0000-0000-0000-000000000000",
    "nexo": {
        "binding": {
            "agent_id": "ana",
            "channel": "whatsapp",
            "account_id": "personal",
            "binding_id": "whatsapp:personal"
        }
    }
});

let binding: BindingContext = parse_binding_from_meta(&meta).unwrap();
assert_eq!(binding.channel.as_deref(), Some("whatsapp"));
assert_eq!(binding.account_id.as_deref(), Some("personal"));
```

## Forward-compatibility

Every public type is `#[non_exhaustive]`. The daemon adds
fields at minor version bumps; older parsers see the new
fields silently ignored. Microapps built against `0.1.0`
keep working when the runtime emits a `0.2.x` envelope.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE)
or [MIT License](LICENSE-MIT) at your option.
