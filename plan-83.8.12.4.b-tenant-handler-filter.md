# Plan 83.8.12.4.b — handler-level tenant filter + writer tenant_id

## Refs

- **OpenClaw `research/src/infra/agent-events.ts:200-218`** — emit enrichment from per-run context, atomic single-step pattern. Cada paso aquí ataca un sitio distinto del enrichment chain (writer emit, reader emit, handler post-filter).
- **`claude-code-leak/`** — sin precedente. Absence declarada.

## Pasos

### Paso 1 — TranscriptWriter populates tenant_id

Archivos:
- `crates/core/src/agent/transcripts.rs`

Cambios:
- Añadir field `tenant_id: Option<String>` al struct.
- Builder `with_tenant_id(self, tenant: Option<String>) -> Self`.
- `new()` y `with_extras()` initial-default a `None`.
- Emit site (~line 277): reemplazar `tenant_id: None` con `self.tenant_id.clone()`.
- Test: nuevo unit test que construye writer `with_tenant_id(Some("acme"))`, append a session, verifica que el evento emitido lleva `tenant_id: Some("acme")`. Reusa el `TestEmitter` ya en el módulo si existe; sino, capture mock.

Done criterion: `cargo test -p nexo-core --lib agent::transcripts` verde, sin breaking en tests existentes (todos siguen llamando `new()` → `tenant_id: None`).

### Paso 2 — TranscriptReaderFs populates + gates tenant_id

Archivos:
- `crates/setup/src/admin_adapters.rs` (TranscriptReaderFs).

Cambios:
- Añadir field `tenant_id: Option<String>` al struct.
- Builder `with_tenant_id`.
- `list_recent_events`: gate al inicio (`if filter.tenant_id.is_some() && filter.tenant_id != self.tenant_id { return Vec::new() }`); emit-site `tenant_id: self.tenant_id.clone()`.
- `read_session_events`: misma gate al inicio.
- `search_events`: misma gate al inicio.
- 4 fixtures-tests del módulo (`TranscriptReaderFs::new(... "ana")`) siguen sin tenant — back-compat.
- 3 nuevos tests:
  - cross-tenant list returns empty
  - same-tenant list returns events with populated tenant_id
  - reader without tenant_id + filter without tenant_id → unchanged behavior

Done criterion: `cargo test -p nexo-setup` verde.

### Paso 3 — escalations::list handler-side tenant filter

Archivos:
- `crates/core/src/agent/admin_rpc/domains/escalations.rs`
- `crates/core/src/agent/admin_rpc/domains/agents.rs` (promote `agent_tenant_id` to `pub(crate)`)
- `crates/core/src/agent/admin_rpc/dispatcher.rs` (call site for `escalations::list` reads `agents_yaml`)

Cambios:
- `agents.rs`: `fn agent_tenant_id` → `pub(crate) fn`.
- `escalations.rs::list` signature: `pub async fn list(store, patcher: Option<&dyn YamlPatcher>, params)` — patcher es option para no romper tests in-process que no inyectan patcher.
- Post-list filter por tenant cuando ambos están presentes.
- `dispatcher.rs` arm para `nexo/admin/escalations/list`: pasa `self.agents_yaml.as_deref().map(|y| y.as_ref())` al call.
- Tests:
  - 2 nuevos in `escalations.rs`: list con 2 agents distintos tenant + patcher inyectado → filtra; list sin patcher → ignora filter.
  - Existentes pass cuando se inyecta `None` para patcher.

Done criterion: `cargo test -p nexo-core --lib admin_rpc::domains::escalations` verde, dispatcher tests pasan.

### Paso 4 — Boot wire (runtime_snapshot)

Archivos:
- `crates/core/src/runtime_snapshot.rs` (o donde se construya `TranscriptWriter` por agente).

Cambios:
- En la cadena de construcción del writer, llamar `.with_tenant_id(agent.tenant_id.clone())`.
- `nexo-setup`: cualquier sitio que construya `TranscriptReaderFs` para producción threads `agent.tenant_id`.

Done criterion: workspace build verde + boot tests pasan.

### Paso 5 — Docs + FOLLOWUPS close-out

Archivos:
- `FOLLOWUPS.md` — marcar 83.8.12.4.b ✅ los tres deferreds, mover el bloque a "resolved".
- `docs/src/microapps/admin-rpc.md` — una línea sobre "tenant_id en TranscriptAppended events siempre populated cuando agent.tenant_id está set".

Done criterion: `mdbook build docs` verde.

## Riesgos

- 4 sitios de `TranscriptWriter::new(... "kate")` en tests dentro de `transcripts.rs`. Como `new()` queda con default `None`, no breaking.
- 4 sitios `TranscriptReaderFs::new(...)` en `admin_adapters.rs`. Mismo principio.
- `dispatcher.rs` cita `self.agents_yaml` — confirmar que está accesible. Lo está (line 522-528 lo usa para `agents/list`).
- Si la signature change de `escalations::list` rompe consumers fuera del dispatcher (algún test out-of-tree), pivot a un wrapper helper.

## Próximo

Listo para `/forge ejecutar 83.8.12.4.b`.
