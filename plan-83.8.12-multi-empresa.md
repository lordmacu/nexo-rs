# Plan — 83.8.12 multi-empresa framework primitive

**Status:** plan (post `/forge spec 83.8.12`).
Spec: `proyecto/spec-83.8.12-multi-empresa.md`.

## Mining (regla irrompible)

Cubierto en brainstorm + spec:

- `research/src/agents/cli-credentials.ts:292` — TS account_id
  como auth-profile, no tenant. Confirma rust va más lejos.
- `research/AGENTS.md:1` — sin nivel organization. No copiar.
- `claude-code-leak/` — **ausente** (`ls` retorna "No such
  file"). Cumplido por la regla.
- `crates/tool-meta/src/binding.rs:41` — BindingContext shape.
- `crates/config/src/types/agents.rs:30` + `llm.rs:6` — config
  shapes a extender.
- `crates/setup/src/yaml_patch.rs:214,624,658,1950` — yaml
  patch surface.

## Methodology — stress-test recap

Esta sub-fase es la PRIMERA que tiene scope mayor (9 atomic
commits) desde 83.8 agent-creator. Cualquier fricción durante
ejecución → fix framework agnostic, NUNCA workaround microapp.
Memorias activas:

- `feedback_microapp_as_framework_stress_test.md`
- `feedback_outbound_any_channel.md` (channel-agnostic — empresa
  filtering ortogonal al canal)
- `project_microapp_is_saas_meta_creator.md` (constraint #1
  REVISADA — multi-empresa-en-un-daemon)
- `project_ui_whatsapp_web_react.md` (UI espera empresa como
  entidad de primer nivel)

## Numbering

```
83.8.12.1  Wire shapes — tool-meta admin/empresas + BindingContext.empresa_id + AgentConfig.empresa_id
83.8.12.2  Core admin RPC — EmpresaStore trait + domain handler + capability + INVENTORY
83.8.12.3  Production adapter — EmpresasYamlPatcher + config/empresas.yaml
83.8.12.4  Per-empresa filter — AgentsListFilter / AgentEventsListFilter / EscalationsListParams
83.8.12.5  LLM providers per-empresa — llm.yaml empresas sub-namespace + resolution
83.8.12.6  Skills per-empresa — FsSkillsStore layout + SkillLoader fallback + migration helper
83.8.12.7  Audit log empresa_id column + SQLite migration + tail_for_empresa
83.8.12.8  Microapp empresa_* tools + filter forwarding existing tools
83.8.12.9  Docs + admin-ui sync + close-out
```

9 commits. Each step `cargo build --workspace` + `cargo test
--workspace` clean before next.

---

## 83.8.12.1 — Wire shapes (tool-meta + BindingContext + AgentConfig)

**Files NEW:**

- `crates/tool-meta/src/admin/empresas.rs` — wire shapes per spec §2.

**Files MODIFIED:**

- `crates/tool-meta/src/admin/mod.rs` — `pub mod empresas;`.
- `crates/tool-meta/src/binding.rs` — add `empresa_id:
  Option<String>` field with `serde(skip_serializing_if =
  "Option::is_none")`.
- `crates/tool-meta/src/binding.rs` — `agent_only(id)`
  constructor leaves `empresa_id: None` (default).
- `crates/tool-meta/src/meta.rs` — `parse_binding_from_meta`
  reads `_meta.nexo.binding.empresa_id` when present.
- `crates/config/src/types/agents.rs` — `AgentConfig.empresa_id:
  Option<String>` with `#[serde(default,
  skip_serializing_if = "Option::is_none")]`.
- `crates/config/src/types/agents.rs` — same on
  `AgentDraftConfig` if present (any builder/struct that mirrors
  AgentConfig).

**Tests:**

- `tool-meta/src/admin/empresas.rs::tests`:
  - `empresa_summary_round_trips` (snake_case + non-skipped fields).
  - `empresa_detail_round_trips` (metadata BTreeMap preserves order).
  - `empresas_upsert_input_omits_optional_fields`.
  - `empresas_delete_response_omits_orphans_when_empty`.
  - `empresas_list_filter_default_serializes_compact`.
  - `empresas_get_response_serializes_none_explicitly`.
- `tool-meta/src/binding.rs::tests`:
  - `binding_context_round_trips_with_empresa_id`.
  - `agent_only_leaves_empresa_id_none`.
- `tool-meta/src/meta.rs::tests`:
  - `parse_binding_from_meta_reads_empresa_id_when_present`.
  - `parse_binding_from_meta_leaves_empresa_id_none_when_absent`
    (back-compat).

**Done:**

- `cargo build -p nexo-tool-meta -p nexo-config` clean.
- `cargo test -p nexo-tool-meta admin::empresas` 6+ passing.
- `cargo test -p nexo-tool-meta binding::tests` 2+ new passing.

---

## 83.8.12.2 — Core admin RPC empresas domain

**Files NEW:**

- `crates/core/src/agent/admin_rpc/domains/empresas.rs` — trait
  `EmpresaStore` + handlers `list/get/upsert/delete` + helper
  `validate_empresa_id` + `validate_display_name`.

**Files MODIFIED:**

- `crates/core/src/agent/admin_rpc/domains/mod.rs` — `pub mod
  empresas;`.
- `crates/core/src/agent/admin_rpc/dispatcher.rs`:
  - Add `empresa_store: Option<Arc<dyn EmpresaStore>>` field.
  - Add `with_empresas_domain(store: Arc<dyn EmpresaStore>)`
    builder.
  - Add 4 dispatch arms `nexo/admin/empresas/{list,get,upsert,delete}`.
  - Capability `empresas_crud` in `required_capability` map.
- `crates/core/src/agent/admin_rpc/capabilities.rs` —
  `pub const EMPRESAS_CRUD: &str = "empresas_crud";` (or
  string literal directly).
- `crates/setup/src/capabilities.rs::INVENTORY` — entry
  `NEXO_MICROAPP_ADMIN_EMPRESAS_ENABLED` (Risk::High,
  description, hint).

**Tests (in `domains/empresas.rs::tests`):**

- `InMemoryEmpresaStore` mock (BTreeMap-backed).
- `list_empty_returns_empty`.
- `upsert_then_get_round_trip`.
- `upsert_twice_reports_created_false_second`.
- `get_missing_returns_null_not_error`.
- `delete_missing_returns_false`.
- `delete_with_orphan_agents_returns_orphans_when_purge_false` (mock agents-yaml lookup hook).
- `invalid_id_rejected_with_invalid_params` (ALL_CAPS, dots, slashes, traversal).
- `display_name_too_long_rejected`.
- `list_filter_active_only_drops_inactive`.

Capability gate test (in `dispatcher.rs::tests`):

- `nexo/admin/empresas/list` without `empresas_crud` grant
  returns `-32004`.

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-core admin_rpc::domains::empresas` 10+
  passing.
- `cargo test -p nexo-core admin_rpc::dispatcher empresas`
  capability gate passing.

---

## 83.8.12.3 — EmpresasYamlPatcher production adapter

**Files MODIFIED:**

- `crates/setup/src/admin_adapters.rs` — append:
  - `pub struct EmpresasYamlPatcher { path: PathBuf, agents_yaml: Arc<dyn YamlPatcher> }`.
  - `impl EmpresaStore for EmpresasYamlPatcher` — atomic
    write via `tmp+rename` (same pattern as `FsSkillsStore`).
  - `agent_count` computed by scanning agents.yaml.
  - `delete(purge=false)` returns `(false, orphan_ids)`.
  - `delete(purge=true)` cascades agents.delete, then removes
    empresa.
- `crates/setup/src/yaml_patch.rs` — new helper
  `pub fn list_agents_by_empresa(path: &Path, empresa_id:
  &str) -> Result<Vec<String>>`.

**`config/empresas.yaml` example:** added to spec §4.4.
Repository ships an empty stub at `config/empresas.example.yaml`
documenting the shape; no actual `empresas.yaml` committed
(secrets-style — operator creates).

**Tests (in `admin_adapters.rs::tests`):**

- `fs_empresas_upsert_then_list_round_trips` (tempdir).
- `fs_empresas_delete_purge_false_returns_orphans`.
- `fs_empresas_delete_purge_true_cascades`.
- `fs_empresas_list_filters_by_active_only`.
- `fs_empresas_concurrent_upsert_same_id_no_corruption`.
- `fs_empresas_atomic_write_no_partial_file_on_crash` (panic
  inside write — tempfile validates rename pattern).
- `fs_empresas_invalid_id_rejected` (defense-in-depth).

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-setup admin_adapters::*empresas*` 7+
  passing.

---

## 83.8.12.4 — Per-empresa filter on existing admin RPC

**Files MODIFIED:**

- `crates/tool-meta/src/admin/agents.rs` — `AgentsListFilter`
  gain `empresa_id: Option<String>` (serde default).
- `crates/tool-meta/src/admin/agent_events.rs` —
  `AgentEventsListFilter` gain `empresa_id: Option<String>`.
- `crates/tool-meta/src/admin/agent_events.rs` —
  `TranscriptAppended` variant gain `empresa_id: Option<String>`
  (for firehose subscribers — defense-in-depth).
- `crates/tool-meta/src/admin/escalations.rs` —
  `EscalationsListParams` gain `empresa_id: Option<String>`.
- `crates/core/src/agent/admin_rpc/domains/agents.rs` — `list`
  handler honours `filter.empresa_id` (read each yaml row's
  `empresa_id`, compare).
- `crates/core/src/agent/admin_rpc/domains/agent_events.rs` —
  `list` handler joins with agents-yaml empresa lookup,
  filters events.
- `crates/core/src/agent/admin_rpc/domains/escalations.rs` —
  `list` handler filters in-memory by `empresa_id`.
- `crates/core/src/agent/transcripts/...` (whoever emits
  `TranscriptAppended`) — populate the new `empresa_id` field
  from `BindingContext.empresa_id` at emit time.

**Tests:**

- Each domain handler test gains an `empresa_id` filter case:
  - `agents_list_filter_by_empresa_returns_only_matching`.
  - `agents_list_no_empresa_filter_returns_all`.
  - `agent_events_list_filter_by_empresa_drops_others`.
  - `escalations_list_filter_by_empresa_drops_others`.
- `TranscriptAppended` round-trip with `empresa_id: Some("acme")`.

**Done:**

- `cargo build --workspace` clean.
- All workspace tests pass with new fields (existing tests
  default `empresa_id: None`).

---

## 83.8.12.5 — LLM providers per-empresa

**Files MODIFIED:**

- `crates/config/src/types/llm.rs`:
  - `LlmConfig` gain `empresas: HashMap<String,
    EmpresaLlmConfig>` (serde default empty).
  - New struct `EmpresaLlmConfig { providers: HashMap<String,
    LlmProviderConfig> }`.
- `crates/config/src/types/llm.rs` — helper
  `LlmConfig::resolve_provider(&self, empresa_id:
  Option<&str>, name: &str) -> Option<&LlmProviderConfig>`
  with empresa-first / global-fallback semantics.
- `crates/setup/src/yaml_patch.rs` — new helper
  `upsert_empresa_llm_provider_field(path, empresa_id, name,
  dotted, value)` mirroring `upsert_llm_provider_field`.
- `crates/tool-meta/src/admin/llm_providers.rs`:
  - `LlmProviderSummary` gain `empresa_scope:
    Option<String>` field.
  - `LlmProviderUpsertInput` gain `empresa_id:
    Option<String>` (serde default).
  - `LlmProvidersDeleteParams` gain `empresa_id:
    Option<String>`.
- `crates/core/src/agent/admin_rpc/domains/llm_providers.rs` —
  handlers honour `empresa_id` parameter.
- `crates/core/src/agent/llm/...` — wherever an agent's
  provider name is resolved to a `LlmProviderConfig`, pass
  `BindingContext.empresa_id` and call
  `LlmConfig::resolve_provider(empresa, name)` instead of
  bare `providers.get(name)`.

**Tests:**

- `LlmConfig::resolve_provider` (5 cases):
  - empresa-scoped present → returns empresa version.
  - empresa-scoped missing → falls back to global.
  - both missing → returns None.
  - empresa_id None → only global.
  - empresa-scoped exists, global also exists, returns empresa
    (precedence).
- `llm_providers_upsert_with_empresa_id_writes_under_empresa_namespace`.
- `llm_providers_list_includes_empresa_scope_field`.
- Cross-empresa lookup test: upsert `acme.foo`, attempt
  `globex.foo` get → returns None (defense-in-depth).

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-config llm::resolve_provider` 5 passing.
- `cargo test -p nexo-core admin_rpc::domains::llm_providers`
  empresa-scope tests passing.

---

## 83.8.12.6 — Skills per-empresa layout

**Files MODIFIED:**

- `crates/setup/src/admin_adapters.rs::FsSkillsStore`:
  - Per-empresa root resolution: when `params.empresa_id ==
    Some(eid)` → write to `<root>/<eid>/<name>/SKILL.md`.
  - When `None` (or `__global__`) → `<root>/__global__/<name>/`.
  - Same for read/list/delete.
- `crates/core/src/agent/skills.rs::SkillLoader`:
  - New constructor `SkillLoader::with_empresa(root, empresa_id)`
    that loads `<root>/<eid>/*` first, falls back to
    `<root>/__global__/*` for missing names.
  - Existing `SkillLoader::new(root)` keeps current behaviour
    for backward compat (treats `root` as flat dir).
- `crates/core/src/agent/skills.rs` — new helper
  `migrate_flat_skills_to_global(root: &Path) -> Result<()>`:
  - Walks `<root>/<name>/SKILL.md`.
  - For any `<name>` that is NOT `__global__` and NOT a known
    empresa id → moves to `<root>/__global__/<name>/SKILL.md`.
  - Idempotent (skip dirs that already moved).
- `crates/setup/src/admin_bootstrap.rs` — call migration once
  at boot when `skills_root` is configured, before construction
  of `FsSkillsStore`.
- `crates/tool-meta/src/admin/skills.rs` — `SkillsListParams /
  SkillsUpsertParams / SkillsDeleteParams / SkillsGetParams`
  gain `empresa_id: Option<String>` (serde default).
- `crates/core/src/agent/admin_rpc/domains/skills.rs` —
  handlers forward `empresa_id`.

**Tests:**

- `migrate_flat_skills_idempotent_second_run_no_op`.
- `migrate_flat_skills_skips_already_migrated`.
- `fs_skills_upsert_with_empresa_id_writes_under_empresa_dir`.
- `fs_skills_get_falls_back_to_global_when_empresa_missing`
  (only if `SkillLoader::with_empresa` is used).
- `fs_skills_list_with_empresa_id_only_returns_that_empresa`.
- `fs_skills_delete_with_empresa_id_only_removes_from_empresa_dir`.
- `skill_loader_with_empresa_loads_empresa_then_global`.

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-setup admin_adapters::*skills*` updated +
  passing.
- `cargo test -p nexo-core skills::*` updated + passing.

---

## 83.8.12.7 — Audit log empresa_id column

**Files MODIFIED:**

- `crates/core/src/agent/admin_rpc/audit.rs::AdminAuditRow` —
  add `pub empresa_id: Option<String>` field.
- `crates/core/src/agent/admin_rpc/audit_sqlite.rs`:
  - SQL schema gain `empresa_id TEXT NULL` column.
  - Schema migration at open time:
    `CREATE INDEX IF NOT EXISTS idx_admin_audit_empresa
    ON admin_audit(empresa_id)`.
  - Schema-version table tracks migrations
    (`PRAGMA user_version` style).
  - New query method `tail_for_empresa(empresa_id, ...) ->
    Vec<AdminAuditRow>` mirroring `tail_for_account`.
- `crates/core/src/agent/admin_rpc/dispatcher.rs::dispatch` —
  populate `row.empresa_id` from request params (best-effort
  lookup of `params.empresa_id` field via serde Value).
- `crates/cli/...` (or wherever `agent doctor audit` lives) —
  add `--empresa <id>` flag that calls `tail_for_empresa`.

**Tests:**

- `audit_row_round_trips_with_empresa_id`.
- `sqlite_open_runs_migration_idempotently`.
- `tail_for_empresa_returns_only_matching_rows`.
- `dispatch_populates_empresa_id_from_params_when_present`.

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-core admin_rpc::audit*` 4+ passing.
- Existing audit tests still pass (back-compat — old rows have
  `empresa_id: None`).

---

## 83.8.12.8 — Microapp empresa_* tools + filter forwarding

**Repo:** `/home/familia/chat/agent-creator-microapp/`.

**Files NEW:**

- `src/tools/empresas.rs` — handlers
  `empresa_create / list / get / update / delete / set_active`.

**Files MODIFIED:**

- `src/tools/mod.rs` — `pub mod empresas;`.
- `src/main.rs` — register 6 new tools.
- `src/tools/agents.rs` — `agent_list / agent_upsert / agent_delete`
  forward `empresa_id` parameter.
- `src/tools/skills.rs` — same forwarding.
- `src/tools/llm.rs` — same forwarding.
- `src/tools/conversations.rs` — same forwarding.
- `src/tools/escalations.rs` — same forwarding.
- `plugin.toml` — `[capabilities.admin] required` adds
  `empresas_crud`. Tool list adds 6 names.

**Tests:**

- `empresa_create_round_trips_via_admin_mock`.
- `empresa_list_returns_summary_array`.
- `agent_list_forwards_empresa_id_filter`.
- `skill_list_forwards_empresa_id`.

**Done:**

- `cargo build` clean en agent-creator-microapp.
- `cargo test` passing (existing 4 + 4 new = 8 minimum).
- Tool count: 22 + 6 = 28 total.

---

## 83.8.12.9 — Docs + admin-ui sync + close-out

**Files MODIFIED:**

- `docs/src/microapps/agent-creator.md` — empresa CRUD section,
  multi-empresa flow walkthrough.
- `docs/src/admin/multi-empresa.md` — NEW dedicated framework
  page with config layout (empresas.yaml, llm.yaml empresas
  sub-namespace, skills/<empresa>/ layout).
- `docs/src/SUMMARY.md` — index new page.
- `proyecto/PHASES-microapps.md` — Phase 83.8.12 row with full
  9-step table.
- `proyecto/CLAUDE.md` — Phase 83 contador 13/17 → 14/17.
- `proyecto/FOLLOWUPS.md` — move 83.8.12 entry from "Open" to
  "Resolved" with date.
- `proyecto/CHANGELOG.md` — `[Unreleased]` entry summarising
  multi-empresa shipped.
- `admin-ui/PHASES.md` — checkbox for empresa CRUD UI panel
  (operator dashboard).

**Tests:**

- `mdbook build docs` clean.
- `cargo build --workspace && cargo test --workspace` final
  green light.

**Done:**

- All docs files committed.
- FOLLOWUPS shows 83.8.12 resolved + new follow-ups (per-empresa
  rate limits, billing integration, hierarchy, marketplace).
- Final commit subject: `feat(83.8.12): multi-empresa framework
  primitive shipped`.

---

## Risks (consolidated from spec §14)

- **R1 — Cross-empresa enforcement** — every layer (handler +
  microapp + UI) must filter. Tests assert empresa A cannot see
  empresa B data. Mitigation: integration test in 83.8.12.4
  covers cross-empresa lookup → empty.
- **R2 — `llm.yaml` schema migration** — `empresas:` defaults
  to empty. No existing deployment breaks.
- **R3 — Skills migration idempotency** — boot helper detects
  already-migrated layout (every dir is `__global__` or known
  empresa id). Test in 83.8.12.6.
- **R4 — Agent count perf** — O(N) yaml scan. v1 acceptable;
  cache via mtime is follow-up if N > 100.
- **R5 — SQLite ALTER TABLE** — `IF NOT EXISTS` pattern +
  schema-version table. Test in 83.8.12.7.
- **R6 — Microapp v1 back-compat** — `empresa_id` is
  `Option<String>` everywhere with serde defaults. Single-empresa
  daemons keep working without setting it (treat agents as
  `__global__` / no empresa).
- **R7 — Cascade delete bug** — `purge: true` could orphan if
  agents.yaml write fails mid-cascade. Mitigation: cascade
  agents-first, empresa-last; if an agent fails to delete, abort
  before removing empresa.

## Order of execution

```
83.8.12.1 → 83.8.12.2 → 83.8.12.3
            ↓
            83.8.12.4 → 83.8.12.5 → 83.8.12.6 → 83.8.12.7
                                                ↓
                                                83.8.12.8 → 83.8.12.9
```

Steps .4-.7 can run in parallel after .3 if a contributor
splits, but for the single-author /forge ejecutar flow they run
sequentially. Each commit must `cargo build --workspace` clean
+ existing tests pass before next.

## Estimate

- 9 commits.
- ~1500 LOC additive (mostly tests).
- Each step bounded ≤ ~300 LOC (largest: .8 microapp tools).

## Out of plan — microapp-side DB

Cristian recordó 2026-05-02: la microapp puede tener su propia
SQLite DB para datos derivados (caches UI, prefs operador,
analytics, domain workflows). Framework state (yamls + skills
+ audit) sigue siendo SoT vía admin RPC, microapp DB es capa
derivada. Memoria:
`feedback_microapp_owns_its_db.md`.

Para 83.8.12 esta sub-fase NO añade microapp DB — el plan se
mantiene framework-only (yamls + admin RPC). Cuando la UI
(Phase 83.12) lo necesite (ej. para "última empresa vista" o
empresa→display_name cache), se introduce ahí. El plan UI
incluirá `<state_root>/agent-creator/microapp.db` como work
item.

---

**Listo para `/forge ejecutar 83.8.12-multi-empresa`.**
