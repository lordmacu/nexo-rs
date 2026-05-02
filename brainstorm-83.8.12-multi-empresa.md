# Brainstorm — 83.8.12 multi-empresa framework primitive

**Status:** brainstorm. Sub-fase derivada de FOLLOWUPS.md
"Phase 83.8.12 — multi-empresa framework primitive". Decisión
producto 2026-05-02: 1 daemon hosts N empresas.

## 1. Contexto

El SaaS `agent-creator` (Phase 83.8) fue diseñado bajo el modelo
"1 daemon = 1 empresa" (D7 de la spec). Cristian revisó el modelo
2026-05-02: **toda la microapp tiene un daemon, ese daemon hosts
N empresas**. Esto requiere una nueva primitiva framework: el
concepto **empresa**, que sits arriba del `account_id` existente.

| Capa | Hoy | Después de 83.8.12 |
|---|---|---|
| `account_id` | Channel-side discriminator (WhatsApp phone) | Mismo — no cambia |
| `empresa_id` | No existe | **Nuevo** tenant key, sits encima de `account_id` |
| Agentes | Globales en `agents.yaml` | Cada agente pertenece a UNA empresa |
| LLM providers | Globales en `llm.yaml` | Por-empresa (cada empresa sus keys) |
| Skills | Globales en `<root>/<name>/SKILL.md` | Por-empresa o globales (decidir spec) |
| Transcripts | Filtered por agent_id | Filtered por empresa_id (suma de sus agents) |

## 2. Mining (regla irrompible)

### research/

- `research/src/agents/cli-credentials.ts:292` — TS usa
  `account_id` como key de auth-profile, NO como tenant. Mismo
  patrón que Rust hoy: `account_id` es channel-side. **Confirma**
  que TypeScript NO modela "empresa/organization" en agent-side,
  delega multi-tenancy al deployment (1 instance per cliente).
  Rust va más lejos: 1 daemon multi-empresa.
- `research/AGENTS.md:1` — describe agentes como entidades
  globales con su prompt + canales. Sin nivel "organization"
  encima. **No copiar** — el modelo SaaS necesita el nivel
  empresa.

### claude-code-leak/

**Ausente.** `ls /home/familia/chat/claude-code-leak/` → "No
such file or directory". Cumplido por la regla.

### In-repo source-of-truth

- `crates/tool-meta/src/binding.rs:41-81` — `BindingContext`
  campos hoy: `agent_id, session_id, channel, account_id,
  binding_id, mcp_channel_source, event_source`. **Falta**
  `empresa_id`.
- `crates/config/src/types/agents.rs:30-50` — `AgentConfig.id` +
  `extensions_config: BTreeMap`. **Falta** `empresa_id`.
- `crates/core/src/agent/admin_rpc/domains/agents.rs` —
  `AgentsListFilter` solo tiene `active_only` + `plugin_filter`.
  **Falta** `empresa_id` filter.
- `crates/setup/src/admin_adapters.rs::AgentsYamlPatcher` —
  yaml mutator. Hoy lista todos los agentes; debe filtrar por
  empresa cuando el filtro está set.

## 3. Diseño propuesto

### 3.1 — Concepto `empresa_id`

`empresa_id: Option<String>` — string-id estilo `[a-z0-9-]+`,
asignado al crear la empresa. `None` significa "agente global"
(legacy / agentes pre-multi-empresa que el operador del SaaS
opera directamente).

Storage: nuevo file `config/empresas.yaml`:

```yaml
empresas:
  - id: acme-corp
    display_name: "Acme Corp."
    created_at: 2026-05-02T10:00:00Z
    active: true
    llm_provider_refs:        # qué providers de llm.yaml usa esta empresa
      - acme-claude-prod
      - acme-minimax-fallback
    metadata: {}              # free-form: contact info, billing tier, etc.
  - id: globex
    display_name: "Globex S.A."
    ...
```

Por qué archivo separado, no embebido en `agents.yaml`: empresas
viven más tiempo que agentes. Borrar agentes ≠ borrar empresa.

### 3.2 — `BindingContext.empresa_id`

```rust
#[non_exhaustive]
pub struct BindingContext {
    // existing fields...
    /// Phase 83.8.12 — SaaS tenant key. `None` for legacy agents
    /// that pre-date multi-empresa. Multi-tenant filtering on
    /// admin RPC + microapp tools keys on this field.
    pub empresa_id: Option<String>,
}
```

Producer-side: cuando el daemon construye `BindingContext` para
un agente, copia `agents.yaml.<agent_id>.empresa_id` al campo.
Sin cambios en producers existentes — queda `None` para agentes
sin empresa asignada.

### 3.3 — `AgentConfig.empresa_id`

```rust
pub struct AgentConfig {
    pub id: String,
    /// Phase 83.8.12 — empresa owner. `None` = global agent
    /// (operator-level, not tenant-owned).
    pub empresa_id: Option<String>,
    pub model: ModelConfig,
    ...
}
```

YAML:

```yaml
agents:
  - id: ventas-acme-001
    empresa_id: acme-corp     # nuevo
    model: ...
    plugins: ...
```

### 3.4 — Admin RPC `nexo/admin/empresas/*`

Nuevo domain. Wire shapes en `crates/tool-meta/src/admin/empresas.rs`:

```rust
pub struct EmpresaSummary {
    pub id: String,
    pub display_name: String,
    pub active: bool,
    pub agent_count: usize,         // computed from agents.yaml
    pub created_at: DateTime<Utc>,
}

pub struct EmpresaDetail {
    pub id: String,
    pub display_name: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub llm_provider_refs: Vec<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

pub struct EmpresasListFilter { /* active_only, etc. */ }
pub struct EmpresasListResponse { pub empresas: Vec<EmpresaSummary> }
pub struct EmpresasGetParams { pub empresa_id: String }
pub struct EmpresasUpsertInput {
    pub id: String,
    pub display_name: String,
    pub active: Option<bool>,
    pub llm_provider_refs: Option<Vec<String>>,
    pub metadata: Option<BTreeMap<String, serde_json::Value>>,
}
pub struct EmpresasDeleteParams { pub empresa_id: String, pub purge: bool }
pub struct EmpresasDeleteResponse { pub removed: bool, pub orphaned_agents: Vec<String> }
```

Capability: `empresas_crud` (nuevo en INVENTORY).

### 3.5 — `EmpresaStore` trait + production adapter

```rust
#[async_trait]
pub trait EmpresaStore: Send + Sync + std::fmt::Debug {
    async fn list(&self, filter: &EmpresasListFilter) -> anyhow::Result<Vec<EmpresaSummary>>;
    async fn get(&self, empresa_id: &str) -> anyhow::Result<Option<EmpresaDetail>>;
    async fn upsert(&self, params: EmpresasUpsertInput) -> anyhow::Result<(EmpresaDetail, bool)>;
    async fn delete(&self, empresa_id: &str, purge: bool) -> anyhow::Result<EmpresasDeleteResponse>;
}
```

Production adapter: `nexo-setup::admin_adapters::EmpresasYamlPatcher` ↔
`config/empresas.yaml`. Reusa el patrón existente
(`AgentsYamlPatcher`). Atomic write via tmp+rename.

### 3.6 — Per-empresa filtering en admin RPC

Extender los filtros existentes:

```rust
pub struct AgentsListFilter {
    pub active_only: bool,
    pub plugin_filter: Option<String>,
    /// Phase 83.8.12 — empresa scope. None returns every
    /// agent regardless of empresa. Some(id) filters to that
    /// empresa's agents only.
    pub empresa_id: Option<String>,
}
```

Mismo en:
- `AgentEventsListFilter` (Phase 82.11 firehose backfill).
- `EscalationsListParams` (Phase 82.14).
- Microapp tools (parameter forwarding).

### 3.7 — LLM providers per-empresa

Decisión clave. Dos opciones:

**A**: Mantener `llm.yaml` global, pero `Empresa.llm_provider_refs:
Vec<String>` lista qué providers globales puede usar cada
empresa. Operador del SaaS gestiona keys, cobra por uso.

**B**: Cada empresa tiene su sub-namespace en `llm.yaml`:
`empresas.<id>.llm_providers.<provider>.api_key_ref`. Cliente
empresa pone su propia key, paga directo al provider.

**Recomendación**: B (alineado con D2 de la spec —
"API keys LLM las pone el cliente, NO el operador"). Microapp
de cliente puede CRUD sus propias keys sin tocar las del
operador.

Implementación: `LlmYamlPatcher` aprende a navegar el
sub-namespace cuando un `empresa_id` se pasa.

### 3.8 — Skills: ¿per-empresa o globales?

Decisión TBD spec. Argumentos:

**Per-empresa**: aislamiento real. Acme no ve los skills de
Globex. Storage layout: `skills/<empresa_id>/<skill_name>/SKILL.md`.

**Globales**: simpler. Operador SaaS publica skills, todas las
empresas las usan. Marketplace de skills futuro.

**Recomendación v1**: per-empresa (consistente con el modelo
multi-tenant). Layout `skills/<empresa_id>/<name>/SKILL.md`.
`FsSkillsStore::new()` ya aceptaba un root path; se inyecta el
sub-path por empresa. Globales = `skills/__global__/`.

### 3.9 — Audit log enrichment

`crates/core/src/agent/admin_rpc/audit.rs::AdminAuditRow` ya
tiene `account_id` (Phase 82.8). Añadir `empresa_id: Option<String>`
para que `agent doctor audit` filter por empresa. SQLite migration
adds the column.

### 3.10 — Microapp tools

Microapp side (`agent-creator-microapp`):

- `empresa_create / empresa_list / empresa_get / empresa_update / empresa_delete`
- `empresa_set_active`
- Existing `agent_*` tools gain optional `empresa_id` filter.
- Existing `skill_*` tools gain `empresa_id` parameter (which
  empresa's skills root).
- Existing `llm_provider_*` tools gain `empresa_id` to scope.

UI side (Phase 83.12): top-level "create empresa" → enter empresa
→ CRUD agents/skills/keys dentro.

## 4. Decisiones (closed-form)

| # | Decisión | Razón |
|---|---|---|
| D1 | `empresa_id` opcional en BindingContext + AgentConfig | Backward-compat: agentes legacy quedan `None`, siguen operando. |
| D2 | Storage = `config/empresas.yaml` archivo separado | Empresas viven más que agentes; mezclarlos en agents.yaml complica el delete agente. |
| D3 | LLM providers per-empresa (opción B) | Cliente paga su consumo; alineado con spec D2. |
| D4 | Skills per-empresa con `skills/<empresa_id>/` | Aislamiento real. `__global__` slot para skills compartidos. |
| D5 | `purge: bool` en delete empresa | Borrar empresa con agentes activos → opt-in al purge en cascada. Default `false` retorna `orphaned_agents` para que UI confirme. |
| D6 | `empresas_crud` cap separada de `agents_crud` | Operator SaaS puede tener `empresas_crud` sin `agents_crud` (provisioning role); cliente empresa al revés. |
| D7 | Filter `empresa_id: None` en list = retorna TODAS | Operador del SaaS quiere ver el panorama completo. Cliente empresa pasa su `empresa_id` siempre. |
| D8 | Audit log gana columna empresa_id | Multi-tenant observability. SQLite migration additive. |

## 5. Casos de uso end-to-end

```
[Operador SaaS] CREATE empresa "acme-corp"
  → microapp empresa_create
    → admin.empresas.upsert
      → EmpresasYamlPatcher writes config/empresas.yaml
    → operator-token capability empresas_crud OK

[Cliente "acme-corp"] CREATE agent "ventas-001"
  → microapp agent_create { empresa_id: "acme-corp", id: "ventas-001", ... }
    → admin.agents.upsert (writes agents.yaml.<id>.empresa_id = "acme-corp")

[Cliente "acme-corp"] LIST agents
  → microapp agent_list { empresa_id: "acme-corp" }
    → admin.agents.list { filter.empresa_id: Some("acme-corp") }
    → returns only acme's agents (defense-in-depth multi-tenant)

[Inbound WhatsApp message lands]
  → daemon binds agent ventas-001 → BindingContext.empresa_id = "acme-corp"
  → llm_behavior reads BindingContext.empresa_id → resolves provider
    via Empresa.llm_provider_refs → calls Claude with acme's key
  → transcript event published with empresa_id stamped

[Cliente "acme-corp"] firehose subscribe
  → SDK TranscriptStream::filter_by_empresa("acme-corp") (new helper)
    → drops events where empresa_id != "acme-corp"
```

## 6. Riesgos / open questions

- **R1 — Multi-tenant security**: cross-empresa lookup
  (cliente A pide los agentes de empresa B) DEBE retornar
  empty/Unknown, no error filtrante. Capability gate
  `empresas_crud` debe scope por empresa. Defense-in-depth:
  microapp side filtra por su empresa antes de llamar admin RPC,
  Y admin RPC re-filtra. Doble check.
- **R2 — LLM provider migration**: hoy `llm.yaml.providers.*` es
  global. Migration: providers globales se mueven a
  `__global__` empresa especial; nuevos providers se crean
  per-empresa. Sin breaking change para deployments existentes.
- **R3 — Skills migration**: hoy `skills/<name>/` es plano. Si
  optamos por D4 per-empresa, hay que mover lo existente a
  `skills/__global__/<name>/` o keep a flat fallback path. El
  `SkillLoader` pre-existing se actualiza para entender la
  estructura jerárquica.
- **R4 — Empresa orphan agents**: si delete empresa con agentes
  vivos `purge: false`, retornamos lista de orphan agents y la
  empresa queda en estado deleted-pending. Operador decide si
  reasignar agents a otra empresa o hacer purge.
- **R5 — Capability granularity**: ¿`empresas_crud` global per
  empresa o per-empresa-token? v1 = global (operador SaaS
  manage all). Per-empresa-scoping = follow-up.
- **R6 — Skills LLM cross-empresa share**: si cliente Acme
  comparte una skill útil con Globex, ¿cómo? v1 = no se
  comparte; copy-paste manual. Marketplace = follow-up.
- **R7 — Audit log retention per-empresa**: cliente puede pedir
  "delete my data". v1 audit log es global; per-empresa retention
  policy = follow-up.

## 7. Cortes propuestos para spec

```
83.8.12.1  Wire shapes — tool-meta admin/empresas + BindingContext.empresa_id + AgentConfig.empresa_id
83.8.12.2  Core admin RPC empresas domain handler + EmpresaStore trait + capability + INVENTORY
83.8.12.3  Production adapter — EmpresasYamlPatcher + config/empresas.yaml
83.8.12.4  Per-empresa filter en agents/list + agent_events/list + escalations/list
83.8.12.5  LLM providers per-empresa (sub-namespace en llm.yaml)
83.8.12.6  Skills per-empresa (skills/<empresa_id>/<name>/) + migration helper
83.8.12.7  Audit log empresa_id column + SQLite migration
83.8.12.8  Microapp empresa_* tools + agent_* filter forwarding
83.8.12.9  Docs + close-out
```

9 commits estimate. Bigger than 83.8.4.b (3 commits) but each
step bounded. Could partition further si algún corte excede ~150
LOC.

## 7.5 — Channel-agnostic constraint reafirmada

Cristian recordó 2026-05-02: **microapp v1 sólo expone WhatsApp
en su UI, pero el framework debe soportar la funcionalidad
multi-empresa Y la pausa/takeover en cualquier canal**. Los
mismos primitivos `nexo/admin/processing/{pause,intervention,resume}`
que Phase 82.13 + Phase 83.8.4.b ya hicieron channel-agnostic
(WhatsApp + Telegram + Email translators shipped en 83.8.4.b.b)
deben seguir funcionando con `empresa_id` filtering.

Implicaciones para esta sub-fase:

- `ProcessingScope::Conversation` ya tiene `channel: String`. Un
  empresa puede tener agentes en N canales distintos. El filter
  por empresa debe ortogonal al canal — pausar conversation X
  del agent de empresa Acme funciona igual sea WhatsApp,
  Telegram, Email, o futuro.
- `BindingContext.empresa_id` se setea regardless del channel.
- `nexo/admin/processing/intervention` con `Reply` action
  enruta via `BrokerOutboundDispatcher` (Phase 83.8.4.b), que
  routes por `OutboundMessage.channel` a los translators
  WhatsApp/Telegram/Email/futuros — sin tocar.
- Audit log empresa_id column captura intervention events de
  cualquier canal por igual.
- Microapp v1 limita el flow a `channel: "whatsapp"` en su UI;
  framework tests verifican el path para `telegram` + `email`
  también.

Memoria cross-ref: `feedback_outbound_any_channel.md` +
`feedback_only_whatsapp_channel.md`.

## 8. Out of scope v1

- Per-empresa rate limits (usar Phase 82.7 framework).
- Per-empresa billing integration.
- Empresa hierarchy (parent/child empresas).
- Skill marketplace cross-empresa.
- Per-empresa audit retention policy.
- Empresa-level capability scoping (granular RBAC per empresa).

---

**Listo para `/forge spec 83.8.12-multi-empresa`.**
