# Spec 83.8.12.4.b — handler-level tenant filter + writer tenant_id

## Refs

- **OpenClaw `research/src/infra/agent-events.ts:99-218`** — emit-time enrichment via per-run context registry. Misma idea, sólo que en Rust el "registry" es el campo del struct (writer per-agent).
- **`claude-code-leak/`** — single-tenant CLI; sin patrón aplicable. Absence declarada.

## Wire shapes

Sin cambios en wire (`AgentEventKind::TranscriptAppended.tenant_id` ya existe; `EscalationsListParams.tenant_id` ya existe; `AgentEventsListFilter.tenant_id` ya existe). Sólo populación + filtrado.

## TranscriptWriter

```rust
pub struct TranscriptWriter {
    // ... existing fields ...
    /// Phase 83.8.12.4.b — owning tenant. Stamped on every
    /// `TranscriptAppended` event the writer emits. `None` for
    /// single-tenant deployments.
    tenant_id: Option<String>,
}

impl TranscriptWriter {
    pub fn with_tenant_id(mut self, tenant_id: Option<String>) -> Self { ... }
}
```

Emit site (`transcripts.rs:259-278`):

```rust
let event = AgentEventKind::TranscriptAppended {
    // ...
    tenant_id: self.tenant_id.clone(),  // was: None
};
```

## TranscriptReaderFs

```rust
pub struct TranscriptReaderFs {
    // ... existing fields ...
    tenant_id: Option<String>,
}

impl TranscriptReaderFs {
    pub fn with_tenant_id(mut self, tenant_id: Option<String>) -> Self { ... }
}
```

`list_recent_events`:

```rust
// Defense-in-depth — cross-tenant query returns empty.
if let Some(want) = &filter.tenant_id {
    if self.tenant_id.as_deref() != Some(want.as_str()) {
        return Ok(Vec::new());
    }
}
// ...
out.push(AgentEventKind::TranscriptAppended {
    // ...
    tenant_id: self.tenant_id.clone(),  // was: None
});
```

`read_session_events` + `search_events` get the same tenant gate at entry (no cross-tenant access).

## escalations::list

```rust
pub async fn list(
    store: &dyn EscalationStore,
    patcher: Option<&dyn YamlPatcher>,  // NEW — None disables tenant filter
    params: Value,
) -> AdminRpcResult { ... }
```

Post-list filter:

```rust
let entries = store.list(&p).await?;
let entries = match (&p.tenant_id, patcher) {
    (Some(want), Some(patcher)) => entries
        .into_iter()
        .filter(|e| {
            super::agents::agent_tenant_id(patcher, &e.agent_id).as_deref()
                == Some(want.as_str())
        })
        .collect(),
    _ => entries,  // no filter requested OR no patcher wired
};
```

`agent_tenant_id` actualmente es `pub(super) fn` en `agents.rs` — promote a `pub(crate)`.

Dispatcher: `with_escalations_domain(store)` → `with_escalations_domain(store)` y la inyección del patcher la hace el dispatcher leyendo su propio `agents_yaml` field. No nuevo argumento al builder.

## Boot wiring

`runtime_snapshot.rs` (donde se construye el writer por agente): añadir `.with_tenant_id(agent.tenant_id.clone())` en la cadena de construcción. Mismo en cualquier ctor de `TranscriptReaderFs` en `nexo-setup`.

## Casos de prueba

1. `TranscriptAppended` emitido por writer con `tenant_id: Some("acme")` lleva el campo en el payload (no `None`).
2. `TranscriptReaderFs` con tenant `"acme"` ante filter `tenant_id: Some("globex")` → `Vec::new()`.
3. `TranscriptReaderFs` con tenant `"acme"` ante filter `tenant_id: Some("acme")` → eventos populated.
4. Writer sin `with_tenant_id` (legacy) → `tenant_id: None` (back-compat).
5. `escalations::list` con `tenant_id: Some("acme")` y patcher con dos agents (uno tenant=acme, otro tenant=globex) → sólo retorna rows del agent acme.
6. `escalations::list` sin patcher (test fixture) → filter ignorado, todos rows pasan (no break para tests existentes).

## Próximo

Listo para `/forge plan 83.8.12.4.b`.
