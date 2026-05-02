# Spec — 83.8.12 multi-empresa framework primitive

**Status:** spec (post `/forge brainstorm 83.8.12`).
Brainstorm: `proyecto/brainstorm-83.8.12-multi-empresa.md`.

## 1. Mining (regla irrompible)

### research/

- `research/src/agents/cli-credentials.ts:292` — TS usa
  `account_id` como auth-profile key, NO como tenant. Confirma
  TS no modela "organization". Rust va más lejos.
- `research/AGENTS.md:1` — agentes globales sin nivel
  organization. **No copiar**.

### claude-code-leak/

**Ausente.** Cumplido.

### In-repo source-of-truth

- `crates/tool-meta/src/binding.rs:41` — BindingContext shape
  hoy.
- `crates/config/src/types/agents.rs:30` — AgentConfig.id +
  extensions_config.
- `crates/config/src/types/llm.rs:6` — `LlmConfig.providers:
  HashMap<String, LlmProviderConfig>`.
- `crates/setup/src/yaml_patch.rs:214,624,658,1950` — yaml
  patch surface (list/read/upsert agent + upsert_llm_provider_field).
- `crates/core/src/agent/admin_rpc/audit.rs::AdminAuditRow` —
  audit row tiene `account_id`, falta `empresa_id`.

## 2. Wire shapes (tool-meta)

**File NEW:** `crates/tool-meta/src/admin/empresas.rs`.

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresaSummary {
    pub id: String,
    pub display_name: String,
    pub active: bool,
    pub agent_count: usize,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmpresaDetail {
    pub id: String,
    pub display_name: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub llm_provider_refs: Vec<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EmpresasListFilter {
    pub active_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresasListResponse {
    pub empresas: Vec<EmpresaSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresasGetParams {
    pub empresa_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmpresasGetResponse {
    pub empresa: Option<EmpresaDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmpresasUpsertInput {
    pub id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_provider_refs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmpresasUpsertResponse {
    pub empresa: EmpresaDetail,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresasDeleteParams {
    pub empresa_id: String,
    /// `true` deletes empresa + every agent owned by it.
    /// `false` (default) only succeeds when no agents reference
    /// the empresa — UI confirms before passing `true`.
    #[serde(default)]
    pub purge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmpresasDeleteResponse {
    pub removed: bool,
    /// Agent ids that still reference this empresa. Populated
    /// when `purge: false` and the delete was rejected so UI can
    /// show "Acme has 3 agents — purge to confirm?".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orphaned_agents: Vec<String>,
}
```

**Validation rule (handler-side):**

- `id` regex: `^[a-z0-9][a-z0-9-]{0,63}$` (kebab, like skill names).
- `display_name` 1..=128 chars after trim.
- `active` defaults to `true` for new empresas.
- `llm_provider_refs` references must exist in `llm.yaml.providers.*`
  (validated when present).

## 3. BindingContext + AgentConfig

### 3.1 — `BindingContext.empresa_id`

**File:** `crates/tool-meta/src/binding.rs`.

```rust
pub struct BindingContext {
    // existing fields...
    /// Phase 83.8.12 — SaaS tenant key. `None` for legacy
    /// agents predating multi-empresa. Multi-tenant filtering
    /// keys on this field across admin RPC + microapp tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empresa_id: Option<String>,
}
```

`agent_only(id)` constructor leaves `empresa_id: None`. Builder
methods + `parse_binding_from_meta` honor the new field when
present in the producer-side `_meta.nexo.binding.empresa_id`.

### 3.2 — `AgentConfig.empresa_id`

**File:** `crates/config/src/types/agents.rs`.

```rust
pub struct AgentConfig {
    pub id: String,
    /// Phase 83.8.12 — empresa owner. `None` = global agent
    /// (operator-level, not tenant-owned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empresa_id: Option<String>,
    pub model: ModelConfig,
    // ...existing fields
}
```

YAML form:

```yaml
agents:
  - id: ventas-acme-001
    empresa_id: acme-corp     # Phase 83.8.12
    model: { provider: claude-acme, name: claude-opus-4-7 }
    plugins: [whatsapp]
    extensions_config: {}
```

### 3.3 — Producer wiring

`crates/core/src/agent/binding.rs::resolve_binding` (or
equivalent producer site) reads `agent_cfg.empresa_id` and
populates `BindingContext.empresa_id`. Same path everywhere
producer assembles BindingContext (whatsapp inbound, telegram
inbound, NATS event subscriber, delegation receive, heartbeat
bootstrap).

## 4. Empresas admin RPC domain

### 4.1 — `EmpresaStore` trait

**File NEW:** `crates/core/src/agent/admin_rpc/domains/empresas.rs`.

```rust
use async_trait::async_trait;
use nexo_tool_meta::admin::empresas::{
    EmpresaDetail, EmpresaSummary, EmpresasListFilter, EmpresasUpsertInput,
};

#[async_trait]
pub trait EmpresaStore: Send + Sync + std::fmt::Debug {
    async fn list(&self, filter: &EmpresasListFilter) -> anyhow::Result<Vec<EmpresaSummary>>;
    async fn get(&self, empresa_id: &str) -> anyhow::Result<Option<EmpresaDetail>>;
    async fn upsert(
        &self,
        params: EmpresasUpsertInput,
    ) -> anyhow::Result<(EmpresaDetail, bool)>;
    async fn delete(
        &self,
        empresa_id: &str,
        purge: bool,
    ) -> anyhow::Result<(bool, Vec<String>)>;
}
```

### 4.2 — Handlers

`pub async fn list / get / upsert / delete` in
`domains/empresas.rs`. Same shape as Phase 83.8.2 skills domain.
Validation:

- `validate_empresa_id` — kebab regex.
- `validate_display_name` — 1..=128 chars.
- `upsert` rejects unknown `llm_provider_refs` (cross-validate
  against `LlmYamlPatcher::list_provider_ids()` — new helper).

### 4.3 — Capability + INVENTORY

- New capability constant: `empresas_crud`.
- New INVENTORY entry: `NEXO_MICROAPP_ADMIN_EMPRESAS_ENABLED`.
- Dispatcher arms `nexo/admin/empresas/{list,get,upsert,delete}`
  → capability `empresas_crud`.

### 4.4 — Production adapter

**File MODIFIED:** `crates/setup/src/admin_adapters.rs`.

```rust
pub struct EmpresasYamlPatcher {
    path: PathBuf,
    // computed agent_count via cross-reference to agents.yaml
    agents_yaml: Arc<dyn YamlPatcher>,
}

impl EmpresaStore for EmpresasYamlPatcher {
    // reads/writes config/empresas.yaml atomically (tmp+rename)
    // computes agent_count by scanning agents.yaml.<id>.empresa_id
    // delete with purge=false returns orphaned_agents from agents.yaml scan
    // delete with purge=true cascades to AgentsYamlPatcher::delete each orphan
}
```

`config/empresas.yaml` shape:

```yaml
empresas:
  - id: acme-corp
    display_name: "Acme Corp."
    active: true
    created_at: 2026-05-02T10:00:00Z
    llm_provider_refs:
      - acme-claude-prod
      - acme-minimax-fallback
    metadata:
      contact: "support@acme.example"
      tier: pro
```

## 5. Per-empresa filter en admin RPC existentes

### 5.1 — `AgentsListFilter.empresa_id`

```rust
pub struct AgentsListFilter {
    pub active_only: bool,
    pub plugin_filter: Option<String>,
    /// Phase 83.8.12 — `Some(id)` → only that empresa's agents.
    /// `None` → all agents regardless of empresa.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empresa_id: Option<String>,
}
```

Handler `agents::list` filters via
`agents.yaml.<id>.empresa_id == Some(filter.empresa_id)`.

### 5.2 — `AgentEventsListFilter.empresa_id`

Same addition. Handler filters events by joining with
agents.yaml empresa lookup. Defense-in-depth: also embed
`empresa_id` in `TranscriptAppended` event so downstream
firehose subscribers can filter without re-querying.

### 5.3 — `EscalationsListParams.empresa_id`

Same. EscalationStore filters in-memory.

### 5.4 — Wire shape addition propagation

All three filter structs gain a new `empresa_id: Option<String>`
field with `serde(default)`. Existing callers with `..Default::default()`
unaffected. Adding the field is an additive serde change.

## 6. LLM providers per-empresa

### 6.1 — `llm.yaml` shape (additive)

```yaml
# Top-level providers (operator-managed, shared across empresas
# unless an empresa overrides).
providers:
  __operator-claude:
    kind: claude
    api_key_env: ANTHROPIC_OPERATOR_KEY
    ...

# Phase 83.8.12 — per-empresa providers.
empresas:
  acme-corp:
    providers:
      acme-claude-prod:
        kind: claude
        api_key_env: ACME_CLAUDE_KEY
        ...
      acme-minimax-fallback:
        kind: minimax
        api_key_env: ACME_MINIMAX_KEY
  globex:
    providers:
      globex-deepseek:
        kind: deepseek
        api_key_env: GLOBEX_DEEPSEEK_KEY
```

### 6.2 — `LlmConfig` extension

```rust
pub struct LlmConfig {
    pub providers: HashMap<String, LlmProviderConfig>, // operator-shared
    /// Phase 83.8.12 — per-empresa providers. Resolution
    /// order: empresa-scoped first, fallback to top-level
    /// `providers` if not found.
    #[serde(default)]
    pub empresas: HashMap<String, EmpresaLlmConfig>,
}

#[derive(...)]
pub struct EmpresaLlmConfig {
    pub providers: HashMap<String, LlmProviderConfig>,
}
```

### 6.3 — Resolution

When `BindingContext.empresa_id == Some(eid)` and an agent
references provider `X`:

1. Look up `llm.yaml.empresas.<eid>.providers.X`. Return if
   found.
2. Fall back to `llm.yaml.providers.X` (operator default).
3. If still missing → existing "provider not found" error.

### 6.4 — Admin RPC `nexo/admin/llm_providers/*` extension

- `LlmProvidersListResponse.providers` gains an
  `empresa_scope: Option<String>` field per row (which empresa
  owns this provider, or None for operator-shared).
- `LlmProviderUpsertInput` gains `empresa_id: Option<String>`
  — when `Some`, writes under `llm.yaml.empresas.<id>.providers.<name>`
  instead of top-level.

### 6.5 — Defense-in-depth

Cross-empresa lookup MUST fail. If client A asks for provider
defined under empresa B, handler returns `-32004 not_found`
(NOT "found but unauthorized" — don't leak existence).

## 7. Skills per-empresa

### 7.1 — Filesystem layout

```
config/skills/
  __global__/
    weather/SKILL.md         # operator-published, all empresas
    coverage/SKILL.md
  acme-corp/
    tarifario-2026/SKILL.md  # acme-only
  globex/
    crm-cheatsheet/SKILL.md  # globex-only
```

### 7.2 — `FsSkillsStore` extension

`FsSkillsStore::new(root)` keeps current signature.
`SkillsListParams.empresa_id: Option<String>`:

- `None` → list `__global__/` only (operator scope).
- `Some(eid)` → list `<eid>/` directory ONLY (no fall-through
  to global at this layer; agent-side resolution handles
  fallback).

`SkillLoader` (runtime, Phase 83.2) is updated:

- For `BindingContext.empresa_id = Some(eid)`: load skills from
  `<eid>/` first; for any name missing, fall back to
  `__global__/`.
- For `BindingContext.empresa_id = None`: load only from
  `__global__/`.

### 7.3 — Migration

Existing `skills/<name>/` flat layout (pre-83.8.12) treated as
`__global__/<name>/`. Migration helper at boot:

```rust
fn migrate_flat_skills_to_global(root: &Path) -> anyhow::Result<()> {
    // for each <root>/<name>/SKILL.md, if neither __global__
    // nor an empresa-id directory, move to __global__/<name>/.
    // Idempotent — second run is no-op.
}
```

### 7.4 — Admin RPC `nexo/admin/skills/*`

- `SkillsListParams.empresa_id` (new optional).
- `SkillsUpsertParams.empresa_id` (new optional, defaults to
  `__global__`).
- `SkillsDeleteParams.empresa_id` (new optional, defaults to
  `__global__`).

## 8. Audit log empresa_id column

### 8.1 — SQLite migration

`crates/core/src/agent/admin_rpc/audit_sqlite.rs` adds:

```sql
ALTER TABLE admin_audit ADD COLUMN empresa_id TEXT NULL;
CREATE INDEX idx_admin_audit_empresa ON admin_audit(empresa_id);
```

Migration is idempotent — `IF NOT EXISTS` style or schema-version
check.

### 8.2 — `AdminAuditRow.empresa_id`

```rust
pub struct AdminAuditRow {
    pub microapp_id: String,
    pub method: String,
    // existing fields...
    /// Phase 83.8.12 — empresa scope of this dispatch.
    /// Resolved from the request params (e.g.
    /// `agents.list.empresa_id`) OR from the
    /// `BindingContext.empresa_id` of the inbound that
    /// triggered the call. `None` for operator-level calls
    /// without empresa scope.
    pub empresa_id: Option<String>,
}
```

### 8.3 — `tail_for_empresa(empresa_id, ...)` helper

New tail query analog to `tail_for_account` (Phase 82.8). Uses
the new index. CLI subcommand `agent doctor audit
--empresa <id>` surfaces it.

## 9. Microapp tools

### 9.1 — New tools (agent-creator-microapp)

```
empresa_create        → admin.empresas.upsert
empresa_list          → admin.empresas.list
empresa_get           → admin.empresas.get
empresa_update        → admin.empresas.upsert
empresa_delete        → admin.empresas.delete
empresa_set_active    → admin.empresas.upsert (active flag only)
```

### 9.2 — Existing tools forward `empresa_id`

- `agent_list { empresa_id }` — filter forwarded.
- `agent_get { agent_id, empresa_id }` — defense-in-depth: rejects when
  agent's empresa_id != filter's.
- `agent_upsert { ..., empresa_id }` — required for new agents.
- `skill_*` tools forward `empresa_id`.
- `llm_provider_*` tools forward `empresa_id`.
- `conversation_list { empresa_id }` — filter via
  `agent_events/list.empresa_id`.
- `escalation_list { empresa_id }` — filter forwarded.

## 10. plugin.toml capability declaration

`agent-creator-microapp/plugin.toml` updates:

```toml
[capabilities.admin]
required = [
    "empresas_crud",        # NEW Phase 83.8.12
    "agents_crud", "skills_crud", "llm_keys_crud",
    "pairing_initiate", "transcripts_read",
    "operator_intervention",
    "escalations_read", "escalations_resolve",
]
optional = ["credentials_crud", "channels_crud"]
```

## 11. Channel-agnostic constraint reafirmada

Memoria `feedback_outbound_any_channel.md` aplica. Tests + docs:

- All admin RPC empresa-scoped methods MUST work for agents on
  any channel (whatsapp / telegram / email / future).
- Integration tests (Phase 83.8.4.b.b broker subscriber) verify
  takeover flow for all 3 channels under multi-empresa scope.
- Microapp v1 UI surfaces only WhatsApp; tests assert telegram
  + email paths still functional.

## 12. End-to-end use cases

### 12.1 — Operator creates empresa "acme-corp"

```
[Operator UI] POST /tools/empresa_create {
  id: "acme-corp",
  display_name: "Acme Corp.",
  llm_provider_refs: ["acme-claude-prod"]
}
→ microapp empresa_create
  → admin.empresas.upsert
    → EmpresasYamlPatcher writes config/empresas.yaml
  → operator-token capability empresas_crud OK

[Operator UI] POST /tools/llm_provider_upsert {
  empresa_id: "acme-corp",
  name: "acme-claude-prod",
  kind: "claude",
  api_key_env: "ACME_CLAUDE_KEY"
}
→ writes llm.yaml.empresas.acme-corp.providers.acme-claude-prod
```

### 12.2 — Acme client creates an agent + skill

```
[Acme client UI] POST /tools/agent_create {
  empresa_id: "acme-corp",
  id: "ventas-001",
  model: { provider: "acme-claude-prod", name: "claude-opus-4-7" },
  plugins: ["whatsapp"]
}
→ writes agents.yaml.<id> with empresa_id

[Acme client UI] POST /tools/skill_upsert {
  empresa_id: "acme-corp",
  name: "tarifario-2026",
  body: "..."
}
→ FsSkillsStore writes skills/acme-corp/tarifario-2026/SKILL.md
```

### 12.3 — Inbound message → empresa-scoped resolution

```
WhatsApp inbound for ventas-001:
  daemon resolves agent → reads empresa_id "acme-corp"
  → BindingContext { agent_id: "ventas-001", empresa_id: "acme-corp", ... }
  → llm_behavior reads ctx.empresa_id
    → llm.yaml.empresas.acme-corp.providers.acme-claude-prod
    → calls Claude with Acme's key
  → SkillLoader loads skills/acme-corp/* + skills/__global__/*
  → response → outbound dispatch
```

### 12.4 — Acme client UI lists conversations

```
[Acme UI] GET /tools/conversation_list { empresa_id: "acme-corp" }
→ admin.agent_events.list { empresa_id: Some("acme-corp"), agent_id: "*" }
  → handler joins with agents.yaml lookup, filters events to acme's agents only
→ returns. Client cannot see Globex transcripts (defense-in-depth filter).
```

### 12.5 — Operator deletes empresa with active agents

```
[Operator UI] POST /tools/empresa_delete { empresa_id: "globex", purge: false }
→ admin.empresas.delete (purge=false)
  → EmpresasYamlPatcher scans agents.yaml: 3 agents reference globex
  → returns { removed: false, orphaned_agents: ["g-001","g-002","g-003"] }
[Operator UI] modal: "Globex has 3 agents. Delete them too?"
[Operator UI] POST /tools/empresa_delete { empresa_id: "globex", purge: true }
→ cascades agents.delete for each + removes empresa from yaml
→ returns { removed: true, orphaned_agents: [] }
```

## 13. Decisions (closed-form, total 12)

| # | Decisión | Razón |
|---|---|---|
| D1 | `empresa_id: Option<String>` en BindingContext + AgentConfig | Backward-compat para agentes legacy. |
| D2 | Storage `config/empresas.yaml` archivo separado | Empresas viven más que agentes. |
| D3 | LLM providers per-empresa (sub-namespace `empresas.<id>.providers`) | Cliente paga sus keys (alineado spec D2 agent-creator-v1). |
| D4 | Skills per-empresa con `__global__/` slot | Aislamiento real + skills compartibles. |
| D5 | `purge: bool` en delete empresa | Default safe; UI confirma cascade. |
| D6 | Capability `empresas_crud` separada | RBAC granular operator vs cliente. |
| D7 | `empresa_id: None` en list = todo | Operator panorama; cliente siempre pasa su id. |
| D8 | Audit log `empresa_id` column + index | Multi-tenant observability. |
| D9 | Cross-empresa lookup → empty/Unknown | Defense-in-depth, no leak existence. |
| D10 | Migration helper para skills flat → __global__ | Sin breaking change deployments existentes. |
| D11 | Llm provider resolution: empresa-scoped first, fallback global | Permite empresa overrides + operator defaults. |
| D12 | TranscriptAppended event embebe `empresa_id` | Firehose subscribers filter sin re-query. |

## 14. Risks

- **R1** — Cross-empresa enforcement at every layer (handler +
  microapp filter). Doble check vital. Tests assert empresa A
  cannot see empresa B's resources.
- **R2** — `llm.yaml` schema migration breaks existing
  deployments. Mitigation: `empresas:` is `default`; absent
  block = no per-empresa providers, all providers operator-shared.
- **R3** — Skills migration script idempotency. Run once at
  boot; second-run no-op. Test in CI.
- **R4** — Agent count per empresa (in `EmpresaSummary`) is
  computed by scanning `agents.yaml`. O(N) per list call. v1
  acceptable; cache via `agents.yaml` mtime if N > 100.
- **R5** — Audit log SQLite migration breaks running daemons
  during deploy. Mitigation: ALTER TABLE IF NOT EXISTS pattern;
  schema_version table tracks migrations.
- **R6** — Microapp v1 already shipped without empresa_id
  param. Adding the field as optional preserves back-compat for
  single-empresa daemons (treat as `__global__`).

## 15. Out of scope v1

(Same as brainstorm §8 — per-empresa rate limits, billing,
hierarchy, marketplace, granular RBAC.)

## 16. Done criteria

- ✅ tool-meta `admin/empresas` shipped + 8 round-trip tests.
- ✅ core `domains/empresas` handler + EmpresaStore trait + capability + INVENTORY.
- ✅ EmpresasYamlPatcher production adapter + tests.
- ✅ AgentsListFilter / AgentEventsListFilter / EscalationsListParams gain empresa_id.
- ✅ LlmConfig empresas sub-namespace + resolution path.
- ✅ FsSkillsStore + SkillLoader empresa-scoped layout + migration helper.
- ✅ Audit log empresa_id column + SQLite migration + tail_for_empresa.
- ✅ Microapp empresa_* tools + filter forwarding.
- ✅ `cargo build --workspace` + `cargo test --workspace` clean.
- ✅ Docs updated.

---

**Listo para `/forge plan 83.8.12-multi-empresa`.**
