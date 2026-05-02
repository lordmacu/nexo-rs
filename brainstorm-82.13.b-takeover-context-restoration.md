# Brainstorm — Phase 82.13.b: IA awareness during/after operator takeover

**Tema:** cuando operador pausa un chat WhatsApp, interviene
manualmente y luego libera, el agente debe reanudar con
contexto completo de lo que pasó durante la pausa.

**Hoy:** agent reanuda *blind* — no ve mensajes que el cliente
mandó durante la pausa, ni lo que el operador respondió, ni un
resumen de lo ocurrido.

---

## Mining obligatorio

### `research/` (OpenClaw, TypeScript)

OpenClaw **no implementa pausa/resume del agent loop** — los
agents no tienen state machine de "paused". Pero sí tiene 4
patterns reusables:

1. **`chat.inject` admin RPC — synthetic transcript injection**
   - `research/src/gateway/server-methods/chat-transcript-inject.ts:44-116`
     `appendInjectedAssistantMessageToTranscript()` agrega un
     mensaje synthetic con `role: "assistant"`,
     `provider: "openclaw"`, `model: "gateway-injected"` para
     distinguir de output del LLM. El agent en su próximo turno lo
     ve como assistant message normal.
   - `research/src/gateway/server-methods/chat.ts:2349-2411`
     handler RPC `chat.inject` (admin scope), acepta `label`
     opcional que se prepended como `[label]\n\n<body>`.
   - **Lección:** la idea de stampear "asistencia operador" como
     `role: Assistant` + metadata distintiva (`source_plugin =
     "intervention"` en nuestro caso) está validada en
     producción.

2. **Operator scope hierarchy en RPC**
   - `research/src/gateway/method-scopes.ts:23-30,149-175` —
     scopes `operator.admin / operator.write / operator.pairing`
     con `chat.inject` en `ADMIN_SCOPE`.
   - **Lección:** "operator" es concepto auth/RPC, NO un role en
     transcript. Mantener nuestros 4 roles (User/Assistant/Tool/
     System) y usar `sender_id: "operator:<token_hash>"` para
     marcar autoría.

3. **Session steer con interrupt — NO buffer**
   - `research/src/gateway/server-methods/sessions.ts:1210-1220` —
     `sessions.steer` aborta run activo y manda nuevo mensaje, no
     hay queue.
   - **Lección negativa:** OpenClaw cortó el queue porque su
     contexto es CLI (1 user activo, agente responde rápido). En
     WhatsApp NO podemos abortar al cliente — sus mensajes
     llegan asíncrono mientras el operador escribe. Necesitamos
     queue server-side.

4. **`replaySessionTranscript` al cargar sesión**
   - `research/src/acp/translator.ts:1336-1352` —
     `replaySessionTranscript()` re-emite mensajes persistidos al
     cargar una sesión. Mensajes vanilla, sin synthetic summaries.
   - **Lección:** si los mensajes ya están EN el transcript
     (porque los stampeamos durante pausa), el replay automático
     en próximo turno los entrega al LLM "gratis" — no necesitamos
     una `release(summary_for_agent)` separada para ese caso.
     Solo necesitamos asegurar que durante pausa **escribimos** al
     transcript en lugar de descartar.

### `claude-code-leak/`

Repo no presente en `/home/familia/chat/`. Mining no aplicable
para este brainstorm. Memory rule cumplida: **absencia declarada
explícitamente**.

---

## Estado actual del código (puntos de inyección)

- `crates/core/src/agent/admin_rpc/domains/processing.rs:251-298` —
  handler `intervention()` ya despacha `Reply` por
  `ChannelOutboundDispatcher::send()` y obtiene
  `outbound_message_id`. Falta: stampear en transcript después
  del `send()` exitoso.
- `crates/core/src/agent/admin_rpc/domains/processing.rs:121` —
  handler `pause()` cambia state a `PausedByOperator`. Falta:
  state row no tiene `pending_inbounds`.
- `crates/core/src/agent/transcripts.rs:148-205` —
  `TranscriptWriter::append_entry(session_id, entry)` ya
  redacta + persiste JSONL + indexa FTS + emite firehose. API
  lista para reuso.
- `crates/core/src/agent/transcripts.rs:54-67` — `TranscriptEntry`
  ya tiene `sender_id: Option<String>` y `source_plugin: String`.
  No requiere extender shape.
- `crates/microapp-sdk/src/admin/takeover.rs:128-146` —
  `HumanTakeover::release(summary_for_agent)` ya recibe el
  parámetro pero lo descarta (`_summary_for_agent`). El handler
  `processing/resume` no lo acepta tampoco. Falta: añadir el
  param al wire shape + propagación al handler.

---

## Ideas / propuesta de diseño

### Idea 1 — "Stamp-on-send" de operator replies (MVP, mayor valor)

**Cuando** `intervention()` despacha `Reply` con éxito,
inmediatamente después del `disp.send()` invoca
`TranscriptWriter::append_entry(session_id, entry)` con:

```rust
TranscriptEntry {
    timestamp: Utc::now(),
    role: TranscriptRole::Assistant,        // ← agent ve como suyo al replay
    content: body.clone(),                  // ← redactor pasa después
    message_id: ack.outbound_message_id.map(uuid_from),
    source_plugin: format!("intervention:{channel}"),
    sender_id: Some(format!("operator:{token_hash}")),
}
```

- `role: Assistant` asegura que el LLM, al leer la sesión,
  reciba el mensaje como contexto que él mismo "dijo" (continuidad
  natural de tono, no requiere prompt-engineering en system).
- `source_plugin = "intervention:<channel>"` permite a
  observabilidad filtrar mensajes de operador vs LLM real.
- `sender_id = "operator:<hash>"` audita autor sin revelar
  identidad PII.
- Reusa redactor + FTS + firehose existentes — un solo
  `append_entry` cubre todo.

**Requisitos para hacerlo:**
- Resolver `session_id` para la conversación. Opciones:
  - **A)** Operator pasa `session_id` explícito en
    `ProcessingInterventionParams` (la UI del microapp ya tiene
    la conversación abierta, conoce el id).
  - **B)** Lookup `(agent_id, channel, account_id, contact_id)
    → active_session_id` vía `TranscriptsIndex` (existing crate).
  - **C)** Lookup vía `BindingResolver` — el daemon ya correlaciona
    inbounds a sessions.
  - Recomendación: **A** (corto camino, microapp ya sabe). B/C
    como follow-up si la UI evoluciona y el operator quiere
    intervenir desde un contexto sin session id (ej. dashboard
    de notificaciones).
- Inyectar `Arc<TranscriptWriter>` (o trait `TranscriptAppender`)
  en el dispatcher + handler `intervention`. Ya hay precedente:
  `outbound: Option<&dyn ChannelOutboundDispatcher>` se inyecta
  igual.

**Coverage:** cierra la pregunta del usuario para 2 de 3 gaps —
"agent ve lo que dijo el operador" + "agent reanuda coherente
con su propio output reciente". Falta: "agent ve lo que el
cliente dijo durante la pausa".

---

### Idea 2 — `pending_inbounds` queue durante pausa (mayor refactor)

**Hoy** el inbound dispatcher, al recibir un mensaje WhatsApp
durante `PausedByOperator`, hace nada (descarta, lo loggea, no
fire turn). El cliente no recibe ack del agente pero su mensaje
queda solo en transcript del canal.

**Propuesta:** el inbound dispatcher, antes de descartar, hace
`store.push_pending(scope, inbound)`. La queue vive en
`ProcessingControlStore` (ya existe trait). En `resume()`:

1. Lee `pending_inbounds` para el scope.
2. Por cada inbound buffereado, escribe `TranscriptEntry { role:
   User, ... }` en el transcript con timestamp original (no `now`).
3. Limpia la queue.
4. Cambia state a `Active`.
5. Dispara una sola corrida del agente (no una por inbound) —
   el agente recibe N mensajes contiguos en su contexto y
   responde con un único turno coherente.

**Riesgos:**
- Memory: si nadie ejecuta `resume()`, queue crece sin límite.
  Mitigación: cap por scope (ej. 50 inbounds), drop oldest +
  emit firehose `pending_inbound_dropped`.
- Restart pierde queue (in-memory). Mitigación: store SQLite
  cuando exista. Para MVP, in-memory está bien (operator
  típicamente cierra el takeover en minutos).
- Ordering: si llegan inbounds en paralelo durante el push,
  necesitamos timestamp/seq monotónico server-side
  (`Instant::now()` es suficiente, ya lo usamos en
  `TranscriptWriter`).

**Coverage:** cierra el último gap. Combinado con Idea 1, agent
reanuda viendo TODO: sus respuestas pre-pausa (transcript), las
del operador (Idea 1), las del cliente durante la pausa (Idea 2).

---

### Idea 3 — `summary_for_agent` synthetic injection (escape hatch)

`HumanTakeover::release(summary_for_agent: Option<String>)` ya
existe en SDK como param ignorado. Hacerlo end-to-end:

1. Wire shape `ProcessingResumeParams` gana
   `summary_for_agent: Option<String>` + `session_id:
   Option<Uuid>`.
2. SDK forwards el param.
3. Handler `resume()`, si llega, inyecta
   `TranscriptEntry { role: System, content: format!(
   "[operator_summary] {body}"), source_plugin:
   "intervention:summary", sender_id:
   Some(format!("operator:{token_hash}")), ... }` en el
   transcript ANTES de cambiar state a Active.
4. Operator UI ofrece textbox opcional al hacer "release".

**Cuándo se usa:** el operador no quiere replay literal
(Idea 2 podría meter ruido — clientes a veces escriben algo
contradictorio durante intervención humana, o el operador
prefiere resumir 5 mensajes en 1 línea). Con summary, el
operador dice "cliente confirmó dirección, IA puede continuar
con confirmación de envío" y eso queda como `System` directive.

**Combinable con Idea 1+2** o standalone. No requiere queue.

---

### Idea 4 — UI hint: "operator wrote N messages" badge

Standalone polish: el firehose ya emite `TranscriptAppended`
cuando entra el `role: Assistant` con
`source_plugin: "intervention:..."`. El microapp UI puede
distinguir visualmente:

- LLM message: bubble normal + avatar agent
- Operator message (vía Idea 1): bubble normal + avatar
  "Operador" + label sutil

**Sin código de framework adicional** — solo el discriminator
`source_plugin: "intervention:..."` en el wire (que sale gratis
de Idea 1). Se documenta como contrato y la UI lo consume.

---

## Decisiones a tomar en spec

1. **Orden de implementación.**
   - Recomendado: Idea 1 (MVP) → Idea 3 (summary) → Idea 2
     (queue). Razón: 1+3 cubren el 80% del valor con ~2 commits;
     2 es 3 commits + cap/drop policy + tests de concurrencia.
   - Alternativa: 1 → 2 → 3, si el feedback de operadores prioriza
     "agent debe ver TODO lo que cliente escribió" sobre
     "operator narrativa".

2. **Resolver `session_id`.**
   - Recomendado: param explícito en `ProcessingInterventionParams`
     y `ProcessingResumeParams`. Microapp ya tiene la sesión
     abierta. Trait `TranscriptAppender` minimal, no requiere
     index dependency.
   - Trade-off: si el operador quiere intervenir desde un
     contexto sin session abierta (ej. notification deep-link),
     debe primero abrir la sesión. Aceptable para v1.

3. **`source_plugin` discriminator format.**
   - `intervention:<channel>` (ej. `intervention:whatsapp`) →
     observabilidad puede agrupar por canal.
   - Alternativas: `operator`, `intervention`, `<channel>:operator`.
   - Recomendado: `intervention:<channel>` por consistencia con
     `source_plugin: "<channel>"` ya usado para mensajes
     normales.

4. **Buffered inbounds: ¿in-memory o SQLite ahora?**
   - Recomendado: in-memory + cap 50 + drop oldest + firehose
     event. SQLite cuando aparezca el follow-up `82.13.c durable
     store` (ya logged).

5. **`role` para operator reply: `Assistant` o nuevo
   `Operator`.**
   - OpenClaw usó assistant + metadata. Recomendado igual:
     evitamos refactor de `TranscriptRole` (4 → 5 variants
     toca SDK + serde + FTS index + tests). Discriminador vive en
     `source_plugin` + `sender_id`.
   - Trade-off: si futuro requiere "show Operator role como
     entidad UI distinta" sin parsear `source_plugin`, refactor
     después. Coste bajo dado que `TranscriptRole` ya tiene 4
     variants y agregar uno es additive.

6. **Sub-fase split.**
   - **82.13.b.1** = Idea 1 (stamp-on-send) — wire shape
     `session_id` param + handler hooks transcript writer + test.
   - **82.13.b.2** = Idea 3 (summary_for_agent) — wire shape
     extension + resume handler injection + SDK forward.
   - **82.13.b.3** = Idea 2 (pending_inbounds queue) — store
     extension + dispatcher hook + resume drain + cap policy.
   - Cada uno commit-able + ship-able independiente. UI v1 se
     beneficia con solo 82.13.b.1.

---

## Riesgos transversales

- **Race condition** entre operador escribiendo `Reply` y
  cliente respondiendo en paralelo. Si `pending_inbounds` queue
  está activa, el orden temporal queda preservado por el
  timestamp del `TranscriptEntry`. Sin queue (solo Idea 1),
  algunos inbounds del cliente aparecen en transcript sin que el
  agent reaccione hasta el resume — visible en firehose UI como
  "huérfanos", aceptable para MVP.

- **Redacción doble**: `TranscriptWriter::append_entry` corre
  redactor SOBRE el `content`. Si el operador escribe contenido
  que coincide con un patrón redactable (PII), redactor lo
  borra antes de stampear. Bueno por defecto — el operador no
  filtra PII al escribir manualmente. La UI puede surfacear el
  contenido original del log de audit (que sí tiene
  `operator_token_hash`) para debugging.

- **`session_id` ausente**: si la microapp no envía
  `session_id` (ej. operador interviene desde notification
  raíz), Idea 1 debe degradar silenciosamente — log warning,
  manda Reply al canal sin stampear transcript, devuelve ack
  con flag `transcript_stamped: false`. Operador UI se entera
  y puede abrir conversación + retry si quiere historial.

- **Multi-tenant**: ya cubierto. La sesión vive bajo
  `state/<tenant>/<agent>/sessions/<id>.jsonl`, redactor +
  FTS + firehose ya respetan el path. Stamp adicional no
  requiere consideración tenant — el writer ya está bound al
  agent del scope.

- **Audit trail**: cada `intervention()` ya escribe en
  `admin_audit` SQLite (Phase 82.10.h). Stamp en transcript NO
  reemplaza audit — son artefactos distintos: audit = "operador
  hizo intervención", transcript = "agent ve esta línea como
  contexto futuro". Both row la misma operación, no duplican.

---

## Checklist robusto + óptimo + transversal (memory rule)

- ✅ Defensive: 5+ edge cases (race, missing session, redactor
  collision, queue overflow, timestamp ordering, audit
  decoupling). Fail-closed cuando session_id ausente. Cap +
  drop policy en queue. Idempotente — re-stamping del mismo
  reply no rompe (timestamp distinto = entry distinto).
- ✅ Eficiente: reusa `TranscriptWriter` existente (un fsync
  por entry, ya optimizado). No nuevas dependencies. Queue
  in-memory con `Arc<Mutex<Vec<_>>>` por scope (ya existe en
  store).
- ✅ Cross-provider: lógica framework agnóstica. WhatsApp /
  Telegram / Email comparten misma path (todos llaman
  `ChannelOutboundDispatcher` ya). El stamp ocurre del lado
  framework, no del plugin.
- ✅ Memory rules: `tenant_id` se resuelve via
  `BindingContext` ya threaded en sessions. No-SaaS deploy
  funciona idéntico (no toca el campo).

---

## Listo para `/forge spec 82.13.b`
