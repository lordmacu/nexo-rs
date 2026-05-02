# Plan — `agent-creator` v1 (SaaS meta-microapp)

**Status:** plan (post `/forge spec agent-creator-v1`).
Spec: `proyecto/spec-agent-creator-v1.md`. Brainstorm:
`proyecto/brainstorm-agent-creator-v1.md`.

## Metodología — microapp = stress test del framework

Esta microapp existe para **retar al framework** y exponer gaps, bugs,
fricción API, oportunidades de mejora. Los 5 gaps documentados
(§3.1–3.5 de la spec) son los identificados ex-ante; nuevos gaps
aparecerán durante ejecución.

**Regla en cada step de `/forge ejecutar`:**

1. Si aparece fricción (API ausente, wire incompleto, helper pobre,
   bug runtime, dx torpe) → **pausar** el step, **fix framework
   agnostic** (que sirva a OTRA microapp), **retomar** consumiendo lo
   nuevo. NO hackear workaround microapp-side.
2. Cada step termina con un mini-review: ¿hubo algo torpe que rodeé?
   Si sí, abrir sub-fase `.b` o commit framework inline antes de cerrar.
3. Si el gap requiere >2 commits → partir en `.b` con plan propio.
   Si ≤1 commit → inline en el step actual.
4. FOLLOWUPS.md absorbe los gaps que se difieren conscientemente
   (regla `feedback_log_followups_for_deferreds.md`).

Métrica de éxito v1 ≠ "tools v1 funcionan". Es "tools v1 funcionan
**Y** framework salió más robusto/transversal/dx-friendly".

Cross-ref memory: `feedback_microapp_as_framework_stress_test.md`.

## Mining (regla irrompible)

- `research/src/agents/skills.ts:1-50` — orden real OpenClaw: skills
  cargan vía SkillLoader equivalente, CRUD no existe en TS (lo cortaron).
  Implica que la API CRUD es greenfield Rust — no hay shape-of-truth
  TS que copiar; se diseña desde la spec.
- `research/src/channels/conversation-binding-context.ts:1` — orden de
  pause→intervene→resume en TS pasa por el binding ID; replica del
  Rust ya validada por Phase 82.13.
- `claude-code-leak/` — **ausente** (`ls` retorna "No such file"). Doc
  cumplido por la regla.

## Numbering

Sub-fases dentro de la rama Phase 83 (microapp framework foundation).
Reusa el slot `83.8` que estaba destinado a ventas-etb (descartado).
Orden lógico framework → SDK → microapp → docs.

```
83.8.1  Framework gap — admin/skills CRUD wire shapes (tool-meta)
83.8.2  Framework gap — admin/skills domain handler + capability + INVENTORY
83.8.3  Framework gap — FsSkillsStore production adapter + atomic write
83.8.4  Framework gap — processing.intervene end-to-end (handler + outbound)
83.8.5  Framework gap — EscalationReason::UnknownQuery variant
83.8.6  SDK helper — HumanTakeover (engage/send_reply/release)
83.8.7  SDK helper — TranscriptStream::filter_by_agent
83.8.8  Microapp — extend agent-creator-microapp tools v1 (CRUD agents/skills/llm/channels)
83.8.9  Microapp — pairing + transcripts + takeover + escalations tools
83.8.10 Microapp — compliance hook dispatcher (toggle-driven)
83.8.11 Docs + admin-ui sync + close-out
```

11 atomic steps. Cada uno = un commit (a veces 2 si tests son grandes).

---

## 83.8.1 — admin/skills CRUD wire shapes

**Files NEW:**

- `crates/tool-meta/src/admin/skills.rs`

**Files MODIFIED:**

- `crates/tool-meta/src/admin/mod.rs` — `pub mod skills;`

**Shapes:**

```rust
SkillRecord, SkillSummary, SkillRequiresRecord
SkillsListParams/Response
SkillsGetParams/Response
SkillsUpsertParams/Response
SkillsDeleteParams/SkillsDeleteAck
```

Todos `#[non_exhaustive]` cuando aplica + `Serialize+Deserialize+Debug+Clone`.

**Tests:**

- Round-trip serde de cada wire shape.
- `SkillRecord` serializa keys en kebab-case (`updated_at`, no
  `updatedAt`).
- Frontmatter compose: `SkillRecord` → markdown body string puede
  re-parsear vía `SkillLoader::parse_frontmatter`.

**Done:**

- `cargo build -p nexo-tool-meta` clean.
- `cargo test -p nexo-tool-meta admin::skills` passing.
- Doc comments en cada tipo (regla `missing-docs deny` aplica al crate
  publicable Tier A).

---

## 83.8.2 — admin/skills domain handler + capability + INVENTORY

**Files NEW:**

- `crates/core/src/agent/admin_rpc/domains/skills.rs` — trait
  `SkillsStore` + handlers `list/get/upsert/delete`.

**Files MODIFIED:**

- `crates/core/src/agent/admin_rpc/domains/mod.rs` — `pub mod skills;`.
- `crates/core/src/agent/admin_rpc/dispatcher.rs` — agregar arms
  `nexo/admin/skills/list|get|upsert|delete` con capability gate.
- `crates/core/src/agent/admin_rpc/capabilities.rs` — añadir
  constante `pub const MANAGE_SKILLS: &str = "manage_skills";` (si no
  existe el patrón). En cualquier caso, propagar el string.
- `crates/setup/src/capabilities.rs::INVENTORY` — entry
  `manage_skills` con descripción + dangerous: false.

**Trait:**

```rust
#[async_trait]
pub trait SkillsStore: Send + Sync + std::fmt::Debug {
    async fn list(&self, prefix: Option<&str>) -> anyhow::Result<Vec<SkillSummary>>;
    async fn get(&self, name: &str) -> anyhow::Result<Option<SkillRecord>>;
    async fn upsert(&self, params: SkillsUpsertParams) -> anyhow::Result<(SkillRecord, bool)>;
    async fn delete(&self, name: &str) -> anyhow::Result<bool>;
}
```

**Validation in handlers:**

- Name regex `^[a-z0-9][a-z0-9-]{0,63}$`.
- Body 1..=65536 chars after trim.
- `list` retorna empty Vec si store está vacío (no error).
- `get` retorna `null` skill cuando no existe (no error).

**Tests:**

- Mock `InMemorySkillsStore` + dispatch per method.
- Capability gate: sin `manage_skills` → `-32004`.
- Invalid name → `-32602 invalid_params`.
- Body too large → `-32602`.

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-core admin_rpc::domains::skills` passing.

---

## 83.8.3 — FsSkillsStore production adapter

**Files MODIFIED:**

- `crates/setup/src/admin_adapters.rs` — añadir struct
  `FsSkillsStore { root: PathBuf, locks: parking_lot::Mutex<HashMap<String, Arc<AsyncMutex<()>>>> }`.
- `crates/setup/src/admin_bootstrap.rs` — wire `FsSkillsStore` en
  el dispatcher con `skills_root` resuelto desde config.

**Behavior:**

- `upsert`: lock por name → write `<root>/<name>/SKILL.md.tmp` →
  rename atómico a `SKILL.md`. Compose frontmatter desde `display_name
  + description + max_chars + requires`.
- `get`: read+parse via `SkillLoader::parse_frontmatter` — reuse,
  no re-implementar.
- `delete`: remove entire `<root>/<name>/` dir (recursive, since
  alguien puede haber añadido subfiles — pero v1 solo tenemos
  `SKILL.md`; defense-in-depth canonicalize + `starts_with(root)`).
- `list`: read_dir filter `is_dir` ∧ contains `SKILL.md`.

**Defense-in-depth:**

- Canonicalize input name → resolve final path → assert
  `starts_with(canonical_root)`. Reject path traversal.
- Reject names equal to `.`, `..`, with `/`, `\`, `:`.

**Hot-reload event:**

- Emit `SkillStoreChanged { name: String, action: SkillStoreAction }`
  via `crates/core/src/agent/event_bus.rs` — reuse local bus.
  Variant `SkillStoreAction::{Upserted, Deleted}`.
- Agentes en su next turn ven el evento y `SkillLoader` recarga.
  v1: simple — agentes que tengan ese skill en `agent.skills` lo
  recargan en el próximo prompt build (lazy). No restart.

**Tests:**

- Tempdir + upsert → file existe + frontmatter correcto.
- Upsert mismo name dos veces → `created: false` la segunda.
- Delete inexistente → `false`, no error.
- Path traversal `name = "../etc/passwd"` → `Err`.
- List vacío sobre dir vacío → empty Vec, no error.
- Concurrent upsert mismo name → ningún archivo corrupto (lock).

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-setup admin_adapters::fs_skills` passing.
- Integration test: spawn dispatcher con `FsSkillsStore` real,
  call `upsert` + `list` end-to-end via JSON-RPC stub.

---

## 83.8.4 — processing.intervene end-to-end

**Files MODIFIED:**

- `crates/tool-meta/src/admin/processing.rs` — añadir
  `ProcessingIntervenParams` + `ProcessingIntervenAck`.
- `crates/core/src/agent/admin_rpc/domains/processing.rs` — handler
  `pub async fn intervene(...)` que:
  1. Lookup state. Si no `PausedByOperator` → `-32004 not_paused`.
  2. Match action. Si `is_v0_supported() == false` → `-32601`.
  3. Para `Reply`: llama `OutboundDispatcher::send` (Phase 82.3).
  4. Audit log entry: action_kind, scope, outbound_message_id.
- `crates/core/src/agent/admin_rpc/dispatcher.rs` — arm
  `nexo/admin/processing/intervene` capability `manage_processing`.

**Outbound integration:**

- Inject `Arc<dyn OutboundDispatcher>` en el dispatcher constructor
  (ya está disponible en boot — Phase 82.3 adapter está en
  `setup/src/admin_bootstrap.rs`).

**Error codes:**

- `-32004 not_paused` — scope no está paused.
- `-32004 channel_unavailable` — outbound dispatcher dice channel
  desconectado.
- `-32601 not_implemented` — action variant ≠ Reply (slot reservado).
- `-32603 internal` — outbound transport error.

**Tests:**

- Mock OutboundDispatcher captura llamadas.
- Pause → intervene Reply → dispatcher recibe payload completo
  (channel, account_id, to, body, msg_kind, attachments,
  reply_to_msg_id).
- Intervene SkipItem → `-32601`.
- Intervene sin pause → `-32004 not_paused`.
- Audit log capturado con outbound_message_id correcto.

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-core admin_rpc::domains::processing::intervene` passing.

---

## 83.8.5 — EscalationReason::UnknownQuery variant

**Files MODIFIED:**

- `crates/tool-meta/src/admin/escalations.rs:28` — añadir variant
  `UnknownQuery`.
- `crates/tool-meta/src/admin/escalations.rs` doc — actualizar comment
  del enum mencionando UnknownQuery + cuando usarlo.
- Cualquier `match` exhaustivo sobre `EscalationReason` en
  `crates/core` + `crates/setup` — añadir arm. Esperado: pocos sites.

**Tests:**

- Round-trip serde — `"unknown_query"` ↔ `EscalationReason::UnknownQuery`.
- Existing escalation tests siguen pasando.

**Done:**

- `cargo build --workspace` clean.
- `cargo test --workspace` passing.

---

## 83.8.6 — SDK helper `HumanTakeover`

**Files NEW:**

- `crates/microapp-sdk/src/admin/takeover.rs` — struct
  `HumanTakeover` con engage/send_reply/release.

**Files MODIFIED:**

- `crates/microapp-sdk/src/admin/mod.rs` — `pub mod takeover; pub use takeover::HumanTakeover;`.
- `crates/microapp-sdk/Cargo.toml` — feature flag NO necesaria
  (depende solo de admin client + tool-meta wires).

**API:**

```rust
pub struct HumanTakeover<'a> {
    admin: &'a AdminRpcClient,
    scope: ProcessingScope,
    audit_reason: Option<String>,
}

pub struct SendReplyArgs {
    pub body: String,
    pub msg_kind: String,
    pub attachments: Vec<serde_json::Value>,
    pub reply_to_msg_id: Option<String>,
}

impl<'a> HumanTakeover<'a> {
    pub async fn engage(admin, scope, reason) -> Result<Self, AdminRpcError>;
    pub async fn send_reply(&self, args: SendReplyArgs) -> Result<ProcessingIntervenAck, AdminRpcError>;
    pub async fn release(self, summary_for_agent: Option<String>) -> Result<(), AdminRpcError>;
}
```

**Behavior:**

- `engage` → `admin.processing.pause(scope, reason)`. Idempotente.
- `send_reply` → `admin.processing.intervene(scope, Reply { ... })`.
  Construye payload desde `scope` + `args`.
- `release(summary)`:
  - Si `summary` is Some: emit synthetic system inbound via existing
    `nexo/admin/transcripts/inject_synthetic` if exists, ELSE
    fallback: skip injection v1 (anotar follow-up).
  - `admin.processing.resume(scope)`.

**Tests:**

- Mock AdminRpcClient captura calls in order.
- `engage → send_reply → release` produce 3 calls correctas.
- `release` sin summary no inyecta synthetic.
- Re-`engage` ejecuta pause idempotente OK (no error).

**Done:**

- `cargo build -p nexo-microapp-sdk` clean.
- `cargo test -p nexo-microapp-sdk admin::takeover` passing.

**Followup si transcripts inject_synthetic no existe:** anotar en
FOLLOWUPS.md "synthetic system message inject API" — gap framework
ortogonal, no bloquea v1 takeover básico.

---

## 83.8.7 — SDK helper `TranscriptStream::filter_by_agent`

**Files MODIFIED:**

- `crates/microapp-sdk/src/admin/transcripts.rs` — añadir métodos
  `filter_by_agent` + `filter_by_agent_slice` sobre el `Stream`
  existente.

**API:**

```rust
impl TranscriptStream {
    pub fn filter_by_agent(self, allowed: HashSet<String>) -> Self;
    pub fn filter_by_agent_slice(self, allowed: &[String]) -> Self;
}
```

**Implementation:**

- Combinator usando `futures::StreamExt::filter`. `Arc<HashSet<String>>`
  cloneable. Zero alloc por evento.

**Tests:**

- Stream con 3 eventos (agent_id ∈ {A, B, C}) + filter [A, C] →
  recibe solo A y C.
- Empty allowed set → drops everything.
- Defense-in-depth: agent_id missing en evento → drop (default deny).

**Done:**

- `cargo build -p nexo-microapp-sdk` clean.
- `cargo test -p nexo-microapp-sdk admin::transcripts::filter_by_agent` passing.

---

## 83.8.8 — Microapp tools v1, parte A (CRUD agents/skills/llm/channels)

**Repo:** `/home/familia/chat/agent-creator-microapp/` (out-of-tree).

**Files NEW:**

- `src/tools/agents.rs` — handlers `agent_create`, `agent_list`,
  `agent_update`, `agent_delete`.
- `src/tools/skills.rs` — `skill_upsert`, `skill_list`, `skill_delete`,
  `skill_attach`.
- `src/tools/llm.rs` — `llm_key_upsert`, `llm_key_list`, `llm_key_delete`.
- `src/tools/mod.rs` — re-exports.

**Files MODIFIED:**

- `src/main.rs` — registrar handlers en `Microapp::tools`.
- `plugin.toml` — `[capabilities.admin]` required:
  `["manage_agents", "manage_skills", "manage_llm_providers"]`.
  Optional: `["manage_credentials"]`.
- `Cargo.toml` — usar path-dep al SDK actualizado en este branch.

**Tool registration via tool-meta:**

```rust
ToolMeta {
    name: "agent_create",
    description: "Create or update a WhatsApp agent for this tenant.",
    json_schema: schemars::schema_for!(AgentCreateArgs),
    ...
}
```

**Multi-tenant:** cada handler lee `binding.account_id` del
`ToolCtx`, lo inyecta como filter cuando llama admin (defense-in-depth
— el cliente solo ve sus agentes).

**Tests:**

- Mock admin client; verifica que `agent_create` traduce args →
  `nexo/admin/agents/upsert` payload.
- `skill_attach` produce `agents/upsert` con `skills += [name]`,
  no replace.
- `account_id` filter aplicado en `agent_list`.

**Done:**

- `cargo build` clean en `/home/familia/chat/agent-creator-microapp/`.
- `cargo test` passing.
- `microapp register-tools` (stdio JSON-RPC test) lista 11 tools de
  parte A.

---

## 83.8.9 — Microapp tools v1, parte B (pairing + transcripts + takeover + escalations)

**Files NEW:**

- `src/tools/pairing.rs` — `whatsapp_pair_start`, `whatsapp_pair_status`.
- `src/tools/conversations.rs` — `conversation_list`, `conversation_get`.
- `src/tools/takeover.rs` — `takeover_engage`, `takeover_send`,
  `takeover_release`. Usa SDK `HumanTakeover`.
- `src/tools/escalations.rs` — `escalation_list`, `escalation_resolve`.

**Files MODIFIED:**

- `src/main.rs` — registrar nuevos handlers.
- `plugin.toml` — añadir capabilities `manage_pairing`,
  `manage_processing`, `manage_transcripts`, `manage_escalations`.

**Multi-tenant defense:**

- `conversation_list` usa `TranscriptStream::filter_by_agent_slice`
  con la lista de agentes del cliente.

**Tests:**

- Takeover end-to-end via mock admin: engage → send → release.
- Pairing start retorna pairing_id; status poll loop.
- Conversation list excluye agentes de otros account_ids.

**Done:**

- `cargo build` clean.
- `cargo test` passing.
- Tools totales: 19 (parte A 11 + parte B 8). Match spec §4.1.

---

## 83.8.10 — Compliance hook dispatcher (toggle-driven)

**Files NEW:**

- `src/hooks/compliance.rs` — single `before_message` handler que
  lee `binding.agent.extensions_config.compliance` y dispatch a
  `nexo-compliance-primitives` cuando toggle = true.

**Files MODIFIED:**

- `src/main.rs` — registrar `before_message: Some(compliance_hook)`.

**Behavior:**

```
on inbound message:
  cfg = binding.agent.extensions_config.compliance
  if cfg.anti_loop.enabled    → AntiLoopDetector::check
  if cfg.opt_out.enabled      → OptOutMatcher::check
  if cfg.pii.enabled          → PiiRedactor::redact (transforms body)
  if cfg.rate_limit.enabled   → RateLimitPerUser::check
  return HookOutcome { decision, transformed_body, ... }
```

Hot-reload: hook re-lee `extensions_config` en cada inbound (no cache),
así `agent_set_compliance` toma efecto en próximo mensaje.

**Tests:**

- Toggle anti_loop on + repeat 4× → `HookOutcome::Block`.
- Toggle pii on + body con teléfono → body redactado.
- All toggles off → `HookOutcome::Continue` siempre.

**Done:**

- `cargo build` clean.
- `cargo test hooks::compliance` passing.

---

## 83.8.11 — Docs + admin-ui sync + close-out

**Files NEW:**

- `docs/src/microapps/agent-creator.md` — overview, tools, auth,
  topology.
- `docs/src/admin/skills_admin_rpc.md` — gap 3.1 reference.

**Files MODIFIED:**

- `docs/src/admin/processing.md` — documentar `intervene` Reply
  end-to-end.
- `docs/src/sdk/microapp_sdk.md` — `HumanTakeover` + `filter_by_agent`.
- `admin-ui/PHASES.md` — checkbox para skills CRUD UI + takeover UI
  + compliance toggles.
- `proyecto/PHASES.md` — marcar 83.8.{1..11} ✅ atomically.
- `proyecto/CLAUDE.md` — actualizar tabla Phase 83 contador.
- `proyecto/FOLLOWUPS.md` — log:
  - synthetic message inject API (si no existe; gap 3.3 release)
  - multi-empresa-en-un-daemon (Phase 2)
  - self-service registration (Phase 2)
  - pricing/billing integration (Phase 2)
  - audit log UI for client (operator usa CLI por ahora)
- `proyecto/CHANGELOG.md` — entry para gaps 3.1/3.2/3.3/3.4/3.5
  + microapp tools v1.

**Done:**

- `mdbook build docs` clean.
- `cargo build --workspace` final clean.
- `cargo test --workspace` final passing.
- Commit final: "feat(83.8): agent-creator v1 SaaS meta-microapp shipped".

---

## Riesgos del plan

- **R1 — Gap 3.2 outbound dispatcher injection:** si el adapter actual
  no expone `OutboundDispatcher` al admin RPC dispatcher, requiere
  refactor de boot wiring. Mitigación: revisar
  `setup/src/admin_bootstrap.rs` antes de empezar 83.8.4. Si falta
  wiring, partir 83.8.4 en .a (refactor wiring) + .b (handler).
- **R2 — Gap 3.1 hot-reload event bus:** si no hay event bus para
  emitir `SkillStoreChanged`, fallback v1 = agentes recargan en
  próximo turn sin evento (lazy). Documentar como follow-up Phase 2
  proactive reload.
- **R3 — Microapp tool count grande (19):** scope creep. Mitigación:
  cortes A/B/C atómicos (83.8.8/83.8.9/83.8.10). Cada uno puede merger
  independiente con tests.
- **R4 — Cambios SDK rompen agent-creator-microapp existente:** path
  deps deberían absorber el cambio. Verificar `cargo build` del
  microapp después de cada cambio SDK (83.8.6 + 83.8.7).
- **R5 — Multi-tenant defense en `agent_list`:** si admin RPC
  `nexo/admin/agents/list` no soporta filter por account_id, requiere
  client-side filter (peor — list todo, dropear). Mitigación:
  inspeccionar handler antes de 83.8.8 step.

## Order de ejecución (commits atómicos)

```
83.8.1 → 83.8.2 → 83.8.3   (skills CRUD framework)
83.8.4                      (intervene end-to-end)
83.8.5                      (UnknownQuery)
83.8.6 → 83.8.7             (SDK helpers)
83.8.8 → 83.8.9 → 83.8.10   (microapp expansion)
83.8.11                     (docs + close-out)
```

11 commits. Sin interleaving. Cada paso debe `cargo build` + tests
antes del siguiente.

---

**Listo para `/forge ejecutar agent-creator-v1`.**
