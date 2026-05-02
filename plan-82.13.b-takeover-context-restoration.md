# Plan — Phase 82.13.b: IA awareness during/after operator takeover

Atómico. Cada step compila + tests pasan + commit. Sub-fases
ship-ables independientes (b.1 → b.2 → b.3).

---

## Mining citado

- **research/** — `chat-transcript-inject.ts:44-116` valida pattern;
  `replaySessionTranscript()` (acp/translator.ts:1336-1352) garantiza
  que entries persistidas se entregan al LLM "gratis" en próximo turn.
- **claude-code-leak/** — repo no presente en `/home/familia/chat/`.
  Mining no aplicable; absencia declarada explícita.

OpenClaw orden real: introduce admin RPC inject **después** de
tener transcript writer maduro. Aplicamos misma secuencia —
TranscriptAppender trait + adapter ANTES de tocar handler.

---

## 82.13.b.1 — Stamp-on-send (3 steps)

### Step 1 — TranscriptAppender trait + wire shape additions

**Archivos nuevos:**
- `crates/core/src/agent/admin_rpc/transcript_appender.rs`
  - `pub trait TranscriptAppender` con `append(agent_id, session_id, entry)`.
  - `pub struct TranscriptEntry { role, content, source_plugin, sender_id, message_id }` (mirror local, no dep en core::transcripts).
  - `pub enum TranscriptRole { User, Assistant, Tool, System }`.
  - `RecordingTranscriptAppender` test helper en `#[cfg(test)]`.
  - 3 unit tests: trait object-safe, recording captura entries, role enum copy.

**Archivos modificados:**
- `crates/core/src/agent/admin_rpc/mod.rs` — `pub mod transcript_appender;`.
- `crates/tool-meta/src/admin/processing.rs`:
  - `ProcessingInterventionParams`: agregar
    `session_id: Option<Uuid>` con
    `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `ProcessingAck`: agregar
    `transcript_stamped: Option<bool>` con
    `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - 2 tests nuevos: round-trip con `session_id` presente,
    legacy params sin `session_id` deserializa OK.

**Done:**
- `cargo build --workspace` clean.
- `cargo test -p nexo-tool-meta -p nexo-core` clean.
- Existing 82.13 tests siguen verdes (graceful absence).

**Commit:** `feat(82.13.b.1.1): TranscriptAppender trait + ProcessingInterventionParams.session_id`

---

### Step 2 — Handler wire-up + dispatcher inject

**Archivos modificados:**
- `crates/core/src/agent/admin_rpc/dispatcher.rs`:
  - field `transcript_appender: Option<Arc<dyn TranscriptAppender>>`.
  - builder `with_transcript_appender(self, appender) -> Self`.
  - dispatch arm `nexo/admin/processing/intervention` pasa
    `appender.as_deref()` al handler.
- `crates/core/src/agent/admin_rpc/domains/processing.rs`:
  - `intervention()` signature gana
    `appender: Option<&dyn TranscriptAppender>` arg.
  - tras `disp.send(msg).await? = Ok(ack)`: build
    `TranscriptEntry`, llamar `appender.append(...)` cuando
    `session_id + appender` ambos `Some`. Capturar
    Ok/Err para set `transcript_stamped`.
  - degradación graceful: si `session_id` `None` o `appender`
    `None` o `append()` `Err` → `transcript_stamped: Some(false)`,
    Reply ya despachado, ack OK.

**Tests nuevos:**
- `intervention_stamps_transcript_when_session_and_appender_both_set`:
  RecordingAppender + RecordingOutbound, intervention con `Reply`
  + `session_id` → 1 entry capturada, ack
  `transcript_stamped: Some(true)`, role Assistant, source_plugin
  `intervention:whatsapp`, sender_id `operator:<hash>`.
- `intervention_skips_stamp_when_session_id_absent`: outbound sí
  envía, appender 0 entries, ack `transcript_stamped: Some(false)`.
- `intervention_skips_stamp_when_appender_unwired`: outbound sí
  envía, dispatcher sin appender, ack `transcript_stamped: Some(false)`.
- `intervention_degrades_when_appender_returns_err`: appender
  configurado para Err, outbound OK, ack
  `transcript_stamped: Some(false)`, RPC sigue Ok overall.
- `intervention_passes_outbound_message_id_through_when_present`:
  ack del outbound trae message_id → entry.message_id matches.

**Done:**
- `cargo build --workspace --tests` clean.
- 5 tests nuevos pasan.

**Commit:** `feat(82.13.b.1.2): processing/intervention stamps operator reply in transcript`

---

### Step 3 — Production adapter + SDK + boot wire + docs

**Archivos modificados:**
- `crates/setup/src/admin_adapters.rs`:
  - `pub struct TranscriptWriterAppender { writer: Arc<TranscriptWriter> }`.
  - `impl TranscriptAppender` traduce `TranscriptEntry` local → core type +
    `writer.append_entry(session_id, ...)`.
  - 2 unit tests: round-trip captura en JSONL temp dir.
- `crates/setup/src/admin_bootstrap.rs`:
  - `AdminBootstrapInputs.transcript_writer: Option<Arc<TranscriptWriter>>`.
  - Si `Some`, instancia `TranscriptWriterAppender`, pasa al dispatcher
    via `with_transcript_appender`.
- `crates/microapp-sdk/src/admin/takeover.rs`:
  - `SendReplyArgs` agrega `pub session_id: Option<Uuid>`.
  - `HumanTakeover::send_reply(&self, args)` propaga al payload JSON
    cuando `Some`.
  - 1 test: `send_reply` con session_id se ve en frame outbound.
- `src/main.rs`:
  - boot construye `TranscriptWriter` (ya existe), lo pasa a
    `AdminBootstrapInputs.transcript_writer`.
- `docs/src/microapps/takeover.md`:
  - Sección nueva "Transcript stamping" con ejemplo
    intervention + session_id + bullet sobre discriminator
    `intervention:<channel>` + `operator:<hash>`.
- `admin-ui/PHASES.md`:
  - Checkbox `[ ] Show transcript_stamped indicator on chat panel`.
- `FOLLOWUPS.md`:
  - Marcar 82.13.b.1 ✅ en sección 82.13.b.

**Tests nuevos:**
- `setup` integration test: end-to-end de adapter
  (TempDir + writer + appender + verify JSONL contiene entry).

**Done:**
- `cargo build --workspace --tests` clean.
- `cargo test --workspace` clean.
- `mdbook build docs` clean.
- Boot sequence funciona en `nexo agent <id>` smoke (manual).

**Commit:** `feat(82.13.b.1.3): TranscriptWriterAppender adapter + SDK SendReplyArgs.session_id + docs`

---

## 82.13.b.2 — summary_for_agent end-to-end (2 steps)

### Step 4 — ProcessingResumeParams extension + handler injection

**Archivos modificados:**
- `crates/tool-meta/src/admin/processing.rs`:
  - `ProcessingResumeParams` agrega
    `session_id: Option<Uuid>` y
    `summary_for_agent: Option<String>`.
  - 2 round-trip tests (con summary, sin summary, legacy).
- `crates/core/src/agent/admin_rpc/domains/processing.rs::resume()`:
  - signature gana `appender: Option<&dyn TranscriptAppender>`.
  - validación:
    - `summary_for_agent` empty/whitespace tras trim → InvalidParams "empty_summary".
    - `summary_for_agent.len() > 4096` → InvalidParams "summary_too_long".
    - `summary_for_agent.is_some() && session_id.is_none()` → InvalidParams "session_id_required_with_summary".
  - tras flip a AgentActive: si summary + session + appender
    todos `Some`, build `TranscriptEntry { role: System,
    content: format!("[operator_summary] {}", trimmed),
    source_plugin: "intervention:summary", sender_id:
    Some(format!("operator:{}", token_hash)), message_id: None }`
    + `appender.append(...)`. Err → log warn, no rollback.
- `crates/core/src/agent/admin_rpc/dispatcher.rs`:
  - dispatch arm `processing/resume` pasa appender al handler.

**Tests nuevos:**
- `resume_injects_summary_as_system_entry_with_prefix`: summary
  + session + appender → 1 entry role System, content empieza
  `[operator_summary] `.
- `resume_rejects_summary_without_session_id`: invalid_params.
- `resume_rejects_empty_summary`.
- `resume_rejects_summary_over_4096_chars`.
- `resume_skips_injection_when_no_summary_provided`: legacy
  resume sin summary funciona idéntico (graceful).
- `resume_logs_and_proceeds_when_appender_errs`: appender Err,
  state ya flipped a Active, ack OK.

**Done:**
- `cargo build --workspace --tests` clean.
- 6 tests nuevos pasan.

**Commit:** `feat(82.13.b.2.1): processing/resume injects operator summary as System transcript entry`

---

### Step 5 — SDK release_with_session + docs + close-out

**Archivos modificados:**
- `crates/microapp-sdk/src/admin/takeover.rs`:
  - `HumanTakeover` field `session_id: Option<Uuid>` + builder
    `with_session(self, id) -> Self`.
  - `release()`: deja de descartar `_summary_for_agent`,
    forwardea + incluye `session_id` cuando `Some`.
  - 2 tests: `release(Some("..."))` con session id se ve en
    frame; `release(None)` no manda summary field.
- `docs/src/microapps/takeover.md`:
  - Sección "Operator summary on resume" con ejemplo.
- `admin-ui/PHASES.md`:
  - Checkbox `[ ] Resume + summary textbox in takeover drawer`.
- `FOLLOWUPS.md`:
  - Marcar 82.13.b.2 ✅.

**Done:** Idem step 3.

**Commit:** `feat(82.13.b.2.2): SDK HumanTakeover.release threads summary_for_agent + session_id end-to-end`

---

## 82.13.b.3 — pending_inbounds queue (3 steps)

### Step 6 — Store extension + cap + drop event variant

**Archivos modificados:**
- `crates/tool-meta/src/admin/processing.rs`:
  - new `pub struct PendingInbound { message_id, from_contact_id,
    body, timestamp_ms, source_plugin }` con derives standard.
  - `ProcessingAck` agrega
    `drained_pending: Option<u32>` con serde skip.
  - 2 round-trip tests (PendingInbound + ack with drained).
- `crates/tool-meta/src/admin/agent_events.rs`:
  - `AgentEventKind::PendingInboundsDropped { agent_id,
    scope: ProcessingScope, dropped: u32 }`.
  - 1 test variante serializa con discriminator
    `pending_inbounds_dropped`.
- `crates/core/src/agent/admin_rpc/processing_control.rs`
  (donde `ProcessingControlStore` vive):
  - 3 trait methods nuevos: `push_pending`, `drain_pending`,
    `pending_depth`. Default impl `Err("not_implemented")`
    para implementaciones legacy (graceful en tests, en prod
    el store de v0 se actualiza).
  - `InMemoryProcessingControlStore` agrega
    `pending: DashMap<ProcessingScope, VecDeque<PendingInbound>>`
    + `MAX_PENDING_PER_SCOPE: usize = 50`.
  - `push_pending`: append + si len > cap, pop_front + return
    `(new_len, dropped_count: 1)`. Concurrent-safe por DashMap
    entry lock.
  - `drain_pending`: take VecDeque + return Vec.
- `crates/setup/src/capabilities.rs::INVENTORY`:
  - Entry `NEXO_PROCESSING_PENDING_QUEUE_CAP` (default 50,
    description, recommended_for_observability).

**Tests nuevos:**
- `push_under_cap_no_drops`.
- `push_at_cap_drops_oldest`.
- `push_to_one_scope_isolated_from_other_scopes`.
- `drain_returns_fifo_order_and_clears`.
- `pending_depth_reports_without_draining`.
- `concurrent_pushes_preserve_total_count` (10 tasks * 10
  pushes, expect 100 total minus drops).

**Done:**
- `cargo build --workspace --tests` clean.
- 6 store tests pasan.

**Commit:** `feat(82.13.b.3.1): ProcessingControlStore push_pending/drain_pending + cap policy`

---

### Step 7 — Inbound dispatcher push + resume drain + fire_turn

**Archivos modificados:**
- `crates/core/src/agent/dispatch_handlers.rs` (o el módulo
  exacto donde inbound checa pause state — verificar en step):
  - Sitio que hoy descarta inbound si scope `PausedByOperator`:
    construir `PendingInbound`, llamar
    `store.push_pending(&scope, p).await`. Si `dropped > 0` →
    `emitter.emit(AgentEventKind::PendingInboundsDropped {...}).await`.
  - Mantener el early-return (no fire turn).
- `crates/core/src/agent/admin_rpc/domains/processing.rs::resume()`:
  - tras flip a Active + (opcional) summary inject:
    - `let drained = store.drain_pending(&scope).await?;`
    - Si `session_id + appender + !drained.empty()`: stamp cada
      uno como `TranscriptEntry { role: User, content: it.body,
      source_plugin: it.source_plugin, sender_id:
      Some(it.from_contact_id), message_id: it.message_id }`
      via `appender.append`.
    - Set `ack.drained_pending = Some(drained.len() as u32)`.
    - **fire_turn**: opción A — solo stamp + dejar próximo
      inbound trigger turn. Opción B — agregar trait
      `AgentRuntimeTrigger { fire_turn(agent_id, session_id) }`
      e invocar.
    - Recomendado **A** para MVP: stamp queda visible para LLM
      en próximo inbound natural. Sin Operación B no requiere
      runtime trigger surface ni mantenimiento. Operator UI
      avisa "drain stamped, agent verá en próximo mensaje" si
      `drained_pending > 0` y no hay inbound nuevo.
    - Trade-off doc'd en código + spec actualizada.

**Tests nuevos:**
- `inbound_during_pause_pushed_not_dropped`: inbound
  dispatcher mock + store en `PausedByOperator`, send 3 inbounds,
  store muestra depth 3, 0 turns fired.
- `inbound_over_cap_emits_dropped_event`: send 51 inbounds,
  firehose recibe 1 `PendingInboundsDropped { dropped: 1 }`.
- `resume_drains_into_transcript_user_entries`: store con
  3 pending, resume con session+appender → 3 entries User en
  appender, ack `drained_pending: Some(3)`.
- `resume_drains_only_from_matching_scope`: 2 scopes, drain
  solo afecta el indicado.
- `resume_without_session_drains_but_skips_stamp`: store
  drained (queue clear), 0 entries en appender, ack
  `drained_pending: Some(N)`.

**Done:**
- `cargo build --workspace --tests` clean.
- 5 tests integration pasan.

**Commit:** `feat(82.13.b.3.2): inbound dispatcher pushes pending during pause + resume drains into transcript`

---

### Step 8 — Docs + admin-ui + close-out

**Archivos modificados:**
- `docs/src/microapps/takeover.md`:
  - Sección "Buffered inbounds during pause" con ejemplo +
    cap policy + cap env var.
- `docs/src/configuration/env_vars.md` (o equivalente):
  - Entry `NEXO_PROCESSING_PENDING_QUEUE_CAP`.
- `admin-ui/PHASES.md`:
  - Checkboxes `[ ] Show pending_depth badge in conversation
    list`, `[ ] Show drop event in operator activity feed`.
- `FOLLOWUPS.md`:
  - Marcar 82.13.b.3 ✅.
  - Reabrir / referenciar 82.13.c (durable SQLite store) +
    nuevo follow-up "collapse hint para drains > 50" si
    relevante.

**Done:**
- `cargo build --workspace --tests` + `cargo test
  --workspace` ambos clean.
- `mdbook build docs` clean.
- Smoke manual: pause + 3 inbounds + resume → JSONL contiene
  3 User entries con timestamps originales.

**Commit:** `docs(82.13.b.3.3): pending queue docs + admin-ui sync + close-out`

---

## Riesgos transversales

| Riesgo | Mitigación |
|--------|-----------|
| Race entre `intervention()` stamp y un inbound concurrente que stampea User | `TranscriptWriter::append_entry` usa per-session mutex (`header_locks`) — concurrent appends serializadas. Verificado en Phase 82.11 |
| Redactor borra body del operador si coincide con patrón PII | Documentar en spec + docs. Audit log retiene cleartext via `operator_token_hash` correlación. Operator UI puede surfacear cleartext desde audit |
| Restart pierde queue | In-memory MVP. SQLite via 82.13.c follow-up. Operator UI muestra `pending_depth` para tomar decisiones |
| Token budget con N=50 drained inbounds | Cap=50 cubre típica burst WhatsApp. Sobre eso → drop oldest + firehose alert |
| Cross-tenant leak en queue | Queue keyed por `ProcessingScope` (incluye `agent_id` que ya es tenant-scoped). Verificado en step 6 test `push_to_one_scope_isolated` |
| `mcp_channel_source` en scope rompe HashMap key | `ProcessingScope` ya derive Hash + Eq + PartialEq (verificado en tool-meta tests). DashMap funciona |

---

## Done de toda la fase 82.13.b

- [x] 8 commits atómicos.
- [x] `cargo build --workspace` + `cargo test --workspace` clean.
- [x] `mdbook build docs` clean.
- [x] FOLLOWUPS.md marca b.1/b.2/b.3 ✅.
- [x] admin-ui/PHASES.md tiene los 4 checkboxes nuevos.
- [x] Smoke manual end-to-end: pause + cliente envía 2 msgs +
      operator interviene con summary al release + agent
      reanuda viendo todo.
- [x] Memory rule honored — research/ + claude-code-leak/ mining
      cited / absencia declarada en cada doc.

---

## Listo para `/forge ejecutar 82.13.b`
