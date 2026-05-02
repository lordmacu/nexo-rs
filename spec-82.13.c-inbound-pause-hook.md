# Spec — Phase 82.13.c: inbound dispatcher pause hook

Cierra el gap crítico de 82.13.b: hoy el daemon NO verifica
state `PausedByOperator` cuando llega un inbound de canal,
así que el agent sigue respondiendo aunque el operator haya
pausado. Esta spec liga la maquinaria de transcript /
queue / summary shippeada en 82.13.b al inbound dispatch
real.

---

## Mining citado

- `research/sessions.ts:1210-1220` — `sessions.steer` valida
  el patrón "intercept-en-el-dispatcher antes de fire_turn"
  como punto correcto de check (vs hacerlo desde dentro del
  agent behavior). Patrón aplicado: el check vive en
  runtime.rs, no en llm_behavior.rs.
- `claude-code-leak/` — no presente local. Absencia
  declarada.

---

## Decisiones cerradas (del brainstorm)

| # | Decisión |
|---|----------|
| D1 | Sitio inserción: `runtime.rs:722` (tras `let message_id = msg.id;`, antes del session-spawn block). |
| D2 | Body redactado AL push (future-proof contra durable store). |
| D3 | Firehose drop event vía `AgentEventEmitter` existente. |
| D4 | `runtime.processing_store: Option<Arc<dyn ProcessingControlStore>>` con `None` default. |
| D5 | Shared instance entre admin RPC + runtime (boot construye una, pasa a ambos). |
| D6 | `mcp_channel_source: None` v0. |
| D7 | Fallbacks: `account_id` → `"default"`, `contact_id` → `"unknown"`. |
| D8 | Fail-open en errores del store (log warn, raw msg pasa). |

---

## Wire shape changes

Ninguno nuevo. Reusa todo de 82.13.b:
- `ProcessingScope::Conversation` (tool-meta)
- `ProcessingControlState::{AgentActive, PausedByOperator}` (tool-meta)
- `PendingInbound` (tool-meta)
- `AgentEventKind::PendingInboundsDropped` (tool-meta)
- `ProcessingControlStore` trait (nexo-core)
- `InMemoryProcessingControlStore` (nexo-setup)
- `AgentEventEmitter` (nexo-core)

---

## Trait surfaces

Ningún trait nuevo. Solo extensión de `Runtime` con campo
opcional + builder.

---

## Implementation

### `crates/core/src/agent/runtime.rs`

**Field nuevo en runtime struct** (busca el struct Runtime):

```rust
pub struct Runtime {
    // ... existing fields ...
    /// Phase 82.13.c — operator pause check. When `Some`, the
    /// inbound intake loop calls `get(scope)` on every inbound;
    /// `PausedByOperator` triggers `push_pending` instead of
    /// firing the agent turn. `None` keeps the legacy
    /// "process every inbound" behaviour for tests + daemons
    /// without admin RPC.
    processing_store: Option<Arc<dyn ProcessingControlStore>>,
}
```

**Builder:**

```rust
impl Runtime {
    pub fn with_processing_store(
        mut self,
        store: Arc<dyn ProcessingControlStore>,
    ) -> Self {
        self.processing_store = Some(store);
        self
    }
}
```

**Hook en intake loop** (alrededor de `runtime.rs:706-720`):

Tras construir `msg` y extraer `message_id`, ANTES del
session-spawn block:

```rust
// Phase 82.13.c — pause check. Build the
// ProcessingScope::Conversation from the inbound binding
// data and consult the store. When paused, redact the body
// and push to the per-scope buffer so resume() can drain it
// onto the transcript as a User entry.
if let Some(ps) = &processing_store {
    let scope = ProcessingScope::Conversation {
        agent_id: agent.id.clone(),
        channel: msg.source_plugin.clone(),
        account_id: msg
            .source_instance
            .clone()
            .unwrap_or_else(|| "default".into()),
        contact_id: msg
            .sender_id
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        mcp_channel_source: None,
    };
    let paused = match ps.get(&scope).await {
        Ok(ProcessingControlState::PausedByOperator { .. }) => true,
        Ok(_) => false,
        Err(e) => {
            // Fail-open: a broken store must not freeze the
            // whole inbound loop. Worst case the operator's
            // pause briefly leaks one inbound through.
            tracing::warn!(
                error = %e,
                agent_id = %agent.id,
                "processing_store.get failed; treating as not paused",
            );
            false
        }
    };
    if paused {
        // Redact body BEFORE pushing — keeps PII out of the
        // queue (cap-bounded in-memory now, future durable
        // SQLite store down the line).
        let redacted_body = if let Some(r) = &redactor {
            r.apply(&msg.text).redacted_text
        } else {
            msg.text.clone()
        };
        let pending = PendingInbound {
            message_id: Some(msg.id),
            from_contact_id: msg
                .sender_id
                .clone()
                .unwrap_or_else(|| "unknown".into()),
            body: redacted_body,
            timestamp_ms: msg.timestamp.timestamp_millis() as u64,
            source_plugin: msg.source_plugin.clone(),
        };
        match ps.push_pending(&scope, pending).await {
            Ok((depth, dropped)) => {
                tracing::debug!(
                    agent_id = %agent.id,
                    depth,
                    dropped,
                    "inbound buffered while paused",
                );
                if dropped > 0 {
                    if let Some(em) = &event_emitter {
                        em.emit(AgentEventKind::PendingInboundsDropped {
                            agent_id: agent.id.clone(),
                            scope: scope.clone(),
                            dropped,
                            at_ms: now_epoch_ms(),
                        })
                        .await;
                    }
                }
            }
            Err(e) => {
                // Fail-open same as get(). Logging only —
                // raw message will fall through to the
                // session channel below.
                tracing::warn!(
                    error = %e,
                    agent_id = %agent.id,
                    "push_pending failed; inbound will fire turn",
                );
            }
        }
        continue; // skip session-spawn + try_send
    }
}
```

### Boot wiring — `crates/setup/src/admin_bootstrap.rs`

Hoy el bootstrap construye `InMemoryProcessingControlStore`
adentro de `with_processing_domain` para el dispatcher. Hay
que extraerlo a una sola instancia compartible:

**Antes:** dispatcher tiene su propio store privado.

**Después:** boot crea una instancia, la registra en
`AdminBootstrapInputs.processing_store` (campo nuevo). El
dispatcher usa esa misma referencia. Runtime también la
recibe via `Runtime::with_processing_store`.

```rust
// In AdminBootstrapInputs:
pub processing_store: Option<Arc<dyn ProcessingControlStore>>,
```

`main.rs` (o donde se construya runtime + bootstrap):

```rust
let processing_store: Arc<dyn ProcessingControlStore> = Arc::new(
    InMemoryProcessingControlStore::with_pending_cap(
        std::env::var("NEXO_PROCESSING_PENDING_QUEUE_CAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_PENDING_INBOUNDS_CAP),
    ),
);
let runtime = Runtime::new(...)
    .with_processing_store(processing_store.clone());
let bootstrap = AdminRpcBootstrap::build(AdminBootstrapInputs {
    processing_store: Some(processing_store.clone()),
    // ... existing fields ...
}).await?;
```

### Tests

**`runtime.rs` integration test (NEW):**

- `inbound_during_pause_buffers_instead_of_firing`:
  - Build runtime with mock `ProcessingControlStore` (state
    set to PausedByOperator for one scope).
  - Send inbound matching that scope.
  - Verify: agent behavior NOT invoked (use a recording
    behavior), store has 1 pending.
- `inbound_active_scope_passes_through`:
  - State `AgentActive` for scope.
  - Inbound received → behavior INVOKED, store has 0
    pending.
- `inbound_when_store_unwired_fires_legacy_path`:
  - `processing_store: None`.
  - Inbound received → behavior invoked, no panic.
- `inbound_when_store_get_fails_fails_open`:
  - Mock store returns Err on get.
  - Inbound received → behavior INVOKED (fail-open),
    warning logged.
- `body_is_redacted_before_push`:
  - Runtime built with redactor + paused store.
  - Inbound with PII pattern → store has 1 pending whose
    body is redacted (no raw PII).
- `cap_exceeded_emits_drop_event`:
  - Store with cap=2 + paused.
  - Send 3 inbounds → firehose receives 1
    PendingInboundsDropped event with dropped=1.

**Existing tests** must keep passing — pause check is
gated behind `processing_store.is_some()`, and existing
runtime constructors leave it `None`.

### Capability gate

No new capability. The pause check is gated on
`with_processing_store()` being called at boot. Operator
already controls the wire-up.

### Audit log

No new audit entries — pause/resume itself audit-logs (Phase
82.10.h). Pending push doesn't need audit (high volume,
firehose is sufficient).

### Compatibility

- All existing tests keep passing because `processing_store`
  defaults to `None`.
- Microapps that don't pause behave identically (store stays
  empty, every `get` returns `AgentActive`).
- Single-tenant deployments work without changes.

---

## Done criteria

- [ ] `cargo build --workspace` clean.
- [ ] All existing runtime tests pass.
- [ ] 6 new tests pass.
- [ ] `mdbook build docs` clean.
- [ ] FOLLOWUPS.md updated: 82.13's "Inbound dispatcher hook"
  ✅ shipped 2026-05-02 as 82.13.c.
- [ ] `docs/src/microapps/admin-rpc.md` updated: remove the
  "**Note (Phase 82.13.b.3.2 limitation)**" caveat about the
  dispatcher push hook being deferred — the round-trip is
  now end-to-end.
- [ ] Smoke manual: pause a conversation, send 3 WhatsApp
  inbounds, confirm agent doesn't reply, resume, confirm
  3 User entries land on transcript with original
  timestamps + 1 fresh agent reply on the next inbound.

---

## Listo para `/forge plan 82.13.c`
