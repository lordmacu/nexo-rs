# nexo-microapp-sdk

Reusable runtime helpers for Phase 11 stdio microapps consuming
the [Nexo](https://github.com/lordmacu/nexo-rs) daemon.

Replaces the ~200 LOC of boilerplate every microapp would
otherwise rewrite (JSON-RPC line loop, dispatch table,
`_meta.nexo.binding` parsing, hook outcome serialisation,
tracing setup).

## Install

```toml
[dependencies]
nexo-microapp-sdk = "0.1"
```

Optional features:

- `outbound` — opt-in `ctx.outbound()` accessor for
  `nexo/dispatch` calls (requires daemon ≥ Phase 82.3.b; v0
  ships a stub that returns `DispatchError::Transport`).
- `test-harness` — `MicroappTestHarness` for `#[cfg(test)]`
  consumption.
- `webhook` — reserved for future `WebhookEnvelope` parsing
  on NATS subjects.

## Quick start

```rust
use nexo_microapp_sdk::{Microapp, ToolCtx, ToolReply, ToolError};
use serde_json::json;

async fn register_lead(args: serde_json::Value, ctx: ToolCtx)
    -> Result<ToolReply, ToolError>
{
    let phone = args["phone"].as_str()
        .ok_or_else(|| ToolError::InvalidArguments("phone required".into()))?;
    let channel = ctx.binding().and_then(|b| b.channel.as_deref()).unwrap_or("");
    Ok(ToolReply::ok_json(json!({
        "registered": phone,
        "via": channel,
    })))
}

#[tokio::main]
async fn main() -> nexo_microapp_sdk::Result<()> {
    nexo_microapp_sdk::init_logging_from_env("agent-creator");
    Microapp::new("agent-creator", env!("CARGO_PKG_VERSION"))
        .with_tool("register_lead", register_lead)
        .run_stdio()
        .await
}
```

## What's inside

- **`Microapp`** — chained builder (`with_tool`, `with_hook`,
  `on_initialize`, `on_shutdown`, `run_stdio`).
- **`ToolCtx`** — handler context with `binding()` accessor
  parsed from `_meta.nexo.binding` (provider-agnostic; works
  for whatsapp / telegram / email / event-subscriber inbounds).
- **`ToolReply`** — typed wire shape (`ok(text)`, `ok_json(value)`,
  `empty()`).
- **`ToolError`** — typed error enum mapped to JSON-RPC error
  codes (-32601, -32602, -32000).
- **`HookOutcome`** — `Continue` / `Abort { reason }` for
  `before_message` / `before_tool_call` / etc. hooks.
- **`init_logging_from_env(crate_name)`** — env-var-driven
  `tracing-subscriber` setup.

## Testing

With the `test-harness` feature:

```rust
#[cfg(test)]
mod tests {
    use nexo_microapp_sdk::{Microapp, MicroappTestHarness, ToolCtx, ToolError, ToolReply};
    use serde_json::json;

    async fn echo(args: serde_json::Value, _ctx: ToolCtx)
        -> Result<ToolReply, ToolError>
    {
        Ok(ToolReply::ok_json(args))
    }

    #[tokio::test]
    async fn echo_round_trip() {
        let app = Microapp::new("test", "0.0.0").with_tool("echo", echo);
        let h = MicroappTestHarness::new(app);
        let out = h.call_tool("echo", json!({"x": 1})).await.unwrap();
        assert_eq!(out["x"], 1);
    }
}
```

## License

Licensed under either Apache-2.0 or MIT at your option.
