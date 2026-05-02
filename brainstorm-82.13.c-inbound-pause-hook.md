# Brainstorm — Phase 82.13.c: inbound dispatcher pause hook

**Tema:** cerrar el gap crítico de 82.13.b — el daemon NO verifica
el state `PausedByOperator` cuando llega un inbound del canal,
así que el agent sigue respondiendo aunque el operador haya
pausado. Esto deja a toda la maquinaria de transcript /
queue / summary shippeada en 82.13.b como pipes desconectados
en producción.

---

## Mining obligatorio

### `research/` (OpenClaw, TypeScript)

OpenClaw NO tiene un análogo directo (cortaron pause/resume del
agent loop, ver brainstorm 82.13.b). Patrones reusables:

- **`sessions.steer` con interrupt** —
  `research/src/gateway/server-methods/sessions.ts:1210-1220`
  intercepta el run activo y manda nuevo mensaje en su lugar.
  No bufferea, ABORTA. **Lección negativa**: no aplica para
  WhatsApp (mensajes async sin run activo). Necesitamos
  buffer (ya shipped en 82.13.b.3.1).
- **Operator/admin scope hierarchy** —
  `research/src/gateway/method-scopes.ts:23-30,149-175`
  separa scopes operacionales (`operator.write`,
  `operator.admin`) de turnos del agent. **Confirma**: el
  pause check vive del lado dispatcher, no inside agent
  behavior — operator scope ≠ agent scope.

### `claude-code-leak/`

Repo no presente local. Mining no aplicable; absencia
explícita declarada.

---

## Estado del código actual (mining propio)

Ya cumplido para encontrar el sitio exacto vía Explore agent:

- `crates/core/src/agent/runtime.rs:706` — `InboundMessage::new`
  se construye después de los gates existentes (binding match
  481, rate limiter 681, pairing gate 603, empty-text 698).
- `crates/core/src/agent/runtime.rs:859` — `tx.try_send(msg)`
  al per-session channel que feeds `flush()` (línea 1359-1495).
- En el sitio de inserción se tiene **TODO** el contexto
  necesario para construir `ProcessingScope::Conversation`:
  - `agent.id` ✅
  - `source_plugin` ✅ (línea 707)
  - `source_instance` ✅ (línea 708) — uso como `account_id`
  - `sender_id` ✅ (línea 709) — uso como `contact_id`
- `InboundMessage` ya tiene id (UUIDv5 derivado), text,
  timestamp, sender_id, source_plugin → suficientes para
  construir `PendingInbound`.
- **Redactor disponible** en runtime (línea 804) pero NO se
  ejecuta hoy antes de `flush()`. Hoy redacta dentro de
  `TranscriptWriter::append_entry` cuando se escribe.

---

## Ideas / propuesta

### Sitio de inserción

**Decisión: justo antes de `tx.try_send(msg)` en línea 859**, NO
antes de construir `InboundMessage`.

Razones:
- `InboundMessage` ya tiene `id` resuelto (UUIDv5) — útil para
  el firehose drop event y para `PendingInbound.message_id`.
- Todos los gates anteriores (rate limiter, pairing, empty-text)
  ya filtraron mensajes basura → no buffereamos junk.
- Si la pausa se activa entre push y try_send hay race
  benign — el peor caso es 1 mensaje extra pasa al agent (la
  carrera ya existe por la asincronía broker → dispatcher).

```
runtime.rs:706  let mut msg = InboundMessage::new(...);
runtime.rs:707-721  populate fields
runtime.rs:722  let message_id = msg.id;
                ━━━ NUEVO check aquí ━━━
                if processing_store.get(scope).await? == PausedByOperator {
                    push_pending(scope, PendingInbound::from(msg));
                    emit firehose drop event if dropped > 0;
                    continue; // skip session spawn + try_send
                }
runtime.rs:737-857  session-channel get-or-spawn
runtime.rs:859  tx.try_send(msg);
```

### Cómo construir el scope

```rust
let scope = ProcessingScope::Conversation {
    agent_id: agent.id.clone(),
    channel: source_plugin.clone(),
    account_id: source_instance
        .clone()
        .unwrap_or_else(|| "default".into()),
    contact_id: sender_id
        .clone()
        .unwrap_or_else(|| "unknown".into()),
    mcp_channel_source: None,
};
```

Nota: `mcp_channel_source` se deja `None` por simplicidad v0;
inbounds vía MCP-channel server (Phase 80.9) tendrían su
propio tagging que se enchufa después.

### Body redacted vs raw en queue

**Decisión: redactar AL push.**

Razones:
- Cap 50 in-memory hoy, pero 82.13.c durable SQLite store
  futuro persiste. Tener body raw en disco = PII a la deriva.
- El redactor ya está disponible en runtime (`Arc<Redactor>`).
- Stamping en transcript al drain via `TranscriptWriter`
  redacta otra vez (idempotente — re-aplicar regex sobre
  `[REDACTED:phone]` no rompe). Doble redacción es OK.
- Tests verifican que body buffereado nunca contiene
  patrones PII raw.

Implementation:
```rust
let body = if let Some(r) = &redactor {
    r.apply(&msg.text).redacted_text
} else {
    msg.text.clone()
};
```

### Firehose drop event

Cuando `push_pending` retorna `dropped > 0`:

```rust
emitter.emit(AgentEventKind::PendingInboundsDropped {
    agent_id: agent.id.clone(),
    scope: scope.clone(),
    dropped,
    at_ms: now_epoch_ms(),
}).await;
```

`emitter` es el `Arc<dyn AgentEventEmitter>` ya inyectado a
runtime para Phase 82.11 firehose. No requiere wire-up
nuevo.

### Inyección al runtime

Runtime hoy no tiene `Arc<dyn ProcessingControlStore>`. Hay
que threading desde boot. Options:

- **A)** Field opcional en runtime constructor.
  `Option<Arc<dyn ProcessingControlStore>>`. Si `None`,
  comportamiento actual (no check). Backwards-compat
  total.
- **B)** Field obligatorio con `NoopStore` default. Force
  cada caller a explicitar.

Recomendado: **A**. Tests que no usan pause no necesitan
construir un store; producción wire en boot.

### Boot wire-up

- `crates/setup/src/admin_bootstrap.rs` ya construye
  `InMemoryProcessingControlStore` para el dispatcher.
- Boot tiene que **compartir** la misma instancia con runtime
  (no construir 2 — sería un store por dispatcher y otro por
  runtime, no se sincronizarían).
- En `main.rs` o donde se construya el runtime: pasar el
  shared `Arc` del store.
- El `ProcessingControlStore` ya implementa `Clone` (`Arc`
  inside). Compartir es trivial.

---

## Decisiones cerradas

| # | Decisión |
|---|----------|
| D1 | Sitio: `runtime.rs:722` (después de extraer `message_id`, antes del session spawn + `try_send`). |
| D2 | Body redactado AL push (no raw, future-proof contra durable store). |
| D3 | Drop event vía firehose existente (`AgentEventEmitter` ya inyectado). |
| D4 | Field `processing_store: Option<Arc<dyn ProcessingControlStore>>` en runtime — default `None` para tests, prod wires desde boot. |
| D5 | Shared instance entre admin RPC dispatcher + runtime (boot construye una, pasa a ambos). |
| D6 | `mcp_channel_source: None` v0 (MCP-channel inbounds enchufan después si se necesita). |
| D7 | `account_id` cae a `"default"`, `contact_id` cae a `"unknown"` cuando faltan campos — coherente con cómo se construye `ProcessingScope::Conversation` desde admin RPC pause/resume params. |

---

## Riesgos transversales

- **Race entre pause y inbound concurrente**: si operator
  hace pause y simultáneamente llega un inbound, hay ventana
  donde el read de `store.get()` ve `AgentActive` y el agent
  sigue. Aceptable: la ventana es ~ms, próximo inbound queda
  bufferado correctamente. Operator ve un agent reply
  "huérfano" en transcript que ya estaba en vuelo.

- **Cross-tenant leak**: scope incluye `agent_id` que ya está
  pinned al runtime (no se puede leer scope ajeno). DashMap
  keying garantiza aislamiento. Verificado por tests
  existentes en 82.13.b.3.1.

- **store.get() error**: si el store falla (red SQLite
  futuro), default a "no paused" y log warn. Fail-open evita
  que un store roto bloquee toda la operación. Trade-off: en
  un mundo donde el operator esperaba que pause funcione,
  fail-open puede causar respuestas "no autorizadas". Pero
  fail-closed congela TODA la app por un bug del store —
  peor. Documentar.

- **Performance**: `store.get()` es DashMap read = ~100ns.
  Negligible vs el LLM call que viene después.

- **Dead store en runtime**: si nadie llama pause/resume, el
  store queda vacío y `get()` siempre devuelve `AgentActive`
  → check es no-op. Runtime sin pause infrastructure simple-
  mente lo ignora con `None`. Verificado por test path.

---

## Robusto + óptimo + transversal (memory rule)

- ✅ **Defensive (10+ edge cases)**:
  - missing source_instance → `"default"`
  - missing sender_id → `"unknown"`
  - empty text + no media → ya filtrado por gate previo
  - rate-limited inbound → ya filtrado
  - sender_trusted=false sin pairing → ya filtrado
  - store.get() Err → fail-open + log
  - push_pending Err → fail-open + log (raw msg pasa al
    agent, NO peor que sin la feature)
  - cap exceeded → drop oldest + firehose event
  - cross-tenant scope → DashMap keying
  - race pause vs inbound → ventana ms, benign
- ✅ **Eficiente**: DashMap read O(1) hash lookup. Redactor
  `apply()` ya optimizado (regex compiled at boot).
- ✅ **Cross-channel**: scope se construye agnostic a
  whatsapp/telegram/email. Misma lógica para todos.
- ✅ **Memory rules**: tenant via `BindingContext` ya
  threaded. No-SaaS deploy funciona idéntico (scope-keyed,
  no tenant-keyed para v0).

---

## Listo para `/forge spec 82.13.c`
