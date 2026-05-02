# Testing microapps

Microapps you build on `nexo-microapp-sdk` get a full in-process
test harness so tool / hook handlers run without a daemon. Two
pieces:

- **`MicroappTestHarness`** drives a `Microapp` builder through
  the JSON-RPC dispatch loop end-to-end, returning the parsed
  result frame. Tools and hooks see the same `ToolCtx` /
  `HookCtx` they would in production.
- **`MockAdminRpc`** is a programmable stand-in for the daemon
  side of `nexo/admin/*`. Register canned responses per method,
  hand the mock to the harness, and your tools that call
  `ctx.admin().call(...)` see the canned values. The mock also
  records every request so tests assert on shape.

Both ship behind the SDK's `test-harness` cargo feature; the
`MockAdminRpc` additionally requires the `admin` feature.

```toml
# In your microapp's Cargo.toml
[dev-dependencies]
nexo-microapp-sdk = { version = "0.1", features = ["admin", "test-harness"] }
```

The reference test in `extensions/template-microapp-rust/src/main.rs`
exercises every piece below; copy it as a starting template.

## Smoke test (no admin, no binding)

```rust
use nexo_microapp_sdk::{Microapp, MicroappTestHarness, ToolCtx, ToolError, ToolReply};
use serde_json::{json, Value};

async fn ping(_args: Value, _ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    Ok(ToolReply::ok_json(json!({ "pong": true })))
}

#[tokio::test]
async fn ping_returns_pong() {
    let app = Microapp::new("my-microapp", "0.1.0").with_tool("ping", ping);
    let h = MicroappTestHarness::new(app);
    let out = h.call_tool("ping", json!({})).await.unwrap();
    assert_eq!(out["pong"], true);
}
```

The harness consumes the `Microapp` once per call. Tests that
need multiple calls build a fresh app each time, or factor the
builder into a `build_app()` helper (see the template).

## Tool with `BindingContext`

`ctx.binding()` returns the `(agent_id, channel, account_id, …)`
the daemon resolved for this turn. In production it's threaded
through `_meta.nexo.binding`; tests inject a `MockBindingContext`
through the same path.

```rust
use nexo_microapp_sdk::{MicroappTestHarness, MockBindingContext};

#[tokio::test]
async fn tool_reads_agent_id_from_binding() {
    let binding = MockBindingContext::new()
        .with_agent("ana")
        .with_channel("whatsapp")
        .with_account("acme")
        .build();
    let h = MicroappTestHarness::new(build_app());
    let out = h
        .call_tool_with_binding("greet", json!({ "name": "world" }), binding)
        .await
        .unwrap();
    assert_eq!(out["agent_id"], "ana");
}
```

`MockBindingContext::new().build()` panics if `agent_id` is
unset — the daemon never delivers a tool call without one, so
the panic surfaces test wiring mistakes immediately.

## Tool that calls `nexo/admin/*`

When a tool calls `ctx.admin().call(...)` the production path
talks JSON-RPC over stdio to the daemon. The harness installs
the `MockAdminRpc`'s `AdminClient` instead:

```rust
use nexo_microapp_sdk::admin::MockAdminRpc;
use nexo_microapp_sdk::AdminError;

#[tokio::test]
async fn whoami_calls_admin_and_surfaces_detail() {
    let mock = MockAdminRpc::new();

    // Register a canned `Ok(value)` response.
    mock.on(
        "nexo/admin/agents/get",
        json!({ "id": "ana", "active": true, "model": { "provider": "minimax" } }),
    );

    let binding = MockBindingContext::new().with_agent("ana").build();
    let h = MicroappTestHarness::new(build_app())
        .with_admin_mock(&mock)
        .await;

    let out = h
        .call_tool_with_binding("whoami", json!({}), binding)
        .await
        .unwrap();
    assert_eq!(out["queried_agent"], "ana");

    // Mock recorded the request — assert on shape.
    let calls = mock.requests_for("nexo/admin/agents/get");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].params["agent_id"], "ana");
}
```

### Three flavours of `on*`

| Method | Signature | When |
|--------|-----------|------|
| `on(method, value)` | `&self, &str, Value` | Static `Ok(value)` |
| `on_err(method, err)` | `&self, &str, AdminError` | Static `Err(err)` |
| `on_with(method, F)` | `&self, &str, F: Fn(Value) -> Result<Value, AdminError>` | Closure responder — receives the request params, returns the result. Use this when the response depends on input or the test wants to count invocations |

A method without a registered responder returns
`AdminError::MethodNotFound`. The mock is fail-loud on purpose
— tests that forget to wire a response see a clear error rather
than hanging on a default response.

### Asserting on errors

The error round-trip is variant-preserving. A daemon that
returns `CapabilityNotGranted` on the wire shows up as the same
typed variant on the microapp side, and the mock matches that
shape:

```rust
mock.on_err(
    "nexo/admin/agents/upsert",
    AdminError::CapabilityNotGranted {
        capability: "agents_crud".into(),
        method: "nexo/admin/agents/upsert".into(),
    },
);
```

The tool's `ctx.admin().call(...)` returns `Err(AdminError::CapabilityNotGranted { .. })`
verbatim — so the tool's error-mapping logic gets exercised
exactly as it would against the live daemon.

### Counting invocations from a closure

`on_with` captures any state the closure needs:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

let count = Arc::new(AtomicUsize::new(0));
let count_clone = Arc::clone(&count);
mock.on_with("nexo/admin/ping", move |_| {
    count_clone.fetch_add(1, Ordering::SeqCst);
    Ok(json!({}))
});
// ... drive the harness ...
assert_eq!(count.load(Ordering::SeqCst), 3);
```

## Hooks

`fire_hook(hook_name, args)` returns the parsed `HookOutcome`.
Same harness, different surface:

```rust
let h = MicroappTestHarness::new(build_app());
let outcome = h
    .fire_hook("before_message", json!({ "body": "hi" }))
    .await
    .unwrap();
assert!(matches!(outcome, HookOutcome::Continue));
```

For `Abort` cases, match on the variant and inspect `reason`.

## What the harness does NOT do

- **Boot a real daemon.** No NATS, no `agents.yaml`, no live
  agent loop. Use the harness for tool / hook unit tests; reach
  for an end-to-end test (a real daemon process spawned from
  the test) when you need the full pipeline.
- **Subscribe to the firehose.** `nexo/notify/agent_event`
  delivery is daemon-side; the harness exits after one
  request/response. Future helper lands in 83.15.b.b.
- **Persist anything.** Every harness call gets a fresh
  `Handlers` registry; admin mock state is the
  `MockAdminRpc` you explicitly hand it. Tests are isolated
  by construction.

## Reference

The template microapp ships every pattern above as runnable
tests:

```bash
cargo test -p template-microapp-rust
```

See `extensions/template-microapp-rust/src/main.rs#tests` for
the source. Copy whichever tests apply when you start a new
microapp.
