# Spec — `agent-creator` v1 (SaaS meta-microapp)

**Status:** spec (post `/forge brainstorm agent-creator-v1`).
Brainstorm: `proyecto/brainstorm-agent-creator-v1.md`.

## 1. Mining (regla irrompible)

### research/

- `research/src/channels/conversation-binding-context.ts:1` — confirma
  binding shape canónico TS. Equivalente Rust:
  `crates/core/src/agent/binding.rs::BindingContext` (Phase 82.1, listo).
- `research/extensions/whatsapp/src/channel.ts:65` — bloque pairing
  store en TS. Confirma decisión de exponer pairing por admin RPC, no
  por YAML. Análogo Rust: `nexo/admin/pairing/*` (82.10 step 5, listo).
- `research/src/agents/skills.ts:1-50` — skills load + frontmatter en
  TS. Análogo Rust: `crates/core/src/agent/skills.rs` ya implementa
  `SkillLoader::new(root)` con frontmatter, deps mode (strict/warn/disable),
  per-agent override. **CRUD admin RPC NO existe — gap 1.**

### claude-code-leak/

**Ausente.** `ls /home/familia/chat/claude-code-leak/` retorna
"No such file or directory". Cumplido el reporte por la regla
`feedback_brainstorm_must_mine_research_and_leak.md`.

## 2. Boundary recap (gates antes del detail)

- **Microapp drives config** (memory `feedback_framework_agnostic_microapp_drives.md`):
  toda config (agentes / skills / LLM keys / pairing) pasa por admin RPC
  desde la microapp. NO YAML manual.
- **Framework agnostic** (misma memory): cualquier addition framework-side
  debe pasar el test "¿lo usaría OTRA microapp?". Sí en todos los gaps.
- **Multi-tenant defense-in-depth** (memory `project_microapp_is_saas_meta_creator.md`):
  todo keyed por `account_id`; cross-tenant lookup retorna empty/Unknown,
  no error con info que filtre.

## 3. Framework changes (agnostic, on-demand)

### 3.1 — `nexo/admin/skills/*` CRUD

**Crate:** `crates/tool-meta/src/admin/skills.rs` (nuevo módulo, registrar en `admin/mod.rs`).

**Wire shapes (tool-meta):**

```rust
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRecord {
    pub name: String,            // dir name, kebab-case, [a-z0-9-]+
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub body: String,            // markdown body sans frontmatter
    pub max_chars: Option<usize>,
    pub requires: SkillRequiresRecord,
    pub updated_at: DateTime<Utc>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRequiresRecord {
    pub bins: Vec<String>,
    pub env: Vec<String>,
    pub mode: SkillDepsMode,     // re-export from nexo-config
}

pub struct SkillsListParams { /* opcional filter prefix */ }
pub struct SkillsListResponse { pub skills: Vec<SkillSummary> }
pub struct SkillSummary { pub name: String, pub description: Option<String>, pub updated_at: DateTime<Utc> }

pub struct SkillsGetParams { pub name: String }
pub struct SkillsGetResponse { pub skill: SkillRecord }

pub struct SkillsUpsertParams {
    pub name: String,            // create-or-update; same dir
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub body: String,
    pub max_chars: Option<usize>,
    pub requires: Option<SkillRequiresRecord>,
}
pub struct SkillsUpsertResponse { pub skill: SkillRecord, pub created: bool }

pub struct SkillsDeleteParams { pub name: String }
pub struct SkillsDeleteAck { pub deleted: bool }
```

**RPC methods:**
- `nexo/admin/skills/list`
- `nexo/admin/skills/get`
- `nexo/admin/skills/upsert`
- `nexo/admin/skills/delete`

**Handler crate:** `crates/core/src/agent/admin_rpc/domains/skills.rs`
(nuevo). Trait `SkillsStore`:

```rust
#[async_trait]
pub trait SkillsStore: Send + Sync + std::fmt::Debug {
    async fn list(&self, prefix: Option<&str>) -> anyhow::Result<Vec<SkillSummary>>;
    async fn get(&self, name: &str) -> anyhow::Result<Option<SkillRecord>>;
    async fn upsert(&self, params: SkillsUpsertParams) -> anyhow::Result<(SkillRecord, bool)>;
    async fn delete(&self, name: &str) -> anyhow::Result<bool>;
}
```

**Production adapter:** `crates/setup/src/admin_adapters.rs::FsSkillsStore`
— escribe `<root>/<name>/SKILL.md` reusando frontmatter format que
`SkillLoader` ya parsea. Concurrency: `parking_lot::Mutex` por nombre.

**Capability gate:** nuevo `manage_skills` en
`crates/core/src/agent/admin_rpc/capabilities.rs` + entry en
`crates/setup/src/capabilities.rs::INVENTORY` (regla CLAUDE.md #5).

**Validation:**
- Name `[a-z0-9][a-z0-9-]{0,63}` (kebab, no path traversal).
- Body min 1 char tras trim, max 64 KiB (defense-in-depth).
- Frontmatter compuesto desde `display_name` + `description` +
  `max_chars` + `requires`; body se inyecta debajo de `---`.
- Atomic write: `tokio::fs::write` a `SKILL.md.tmp` + `rename`.
- Defense-in-depth: canonicalize path + `starts_with(skills_root)`.

**Hot-reload:** `upsert/delete` emiten un evento
`SkillStoreChanged { name, action }` por el bus interno; agentes
recargan via `SkillLoader` en su próximo turn (sin reiniciar).

### 3.2 — Cerrar `InterventionAction::Reply` end-to-end

**Estado actual:** wire shape OK
(`crates/tool-meta/src/admin/processing.rs:108-128`); `is_v0_supported`
retorna true para `Reply`. Falta handler que ejecute el outbound.

**Cambio:** `crates/core/src/agent/admin_rpc/domains/processing.rs` ya
tiene `pause/resume/state`; añadir `intervene` que:

1. Verifica scope `Conversation { agent_id, channel, account_id, contact_id }`
   está paused.
2. Para `Reply`, llama `OutboundDispatcher::send` (Phase 82.3) con
   `{channel, account_id, to, body, msg_kind, attachments, reply_to_msg_id}`.
3. Reencauza errors: outbound failure → `-32603 internal`; canal no
   conectado → `-32004 channel_unavailable` (nuevo error code).
4. Por sí solo no resume — el operador decide si llamar `resume`.

**Wire (nuevo):**
```rust
pub struct ProcessingIntervenParams {
    pub scope: ProcessingScope,
    pub action: InterventionAction,
    pub reason: Option<String>, // opcional para audit log
}
pub struct ProcessingIntervenAck {
    pub dispatched: bool,
    pub outbound_message_id: Option<String>,
}
```

**RPC method:** `nexo/admin/processing/intervene`. Capability:
`manage_processing` (ya existe).

**Audit:** cada intervene se registra en SQLite audit (Phase 82.10.h)
con `actor_token_hash`, `scope`, `action_kind`, `outbound_message_id`.

### 3.3 — SDK helper `HumanTakeover`

**Crate:** `crates/microapp-sdk/src/admin.rs` (extender; ya existe el module).

```rust
pub struct HumanTakeover<'a> {
    admin: &'a AdminRpcClient,
    scope: ProcessingScope,
    audit_reason: Option<String>,
}

impl<'a> HumanTakeover<'a> {
    /// Pausar el scope (idempotente).
    pub async fn engage(
        admin: &'a AdminRpcClient,
        scope: ProcessingScope,
        reason: Option<String>,
    ) -> Result<Self, AdminRpcError> { ... }

    /// Enviar mensaje manual sin reanudar.
    pub async fn send_reply(&self, body: SendReplyArgs)
        -> Result<ProcessingIntervenAck, AdminRpcError> { ... }

    /// Reanudar IA (opcionalmente con summary inyectado al contexto).
    pub async fn release(self, summary_for_agent: Option<String>)
        -> Result<(), AdminRpcError> { ... }
}

pub struct SendReplyArgs {
    pub body: String,
    pub msg_kind: String, // "text" / "template" / "media"
    pub attachments: Vec<serde_json::Value>,
    pub reply_to_msg_id: Option<String>,
}
```

`summary_for_agent` se inyecta como un mensaje `system` sintético al
hilo (ChannelInbound + flag `synthetic_takeover_summary: true`) antes
del resume — mecanismo existente de Phase 82.5 inbound metadata.

**Tests SDK:** mock dispatcher confirma orden: pause → intervene → resume.

### 3.4 — SDK helper `TranscriptStream::filter_by_agent`

**Crate:** `crates/microapp-sdk/src/admin/transcripts.rs` (extender).

```rust
impl TranscriptStream {
    /// Wrap firehose stream; drops events cuyo agent_id no esté
    /// en el set permitido. Defense-in-depth multi-tenant.
    pub fn filter_by_agent(self, allowed: HashSet<String>) -> Self { ... }

    /// Convenience: filter from a slice.
    pub fn filter_by_agent_slice(self, allowed: &[String]) -> Self {
        self.filter_by_agent(allowed.iter().cloned().collect())
    }
}
```

Implementación: combinator sobre el `Stream<Item = TranscriptEvent>`
existente (Phase 82.11). Zero alloc por evento — `Arc<HashSet<String>>`
clonable.

### 3.5 — `EscalationReason::UnknownQuery`

**Crate:** `crates/tool-meta/src/admin/escalations.rs:28`.

Añadir variant `UnknownQuery` al enum (NO `#[non_exhaustive]` actual,
así que esto es semver minor — todos los downstream son in-tree, no
se publica todavía). Documentar:

> `UnknownQuery` — agent could not answer because the user request
> falls outside the loaded skills/knowledge. Use this when surfacing
> "agent doesn't know" UI notifications.

### 3.6 — Resumen gaps framework

| Gap | Crate(s) modificados | Capability nueva | Wire breaking? |
|-----|---------------------|------------------|----------------|
| 3.1 skills CRUD | tool-meta + core/admin_rpc + setup | `manage_skills` | No (additive) |
| 3.2 intervene reply | core/admin_rpc + tool-meta | reuse `manage_processing` | No (new method) |
| 3.3 HumanTakeover SDK | microapp-sdk | — | No (new helper) |
| 3.4 transcript filter SDK | microapp-sdk | — | No (combinator) |
| 3.5 UnknownQuery | tool-meta | — | Minor (variant add) |

## 4. Microapp expansion (`agent-creator-microapp/`)

### 4.1 — Tools v1 (12 tools)

Cada tool registra metadata via `nexo-tool-meta` y delega a
`nexo-microapp-sdk` admin client.

| Tool name | Admin RPC backing | Compliance hooks que puede tocar |
|-----------|---|---|
| `agent_create` | `nexo/admin/agents/upsert` | — |
| `agent_list` | `nexo/admin/agents/list` (filtra al `account_id` del cliente) | — |
| `agent_update` | `nexo/admin/agents/upsert` | — |
| `agent_delete` | `nexo/admin/agents/delete` | — |
| `agent_set_compliance` | `nexo/admin/agents/upsert` (extensions_config.compliance) | toggles anti_loop / opt_out / pii / rate_limit |
| `whatsapp_pair_start` | `nexo/admin/pairing/start` | — |
| `whatsapp_pair_status` | `nexo/admin/pairing/status` | — |
| `llm_key_upsert` | `nexo/admin/llm_providers/upsert` | — |
| `llm_key_list` | `nexo/admin/llm_providers/list` | — |
| `llm_key_delete` | `nexo/admin/llm_providers/delete` | — |
| `skill_upsert` | `nexo/admin/skills/upsert` (gap 3.1) | — |
| `skill_list` | `nexo/admin/skills/list` (gap 3.1) | — |
| `skill_delete` | `nexo/admin/skills/delete` (gap 3.1) | — |
| `skill_attach` | `nexo/admin/agents/upsert` (`agent.skills += [name]`) | — |
| `conversation_list` | `nexo/admin/transcripts/list_contacts` (filter_by_agent helper) | — |
| `conversation_get` | `nexo/admin/transcripts/read_session` | — |
| `takeover_engage` | `HumanTakeover::engage` (gap 3.3) | — |
| `takeover_send` | `HumanTakeover::send_reply` (gap 3.3) | — |
| `takeover_release` | `HumanTakeover::release` (gap 3.3) | — |

### 4.2 — Auth model (cliente vs operador)

- **2 tokens HTTP**, ambos resueltos por Phase 82.12 HTTP server:
  - `OPERATOR_TOKEN` — full access, capabilities incluyen
    `manage_agents`, `manage_credentials`, `manage_skills`,
    `manage_processing`, `manage_transcripts`, `manage_pairing`,
    `manage_llm_providers`, `manage_channels`.
  - `CLIENT_TOKEN` — same set EXCEPT no `manage_credentials` extra
    sensible (revocar API keys). Cliente puede `upsert` su propia key
    pero no listar las del operador.
- v1 single-tenant-per-daemon: `account_id` se infiere del token (1
  daemon = 1 empresa, así que el token de cliente equivale al tenant
  del daemon entero). No hay sub-tenants en el daemon.
- v1 NO tiene `OPERATOR` distinto del root — Cristian opera el daemon
  por SSH/proxy. UI de operador es opcional v2.

### 4.3 — Compliance toggle wiring (per agent)

Layout en `agents.yaml` (escrito por admin RPC, NO manual):

```yaml
agents:
  - id: agent-acme-001
    extensions_config:
      compliance:
        anti_loop: { enabled: true, max_repeat: 3, window_secs: 60 }
        opt_out:   { enabled: true }
        pii:       { enabled: true, redact: ["phone", "email"] }
        rate_limit:{ enabled: true, per_min: 5 }
```

Microapp `before_message` hook (registrado al boot vía SDK) lee
`extensions_config.compliance` del binding context, y delega a
`compliance-primitives` (Phase 83.6, listo) cuando el toggle es true.

Hot-reload: `agent_set_compliance` → `nexo/admin/agents/upsert` →
core re-emite `AgentChanged`; hook se relee al siguiente turn.

### 4.4 — Boot flow

1. Microapp arranca via stdio (`Microapp::run_stdio`).
2. Registra `before_message` hook (compliance dispatcher).
3. Suscribe a `nexo/admin/transcripts/stream` filtrado por agente.
4. Expone tools listadas en §4.1.

## 5. Casos de uso end-to-end

### 5.1 — Cliente crea su primer agente

```
[UI React] → POST /tools/agent_create { id, llm_profile, skills: [] }
  → microapp tool handler
    → admin client.call("nexo/admin/agents/upsert", { ... })
      → daemon: agent registered, agents.yaml updated
    → response: { agent_id }
```

### 5.2 — Cliente añade conocimiento (skill)

```
[UI React] → POST /tools/skill_upsert { name, body, description }
  → microapp tool handler
    → admin client.call("nexo/admin/skills/upsert", { ... })
      → daemon FsSkillsStore writes <root>/<name>/SKILL.md
      → SkillStoreChanged event broadcast
[UI React] → POST /tools/skill_attach { agent_id, skill_name }
  → admin.agents.upsert (agent.skills += [name])
  → next agent turn, SkillLoader reloads
```

### 5.3 — Pairing WhatsApp

```
[UI React] → POST /tools/whatsapp_pair_start { agent_id, phone_number }
  → admin.pairing.start → daemon emite QR via nexo/notify/pairing_qr
[UI React] suscribe firehose, recibe QR, lo renderiza
[Cliente] escanea QR en su WhatsApp
[UI React] poll GET /tools/whatsapp_pair_status hasta paired:true
```

### 5.4 — Takeover humano

```
[UI React] (panel conversación) → POST /tools/takeover_engage
  { scope: { kind: "conversation", agent_id, channel, account_id, contact_id }, reason: "human" }
  → HumanTakeover::engage → admin.processing.pause
[UI React] → POST /tools/takeover_send { body: "Hola, soy operador..." }
  → HumanTakeover::send_reply → admin.processing.intervene { Reply { ... } }
  → daemon dispatch outbound a WhatsApp
[UI React] → POST /tools/takeover_release { summary_for_agent: "..." }
  → HumanTakeover::release → admin.processing.resume + synthetic system msg
```

### 5.5 — Agente "no sabe"

```
[Agent turn] LLM detecta unknown query
  → emite tool call escalate { reason: "unknown_query", urgency: "low" }
    → admin.escalations.raise { reason: UnknownQuery, ... }
      → escalation broadcast
[UI React] subscribe firehose escalations, badge "1 nueva"
[Cliente] decide si toma takeover o ignora
```

## 6. Decisiones (closed-form)

| # | Decisión | Razón |
|---|---|---|
| D1 | 1 daemon = 1 empresa | Simplicidad multi-tenant; aislamiento de proceso. |
| D2 | API keys LLM las pone el cliente, NO el operador | Cliente paga su consumo directo al provider; operador factura suscripción. |
| D3 | Skills CRUD en framework, NO en microapp | Test "¿otra microapp lo reusa?" → sí (CRM, soporte). |
| D4 | `HumanTakeover` en SDK, NO en microapp | Patrón transversal. SDK lo empaqueta. |
| D5 | UI React fuera del repo Rust | Decoupling; React no depende de cargo build. |
| D6 | v1 manual onboarding (no self-service register) | Validar mercado antes de invertir en signup flow. |
| D7 | v1 single-tenant-per-daemon | Multi-empresa-en-un-daemon = Phase 2. |
| D8 | EscalationReason::UnknownQuery semver minor | Todos los consumers in-tree, no publicado. |
| D9 | Skills storage = filesystem (no SQLite) | SkillLoader ya lee filesystem; reuse, no duplicate storage. |
| D10 | Hot-reload skills via event bus | Evita restart del agente al CRUD una skill. |

## 7. Riesgos

- **R1 — Transcripts firehose lag:** UI debe ver mensajes en <1s. Si
  daemon está saturado podría haber lag. Mitigación: backpressure
  Phase 82.11 + UI muestra spinner si lag >2s.
- **R2 — Skill body XSS si UI lo renderiza:** UI debe escapar markdown
  o usar sandboxed renderer. NO es problema del framework.
- **R3 — Pairing QR roba token:** QR válido solo segundos; pairing
  store auto-expira (Phase 82.10 step 5).
- **R4 — Cliente borra agente con conversaciones activas:** orphan
  transcripts. Mitigación: `agent_delete` con flag `purge_transcripts:
  bool`, default false; transcripts quedan accesibles read-only.
- **R5 — Compliance toggle off por error filtra PII:** UI muestra
  warning "PII redaction disabled" en banner persistente.

## 8. Out of scope v1

- Multi-empresa-en-un-daemon (Phase 2).
- Self-service registration (Phase 2).
- Pricing / billing integration (Phase 2).
- WhatsApp number sharing entre agentes (Phase 2).
- Audit log UI para cliente (operador vía CLI por ahora).
- React frontend implementation (Phase 83.13 separate).
- Telegram / email channels (regla existente: scope WhatsApp-only v1).
- Vector search / RAG sobre skills (Phase futura).

## 9. Done criteria

- ✅ Gap 3.1: `nexo/admin/skills/*` CRUD + tests + capability + INVENTORY + docs.
- ✅ Gap 3.2: `processing.intervene` ejecuta `Reply` outbound + audit
  + integration test hits real channel mock.
- ✅ Gap 3.3: SDK `HumanTakeover` con engage/send_reply/release tests.
- ✅ Gap 3.4: SDK `TranscriptStream::filter_by_agent` con tests
  defense-in-depth.
- ✅ Gap 3.5: `EscalationReason::UnknownQuery` variant + docs.
- ✅ Microapp tools v1 (§4.1) implementados via SDK admin client.
- ✅ Compliance toggle hot-reload end-to-end.
- ✅ `cargo build --workspace` clean + `cargo test --workspace` passing.
- ✅ Microapp arranca standalone, registra hooks, lista tools en
  `plugin.toml`.
- ✅ Docs: `docs/src/microapps/agent-creator.md` + admin-ui sync per
  CLAUDE.md regla #4 #5 #6.

---

**Listo para `/forge plan agent-creator-v1`.**
