# Spec — Phase 82.13.b: IA awareness during/after operator takeover

Cierra el gap detectado tras 82.13: agent reanuda *blind* tras
intervención humana. Tres sub-fases independientes, ship-ables
por separado, ordenadas por valor.

---

## Mining citado (memory rule)

- **research/** — `chat-transcript-inject.ts:44-116`
  (`appendInjectedAssistantMessageToTranscript()`) valida el
  patrón "operator inyecta como Assistant + metadata
  distintiva". Ver brainstorm para extracto completo.
  `replaySessionTranscript()` en `acp/translator.ts:1336-1352`
  garantiza que entries persistidas se entregan al LLM en el
  próximo turno sin código adicional.
- **claude-code-leak/** — repo no presente local. Ausencia
  declarada en brainstorm; mining no aplicable.

---

## Decisiones cerradas (del brainstorm)

| # | Decisión |
|---|----------|
| D1 | Orden: `82.13.b.1` (stamp-on-send) → `82.13.b.2` (summary) → `82.13.b.3` (queue). |
| D2 | `session_id` se pasa explícito en wire shape (no lookup). Microapp ya lo conoce. |
| D3 | Discriminador en transcript: `source_plugin = "intervention:<channel>"` + `sender_id = "operator:<token_hash>"`. NO nuevo `TranscriptRole::Operator`. |
| D4 | `pending_inbounds` queue in-memory + cap 50 + drop oldest + firehose drop event. SQLite bajo 82.13.c. |
| D5 | Para operator reply usar `role: Assistant`. Summary usa `role: System` con prefijo `[operator_summary] `. |
| D6 | Si `session_id` ausente en intervention (operator interviene desde notification deep-link sin sesión abierta): degradar gracefully — Reply al canal sí va, transcript NO se stampea, ack lleva flag `transcript_stamped: false`. |

---

## 82.13.b.1 — Stamp-on-send de operator replies

### Wire shape changes

`crates/tool-meta/src/admin/processing.rs`

```rust
pub struct ProcessingInterventionParams {
    pub scope: ProcessingScope,
    pub action: InterventionAction,
    pub operator_token_hash: String,
    /// Phase 82.13.b.1 — session in which to stamp the operator
    /// reply. When set + action is Reply, the daemon appends a
    /// `TranscriptEntry { role: Assistant, sender_id:
    /// "operator:<hash>", source_plugin: "intervention:<channel>"
    /// }` after the outbound dispatcher acks the send. When
    /// absent, the reply still goes out but the transcript is
    /// not modified — operator UI surfaces this via
    /// `ProcessingAck.transcript_stamped: false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
}

pub struct ProcessingAck {
    pub changed: bool,
    pub correlation_id: Uuid,
    /// Phase 82.13.b.1 — `Some(true)` when the daemon appended
    /// the operator reply to the agent transcript; `Some(false)`
    /// when the call provided no `session_id` or no
    /// transcript appender was wired; `None` for non-Reply
    /// interventions / pause / resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_stamped: Option<bool>,
}
```

### Trait surface

`crates/core/src/agent/admin_rpc/transcript_appender.rs` (new)

```rust
use async_trait::async_trait;
use uuid::Uuid;

/// Phase 82.13.b.1 — transcript-side hook for the
/// processing/intervention handler. Decoupled from
/// `TranscriptWriter` so the admin RPC layer doesn't depend on
/// the concrete writer type, and tests can use a recording
/// stub.
#[async_trait]
pub trait TranscriptAppender: Send + Sync + std::fmt::Debug {
    /// Stamp one entry under (agent_id, session_id). Returns
    /// `Ok(())` on success; daemon logs + returns `Err(...)`
    /// on persistence failure but the operator-facing ack
    /// degrades to `transcript_stamped: false` rather than
    /// failing the whole RPC (the channel send already
    /// succeeded by then).
    async fn append(
        &self,
        agent_id: &str,
        session_id: Uuid,
        entry: TranscriptEntry,
    ) -> anyhow::Result<()>;
}

/// Mirror of `nexo_core::agent::transcripts::TranscriptEntry`
/// but lives here so the admin RPC layer doesn't need a hard
/// dep on the writer module.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    pub content: String,
    pub source_plugin: String,
    pub sender_id: Option<String>,
    pub message_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy)]
pub enum TranscriptRole { User, Assistant, Tool, System }
```

### Production adapter

`crates/setup/src/admin_adapters.rs` extends with
`TranscriptWriterAppender` wrapping
`Arc<nexo_core::agent::transcripts::TranscriptWriter>`. Implements
`append()` by translating the local `TranscriptEntry` into the
core type and calling `writer.append_entry(session_id, entry)`.

### Dispatcher / handler wire-up

`crates/core/src/agent/admin_rpc/dispatcher.rs`:

- new field `transcript_appender: Option<Arc<dyn TranscriptAppender>>`
- new builder `with_transcript_appender(appender)`
- passed into `processing::intervention(store, outbound, appender, params)`

`crates/core/src/agent/admin_rpc/domains/processing.rs::intervention()`:

After `disp.send(msg).await?` returns `Ok(ack)` (existing path):

```rust
let transcript_stamped = match (&p.session_id, appender) {
    (Some(session_id), Some(app)) => {
        let entry = TranscriptEntry {
            role: TranscriptRole::Assistant,
            content: body.clone(),
            source_plugin: format!("intervention:{}", channel),
            sender_id: Some(format!("operator:{}",
                p.operator_token_hash)),
            message_id: ack.outbound_message_id
                .as_ref()
                .and_then(|s| Uuid::parse_str(s).ok()),
        };
        match app.append(p.scope.agent_id(), *session_id, entry).await {
            Ok(()) => Some(true),
            Err(e) => {
                tracing::warn!(error = %e, "transcript stamp failed");
                Some(false)
            }
        }
    }
    _ => Some(false),
};
```

ack carries `transcript_stamped` accordingly.

### Tests

- Round-trip `ProcessingInterventionParams` con `session_id`
  presente y ausente.
- Handler test: dispatcher con `RecordingTranscriptAppender`
  verifica que tras intervention exitosa hay 1 entry con
  `role: Assistant`, `source_plugin: "intervention:whatsapp"`,
  `sender_id: "operator:<hash>"`, `content` igual al body.
- Handler test: si `session_id` ausente → `outbound.send` se
  invoca igual + ack `transcript_stamped: Some(false)` + 0
  entries en appender.
- Handler test: appender devuelve `Err` → ack
  `transcript_stamped: Some(false)`, RPC sigue OK (degrada).
- Handler test: legacy params sin `session_id` deserializa
  correctamente (graceful absence).
- SDK `HumanTakeover::send_reply` accepts new optional
  `session_id` arg, threads through.

### Capability gate

Reusa `processing.intervene` (ya existe). No nuevo capability —
stamp es side effect del Reply, no operación independiente.

### Audit log

`admin_audit` ya logea `processing/intervention` con
`operator_token_hash`. Se agrega columna logical
`transcript_stamped` (en `payload` JSON) — sin migración SQLite,
es un campo del JSON existente.

### Done criteria

- `cargo build --workspace` clean.
- `cargo test --workspace` clean (existing tests + 5 new).
- `HumanTakeover::send_reply` SDK helper acepta + propaga
  `session_id`.
- `docs/src/microapps/takeover.md` actualizado: ejemplo del
  flujo completo y nota sobre el discriminator
  `intervention:<channel>` + `operator:<hash>`.
- FOLLOWUPS update: marcar 82.13.b.1 ✅, dejar .2/.3 abiertos.

---

## 82.13.b.2 — `summary_for_agent` end-to-end

### Wire shape

```rust
pub struct ProcessingResumeParams {
    pub scope: ProcessingScope,
    pub operator_token_hash: String,
    /// Phase 82.13.b.2 — session in which to inject the
    /// summary. When `summary_for_agent` is set this MUST be
    /// set too; daemon validates and returns
    /// `-32602 invalid_params` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    /// Phase 82.13.b.2 — operator-supplied free text that the
    /// agent sees as a `System` entry on its next turn.
    /// Prefixed with `[operator_summary] ` server-side so the
    /// agent can identify the source. Trimmed + redacted via
    /// the standard pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_for_agent: Option<String>,
}
```

### Handler

`processing::resume()`:

After existing state transition to `AgentActive`, if both
`session_id` and `summary_for_agent` are `Some` and the
appender is wired:

```rust
let entry = TranscriptEntry {
    role: TranscriptRole::System,
    content: format!("[operator_summary] {}", summary.trim()),
    source_plugin: "intervention:summary".into(),
    sender_id: Some(format!("operator:{}",
        p.operator_token_hash)),
    message_id: None,
};
appender.append(p.scope.agent_id(), session_id, entry).await
```

Validation:

- `summary` empty after trim → reject `-32602` "empty_summary".
- `summary.len() > 4096` → reject `-32602`
  "summary_too_long" (matches transcript indexer FTS doc cap).
- `session_id` ausente con `summary_for_agent` presente →
  reject `-32602` "session_id_required_with_summary".

### SDK

`HumanTakeover::release(summary_for_agent: Option<String>)` —
**ya existe** la firma. Cambio: dejar de descartar y forwardar
junto con `session_id` opcional (nuevo arg
`HumanTakeover::release_with_session(summary, session_id)`, o
agregar al constructor `HumanTakeover { ..., session_id }`).

### Tests

- Round-trip `ProcessingResumeParams` con summary + sin
  summary.
- Handler: summary presente + session_id presente → `System`
  entry stampeada con prefijo `[operator_summary] `.
- Handler: summary sin session_id → invalid_params.
- Handler: summary vacío / > 4096 chars → invalid_params.
- Handler: agent_id en scope no existe en agents.yaml →
  `transcript_stamped: false` + warn (defense-in-depth no
  bloquea resume).
- SDK round-trip via `release(Some("..."))`.

### Done

Idem .b.1 + actualización doc microapp con ejemplo summary.

---

## 82.13.b.3 — `pending_inbounds` queue durante pausa

### Store extension

`crates/core/src/agent/admin_rpc/processing_control.rs`
(donde vive `ProcessingControlStore`):

```rust
#[async_trait]
pub trait ProcessingControlStore: Send + Sync + std::fmt::Debug {
    // existing: get/set/clear...

    /// Phase 82.13.b.3 — push one inbound captured during
    /// pause. Returns the post-push depth. Implementations
    /// MUST cap the queue per scope (default 50) and return
    /// the dropped-oldest count via the second tuple element
    /// when the cap is exceeded; zero on the no-drop path.
    async fn push_pending(
        &self,
        scope: &ProcessingScope,
        inbound: PendingInbound,
    ) -> anyhow::Result<(usize, u32)>;

    /// Phase 82.13.b.3 — drain queue for resume. Returns the
    /// captured inbounds in arrival order; clears the queue.
    async fn drain_pending(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<Vec<PendingInbound>>;

    /// Phase 82.13.b.3 — depth without draining. For
    /// operator UI badge.
    async fn pending_depth(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<usize>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingInbound {
    pub message_id: Option<Uuid>,
    pub from_contact_id: String,
    pub body: String,
    pub timestamp_ms: u64,
    pub source_plugin: String,
}
```

`InMemoryProcessingControlStore` extends with
`pending: DashMap<ProcessingScope, VecDeque<PendingInbound>>`.
Cap defined as `const MAX_PENDING_PER_SCOPE: usize = 50;`.

### Inbound dispatcher hook

`crates/core/src/agent/dispatch_handlers.rs` (or wherever
the inbound path checks pause state):

The existing site that drops inbounds when the scope is
`PausedByOperator` instead does:

```rust
let pending = PendingInbound {
    message_id: inbound.message_id,
    from_contact_id: inbound.contact_id.clone(),
    body: inbound.body.clone(),
    timestamp_ms: now_ms(),
    source_plugin: inbound.source_plugin.clone(),
};
match store.push_pending(&scope, pending).await {
    Ok((_, dropped)) if dropped > 0 => {
        // Emit firehose drop event (best-effort).
        emitter.emit(AgentEventKind::PendingInboundsDropped {
            agent_id, scope, dropped,
        }).await;
    }
    Ok(_) => {}
    Err(e) => tracing::warn!("push_pending: {e}"),
}
return; // do not fire turn
```

### Resume drain

`processing::resume()` after state flip + (optional summary
inject from .b.2) + before returning ack:

```rust
let drained = store.drain_pending(&scope).await?;
if drained.is_empty() { return ack; }

if let (Some(session_id), Some(app)) =
    (params.session_id, appender) {
    // Stamp each as User entry with original timestamp.
    for it in &drained {
        let entry = TranscriptEntry {
            role: TranscriptRole::User,
            content: it.body.clone(),
            source_plugin: it.source_plugin.clone(),
            sender_id: Some(it.from_contact_id.clone()),
            message_id: it.message_id,
        };
        app.append(scope.agent_id(), session_id, entry).await.ok();
    }
}

// Fire ONE agent turn with the drained inbounds folded in.
agent_runtime.fire_turn(scope.agent_id(), session_id, drained).await?;
```

The single-turn dispatch is critical — N inbounds = N turns
would burn LLM budget + emit duplicate replies. Folding into
one turn lets the agent answer with one coherent reply.

### Wire shape

Optional but useful:

```rust
pub struct ProcessingAck {
    // existing fields...
    /// Phase 82.13.b.3 — when the call drained pending
    /// inbounds (resume only), how many. `None` for non-resume
    /// calls; `Some(0)` when no inbounds were buffered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drained_pending: Option<u32>,
}

pub enum AgentEventKind {
    // existing variants...
    /// Phase 82.13.b.3 — pending queue cap exceeded.
    PendingInboundsDropped {
        agent_id: String,
        scope: ProcessingScope,
        dropped: u32,
    },
}
```

### Tests

- Store: push beyond cap drops oldest + reports
  `dropped_count` correctly.
- Store: push to one scope doesn't affect another.
- Store: drain clears + returns FIFO order.
- Dispatcher: inbound during pause → push_pending + no turn fired.
- Resume: drain + single fire_turn + `drained_pending: Some(N)`
  in ack.
- Resume: drain into transcript stamps `role: User` with
  original timestamp_ms.
- Cap exceeded firehose drop event emitted.
- Multi-tenant: queue is keyed by full scope (including
  `agent_id`), no cross-tenant leak.

### Risks

- **Restart loses queue** — in-memory only. Operator UI shows
  `pending_depth` so operator can decide whether to pause for
  10 min vs 10 hr. SQLite under 82.13.c follow-up.
- **Token budget on big drains** — folding 50 inbounds into 1
  turn can blow context. Mitigation: cap at 50 (matches typical
  WhatsApp burst); over that, oldest dropped + firehose event
  surfaces it.
- **Order preservation** — store keeps FIFO (`VecDeque`); drain
  returns in arrival order; transcript timestamps preserve real
  chronology.

### Done

Idem .b.1 + load test (50 concurrent pushes + resume drain
under 100ms).

---

## Cross-cutting (todas las sub-fases)

### Capability inventory

`crates/setup/src/capabilities.rs::INVENTORY`:

- `processing.intervene` — already exists. No new entries.
- `processing.resume` — already exists. No new entries.
- New env toggle `NEXO_PROCESSING_PENDING_QUEUE_CAP` (default
  50) — for ops to tune. Add to INVENTORY entry as recommended
  by mandatory rule #5.

### Audit log

Entries already log per RPC. Payload JSON gets:
- `transcript_stamped: bool` (b.1)
- `summary_injected: bool` (b.2)
- `drained_pending: u32` (b.3)

No SQLite migration — `payload` is JSON column.

### Docs sync (mandatory rule #6)

`docs/src/microapps/takeover.md` rewrite covering:

- The full lifecycle: pause → operator intervenes (Reply) →
  optional release with summary → drain → agent reanuda
  coherente.
- Wire-shape examples for each new field.
- Discriminator pattern (`intervention:<channel>` +
  `operator:<hash>`) with rationale.
- Cap policy + drop event.

### admin-ui sync (mandatory rule #4)

`admin-ui/PHASES.md` adds checkboxes:

- Show transcript_stamped indicator on chat
- Show pending_depth badge on conversation list
- "Resume + summary" textbox in takeover drawer

### Compatibility

All wire shape additions are `Option<...>` with serde defaults.
Legacy microapps that don't know the fields keep working
identical to today (graceful absence).

---

## Open follow-ups (no se cierran en .b)

- **82.13.c** durable queue + persisted state (SQLite).
- Investigar collapse hint para drains gigantes (>50): compactar
  N msgs en 1 con LLM helper antes de fire_turn.
- `transcript_stamped: false` UI hint — operator UI mostrar
  warning cuando intentas summary sin session abierta.

---

## Listo para `/forge plan 82.13.b`
