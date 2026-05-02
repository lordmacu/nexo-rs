# 83.15.b — MockAdminRpc + reference test

## Refs

- **OpenClaw `research/extensions/telegram/src/polling-transport-state.test.ts:8-16`** —
  `makeMockTransport()` pattern: builds a fake transport with hardcoded
  `fetch` response + `vi.fn` close spy. Tests assert on `close.mock.calls` for
  behavior. Mismo patrón aplica acá: mock sender que devuelve respuestas
  canned + log de requests para aserciones.
- **`claude-code-leak/`** — sin precedente directo (CLI single-tenant,
  no admin RPC client). **Absence declarada**.

## Problema

Microapps que llaman `ctx.admin().call("nexo/admin/agents/list", ...)` no
se pueden testear unitariamente sin un daemon nexo corriendo. La harness
existente (`MicroappTestHarness::call_tool`) hardcoded `admin: None` en
`Handlers`, así que cualquier tool que toque admin recibe `Internal("admin
client not configured")` en tests.

## Diseño

### `MockAdminRpc` (en `crates/microapp-sdk/src/admin/mock.rs`, gated por feature `admin` + `test-harness`)

```rust
pub struct MockAdminRpc {
    handlers: Arc<DashMap<String, Arc<MockResponder>>>,
    requests: Arc<Mutex<Vec<MockRequest>>>,
    sender: Arc<MockAdminSender>,  // shared with client
    client: AdminClient,
}

pub struct MockRequest {
    pub method: String,
    pub params: Value,
    pub at_ms: u64,
}

type MockResponder = dyn Fn(Value) -> Result<Value, AdminError> + Send + Sync;

impl MockAdminRpc {
    pub fn new() -> Self { ... }

    /// Register a static `Ok(value)` response for one method.
    pub fn on(&self, method: &str, response: Value) -> &Self {
        let value = response.clone();
        self.on_with(method, move |_| Ok(value.clone()))
    }

    /// Register a static `Err(...)` response.
    pub fn on_err(&self, method: &str, err: AdminError) -> &Self { ... }

    /// Register a closure responder — receives the request params,
    /// returns the typed result.
    pub fn on_with<F>(&self, method: &str, handler: F) -> &Self
    where
        F: Fn(Value) -> Result<Value, AdminError> + Send + Sync + 'static,
    { ... }

    /// AdminClient bound to this mock — pass to harness or
    /// directly to ToolCtx.
    pub fn client(&self) -> AdminClient { self.client.clone() }

    /// Every request seen so far.
    pub fn requests(&self) -> Vec<MockRequest> { ... }

    /// Filter requests by method.
    pub fn requests_for(&self, method: &str) -> Vec<MockRequest> { ... }
}
```

### `MockAdminSender`

Implements `AdminSender::send_line`:
1. Parse the JSON-RPC frame
2. Push request log
3. Look up handler for method; call it with params
4. Build response frame `{jsonrpc, id, result|error}`
5. `tokio::spawn` a task that calls `client.on_inbound_response(id, frame)`

Chicken-and-egg: client needs sender at ctor, sender needs client to deliver
responses. Solved by storing `Arc<RwLock<Option<AdminClient>>>` in the sender;
constructor wires the client back after building.

Method-not-registered case: returns `AdminError::MethodNotFound` so callers
debug missing setup easily.

### Harness integration

```rust
pub fn with_admin_mock(self, mock: &MockAdminRpc) -> Self
```

Replaces `Handlers.admin = None` with `Some(Arc::new(mock.client()))` so
`ToolCtx::admin()` returns the mock client.

## Tests

1. `MockAdminRpc.on(method, value)` → tool's `ctx.admin().call(method, ...)`
   returns `Ok(value)`.
2. `on_err` → tool receives the typed `AdminError`.
3. Method without registration → `MethodNotFound` (defense-in-depth).
4. `on_with` closure receives params; tool can echo them.
5. `requests_for` returns Vec with each call.
6. Multiple methods registered independently — no cross-talk.
7. Harness integration: `MicroappTestHarness::with_admin_mock` makes
   `ctx.admin()` return Some.

## Pasos

1. New module `crates/microapp-sdk/src/admin/mock.rs` (~200 líneas).
2. Wire from `admin/mod.rs` (`pub use mock::*;`).
3. Harness `with_admin_mock` builder method (small).
4. 7 tests (5 in mock module, 2 in harness module).
5. Docs: README snippet on testing tools + admin (small block in
   `crates/microapp-sdk/README.md`).

## Próximo

Listo para ejecutar.
