# Follow-ups

This file tracks the **active technical backlog** in English.

Historical detailed notes that were previously written in Spanish are preserved at:
- `archive/spanish/FOLLOWUPS.es.txt`

## Rules

- After each `/forge ejecutar`, add any deferred work here.
- Keep each item with: what is missing, why it was deferred, and target phase.
- Move completed items to `Resolved` with a completion date.

## Current status

- Main roadmap phases are completed through Phase 19.
- Active work is now hardening, operational polish, and optional capability expansion.

## Open items

### Phase 36.2 — Agent memory snapshots (deferred items)

The `nexo-memory-snapshot` crate is feature-complete and operational.
Three deferred items track follow-up commits — each is isolated and
does not block production use of the feature.

- **MS-1 — Mutation hook fire-site sweep + boot publisher wire**
  ✅ **shipped** (commit `208da43`). Init-order shuffle put the
  snapshotter + `BrokerEventPublisher` + mutation hook
  construction immediately after broker init so
  `LongTermMemory::open_with_vector` picks up
  `with_mutation_hook(...)` cleanly.
  `LongTermMemory::remember_typed` + `forget` fire `Insert` /
  `Delete` events onto `nexo.memory.mutated.<agent_id>` via
  `BrokerEventPublisher` wrapping `AnyBroker`. Best-effort: a
  serialize or publish failure logs `tracing::warn!` and never
  poisons the writer's transaction.

- **MS-1.b — Remaining fire sites**
  ✅ **partial / vector + concepts + git shipped, compactions
  open**.
  - **vector + concepts**: shipped transactionally via the
    `LongTermMemory` fire site (commit `208da43`). Actual writes
    live inside `remember_typed` / `forget`, so a single
    `SqliteLongTerm` event is logically correct. `MutationScope::
    SqliteVector` / `SqliteConcepts` variants stay reserved for
    future standalone writers.
  - **git**: shipped (commit `fabfd38`). `MemoryGitRepo::commit_all`
    fires `Git/Update` events post-success via
    `tokio::runtime::Handle::try_current().spawn(...)` — fire-
    and-forget so the libgit2 thread is never blocked. Boot wire
    attaches the hook to every per-agent repo. 2 unit tests
    cover happy path + clean-tree no-event.
  - **compactions** ⬜ — still open. `CompactionStore` is global
    per-deployment and lacks an `agent_id` correlation token in
    its method signatures. Wiring needs a schema decision:
    either add `agent_id` to `CompactionRow` (breaking schema)
    or move to a per-agent store (big refactor). Defer until the
    operator surface demands compaction-event observability.
  - Effort remaining: ~30 min once the schema decision lands.

- **MS-2 — Per-agent memdir / sqlite path discovery**
  ✅ **shipped** (commit `e78d75f`). New `PathResolver` trait in
  `crates/memory-snapshot/src/path_resolver.rs` plus two impls
  (`DefaultPathResolver` over the YAML globals,
  `ClosureResolver<F1, F2>` for boot-time strategy injection).
  `LocalFsSnapshotterBuilder::path_resolver(Arc<dyn PathResolver>)`
  threads the override through; `snapshot.rs::build_bundle` and
  `restore.rs::apply_restore` consult the resolver. Restore
  picks the tenant from the bundle's manifest so resolver calls
  match what was used at snapshot time.

- **MS-2.b — Inject a `ClosureResolver` from the agent registry
  at boot**
  ✅ **shipped** (commit `3ffc71d`). Boot wire builds a
  `HashMap<agent_id, workspace_pathbuf>` from `cfg.agents.agents`
  and feeds a `nexo_memory_snapshot::ClosureResolver` into the
  snapshotter via `path_resolver(...)`. Agents not in the map
  fall back to `<memdir_root>/<agent_id>` (preserves the
  default behavior). SQLite stays globally shared — same as
  before — until the long-term store goes per-agent.

- **MS-3 — `BootDeps` consumer in `Mode::Run` for AutoDreamRunner**
  ✅ **shipped** (commit `5fe2cc0`). `src/main.rs::Mode::Run`
  per-agent loop now constructs an `AutoDreamRunner` for every
  agent with `auto_dream.enabled = true`, threading the
  `PreDreamSnapshotAdapter` over the shared `Arc<dyn
  MemorySnapshotter>` when `memory.snapshot.auto_pre_dream` is
  on. The runner reports `has_pre_dream_snapshot()` true, the
  fork pass fires the adapter via the
  `nexo_driver_types::PreDreamSnapshotHook` contract, and the
  resulting bundle lands at
  `auto:pre-dream-<run_id>` per Phase 36.2.

- **80.1.b.b.b.b — orchestrator runtime-attach**
  ✅ **shipped** (commit `549828c`). `DriverOrchestrator::auto_dream`
  now lives behind a `Mutex<Option<Arc<dyn AutoDreamHook>>>`;
  `set_auto_dream(Option<...>)` is the public setter the boot
  wire calls after the per-agent loop closes. Multi-runner
  routing within the orchestrator stays open as
  `80.1.b.b.b.c` (per-goal_id dispatch).

- **80.1.b.b.b.c — per-goal_id multi-runner dispatch** ✅
  **shipped** — `DriverOrchestrator::auto_dream` swapped to
  `Mutex<HashMap<String, Arc<dyn AutoDreamHook>>>` keyed by owning
  `agent_id` (option (a) from the original brainstorm).
  `Goal::with_agent_id` / `Goal::agent_id` helpers establish
  `metadata["agent_id"]` as the canonical routing-key convention
  so no breaking schema change to `Goal` was needed.
  `DreamContext.agent_id` field added so runners receive the
  resolved key. Per-turn dispatcher reads the key from goal
  metadata, looks it up, dispatches the matching runner. New API:
  `register_auto_dream` (returns displaced hook),
  `unregister_auto_dream`, `auto_dream_agents` (sorted),
  `has_auto_dream`. Boot wire in `src/main.rs::Mode::Run` now
  iterates every active runner and registers it under its
  `agent_id`. Compat shim `set_auto_dream(Option<...>)` retained
  behind `#[deprecated]`, routes to sentinel `"_default"` key
  with warn-once. Coverage: 5 integration tests in
  `crates/driver-loop/tests/orchestrator_auto_dream_registry_test.rs`
  plus 4 unit tests in `Goal::with_agent_id` / `agent_id()`.
  - Open follow-ups now de-scoped from this rollout:
    - Hot-reload propagation when an agent's `auto_dream.enabled`
      flips at runtime (Phase 18 reload loop should call
      `register_auto_dream` / `unregister_auto_dream`).
    - Lifecycle event for admin-ui so the operator can observe
      registered runners without scraping logs.
    - Prometheus gauge for `auto_dream_agents.len()`.

- _(closed)_ MS-3 placeholder removed — see `5fe2cc0`
  - `nexo_dream::boot::BootDeps` already accepts
    `pre_dream_snapshot: Option<Arc<dyn PreDreamSnapshotHook>>` +
    `pre_dream_tenant: String`, and `build_runner` threads them
    via `with_pre_dream_snapshot` / `with_pre_dream_tenant`. The
    binary has not yet wired `build_runner` into `Mode::Run` (the
    doc-comment in `crates/dream/src/boot.rs:18-37` is the
    intended hookup but is not implemented yet — it is part of
    Phase 80.1.b.b.b backlog, not Phase 36.2).
  - When that consumer lands, attach the snapshot adapter via:
    ```rust
    pre_dream_snapshot: snapshot_yaml.auto_pre_dream
        .then(|| memory_snapshotter.clone()
            .map(|s| nexo_memory_snapshot::PreDreamSnapshotAdapter::new(s)
                .into_arc()))
        .flatten(),
    pre_dream_tenant: "default".into(),
    ```
  - Effort: half day on the dream side, but the parent
    `BootDeps` consumer commit owns the full surgery.

### Phase 81 — Plug-and-Play Plugin System

**Goal**: convertir el modelo "Rust crate + boot wire en main.rs"
en plug-and-play real. Operator drops crate → daemon registry
descubre + wirea + corre. Cero edición de `src/main.rs`, cero
coordinación de archivos cross-cutting.

- **81.1 ✅ shipped 2026-04-30** — `nexo-plugin-manifest` crate.
  Foundation. TOML schema + 4-tier defensive validator + 25
  tests verde. `crates/plugin-manifest/` ~860 LOC. Reference
  manifest `examples/marketing-example.toml` documenta cada
  sección. Operator authors plugins escriben `nexo-plugin.toml`
  declarativo; futuras sub-fases consumen este schema.
- **81.2 ✅ shipped 2026-04-30** — `NexoPlugin` async trait +
  `PluginInitContext` + lifecycle errors en
  `nexo-core::agent::plugin_host`. ~470 LOC + 8 tests verde.
  Trait: `manifest()` + `init(ctx)` + `shutdown()` (default Ok).
  Context exposes 11 handles: ToolRegistry,
  Arc<RwLock<AdvisorRegistry>>, HookRegistry, AnyBroker,
  LlmRegistry, ConfigReloadCoordinator, SessionManager,
  Option<Arc<LongTermMemory>>, config_dir/state_root paths,
  CancellationToken. Helpers `plugin_config_dir(id)` +
  `plugin_state_dir(id)`. `PluginInitError` 5 variants +
  `PluginShutdownError` 2 variants thiserror-typed.
  `DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT = 5s`. Compile-time dyn-safety
  via `static _OBJECT_SAFE_CHECK: OnceLock<Arc<dyn NexoPlugin>>`.
  Distinct del existing Channel `Plugin` trait. Provider-
  agnostic. `nexo-core` Cargo.toml ganó `nexo-plugin-manifest`
  + `nexo-driver-permission` deps.
- **81.3 ⬜** Tool namespace runtime enforcement at boot.
- **81.4 ⬜** Plugin-scoped config dir loader
  (`config/plugins/<id>/*.yaml` auto-read).
- **81.5 ✅ shipped 2026-05-02** — `nexo_core::agent::nexo_plugin_registry`
  module: `discover()` filesystem walker (max_depth=2, manifest fixed
  at `<plugin_dir>/nexo-plugin.toml`), `NexoPluginRegistry` ArcSwap-
  backed snapshot container, `PluginDiscoveryConfig` YAML loaded from
  `<config_dir>/plugins/discovery.yaml`, typed `DiscoveryDiagnostic`
  enum (10 kinds). Reuses `nexo-plugin-manifest::PluginManifest` +
  `validate::run_all`. 16 unit + 1 integration test. Library-only ship:
  boot wire in `src/main.rs::Mode::Run` + `nexo agent doctor plugins`
  CLI deferred to 81.6 (will land alongside `NexoPlugin::init()`).
- **81.6 ✅ shipped 2026-05-02** — `merge_plugin_contributed_agents`
  fn in `nexo_core::agent::nexo_plugin_registry::contributes` walks
  each loaded plugin's `agents.contributes_dir`, parses YAMLs, folds
  into `AgentsConfig` honoring operator-priority + per-plugin
  `allow_override` flag. Conflict detection emits typed
  `MergeResolution { OperatorWins / PluginOverrideAccepted /
  LastPluginWins }`. Attribution sidecar map (`agent_id ->
  plugin_id`) instead of touching `AgentConfig` schema.
  `run_plugin_init_loop` async sequential driver records
  `InitOutcome { Ok / Failed / NoHandle }`. `PluginDiscoveryReport`
  extended with `contributed_agents_per_plugin` +
  `agent_merge_conflicts` + `init_outcomes` (all `#[serde(default,
  skip_serializing_if = ...is_empty)]` for backward-compat with 81.5
  consumers). 8 unit + 1 integration test. **Library-only ship**:
  boot wire in `src/main.rs::Mode::Run` + `nexo agent doctor plugins`
  CLI subcommand (also deferred from 81.5) lands alongside 81.7
  manifest-driven `NexoPlugin` instantiation that populates the
  handles map.
- **81.7 ⬜** Plugin-side `skills_dir` contribution.
- **81.8 ⬜** `ChannelAdapter` trait extension point para nuevos
  channel kinds (SMS, Discord, custom webhook).
- **81.9 ⬜** `Mode::Run` registry sweep — reduce ~500 LOC boot
  wire a ~30 LOC iteration. Critical milestone.
- **81.10 ⬜** Plugin hot-load via Phase 18 reload coord.
- **81.11 ⬜** Plugin doctor + capability inventory integration.
- **81.12 ⬜** Existing-plugin migration
  (whatsapp/telegram/email/browser → `NexoPlugin` impls).
- **81.13 ⬜ DEFER** Reference plugin template
  (`nexo plugin new <name>` CLI) + docs +
  `crates/plugins/sales-agent/` reference example.

Critical path: 81.1 → 81.2 → 81.5 → 81.9 (~3 días). Después
de 81.9 plugin model is fully operational; 81.10-81.13 son
polish + ergonomics.

### Audit 2026-04-30 — Phase 76/77/79 backlog

Source: `proyecto/AUDIT-2026-04-30.md` (audit of commits
`7619fee..96c53fb`, ~22 commits, ~+18 K LOC). Workspace compiles
clean (`cargo check --workspace --all-features` → 0). Three
recurring patterns of gap surfaced — ordered here by severity.

**A1 — C1 EffectiveBindingPolicy extension** — ✅ shipped 2026-04-30
(commit `d1f7641`). Tracked in detail under `H-2` in the Hardening
section below. The struct now resolves `lsp` / `team` /
`config_tool` / `repl` per binding; consumers in `src/main.rs`
still read agent-level (blocked by A2).

**A2 — C2 Hot-reload rebuild of per-binding tool registrations** —
⬜ open (depends on A1). Tracked under `H-3` in Hardening below.
Phase 18 promise broken until shipped: every Phase 79 tool
registers once at boot in `src/main.rs:2042-2705`; only one
post-hook exists today (`PairingGate` flush at `:3492`, Phase 70.7).

**A3 — C3 capabilities.rs::INVENTORY drift** — ✅ shipped 2026-04-30
(commits `5d5c6a7`, `4f8aced`, `91ebb19`). 3 entries added (one of
each category — env Boolean, env Boolean low-risk, Cargo feature)
plus a regex-based drift-prevention test that surfaced 13
previously-undocumented env reads (all classified as benign — see
the commit body for the breakdown).
Scope shipped:
- `ToggleKind::CargoFeature(&'static str)` variant added to support
  compile-time gates alongside runtime env-var toggles. Limitation
  documented: the `cfg!(feature = "X")` check evaluates against
  `nexo-setup`'s flag state, so any new feature must propagate to
  `crates/setup/Cargo.toml::[features]` (workspace pattern, already
  followed by `config-self-edit`).
- `evaluate_one` short-circuits for `CargoFeature`; `render_tty`
  shows "enabled (compiled-in)".
- 3 INVENTORY entries:
  * `CHAT_AUTH_SKIP_PERM_CHECK` (auth, High) — bypass file-perm
    gauntlet on secrets dir. Provider-agnostic.
  * `NEXO_CLAUDE_CLI_VERSION` (llm-anthropic, Low) — Anthropic
    OAuth Bearer CLI version stamp override. Provider-specific.
  * `config-self-edit` Cargo feature (core, Critical) — gates the
    self-config-editing ConfigTool. Provider-agnostic.
- Module doc-comment expanded with provider-agnostic clause naming
  the expected `extension` values for every LLM provider (Anthropic,
  MiniMax, OpenAI, Gemini, DeepSeek, xAI, Mistral, future) plus
  `core` / `auth` / `plugin-*`.
- Drift test `inventory_covers_known_dangerous_envs` walks
  `crates/**/*.rs` regex-matching `env::var("UPPER_NAME")` literals
  and asserts each is classified.
- `NON_DANGEROUS_ENV_ALLOWLIST` structured by category with explicit
  classification rule (version pin / cache / routing → allowlist;
  insecure-tls / skip-ratelimit / allow-write → INVENTORY; credential
  lookup → allowlist), reserved-for-future-providers section.
Limitations + follow-ups:
- `is_cargo_feature_enabled` requires a hard-coded match arm per
  feature. A missing arm falls through to `_ => false` — partially
  detected by `inventory_cargo_features_have_arms` but not fully.
  Cultural mitigation: dev who adds an INVENTORY CargoFeature entry
  also adds the arm.
- CI grep workflow that fails PRs introducing unclassified env reads
  is **deferred** as opt-in follow-up. The unit test cumple la
  función localmente.
- Auto-doc generation (Markdown table from INVENTORY) deferred.
References (validation, not copy):
- claude-code-leak `src/utils/envUtils.ts:32-47` — `isEnvTruthy`
  helpers without master registry, ~160 scattered `CLAUDE_*` vars.
- claude-code-leak `src/commands/doctor/` — UI-hardcoded surface,
  not generated from a registry.
- research/ `src/agents/auth-profiles/doctor.ts:15-42` —
  auth-only doctor, no toggle enumeration.
Implementation 100% Rust idiomatic: `cfg!`, const slice with
`&'static str`, `walkdir + regex` (workspace deps), no YAML registry
(per the module's source-of-truth-is-code design from inception).

**A4 — C4 Orphaned safety modules (Phase 77.9 / 77.10 / 77.11)** —
🟡 partially shipped. Slice C4.a done; C4.b/C4.c remain open.

**A4.a — sed_validator + path_extractor wire** — ✅ shipped
2026-04-30 (C4.a). `gather_bash_warnings` (`crates/driver-permission/
src/mcp.rs:190-260`) now composes 4 advisory tiers:
1. destructive command, 2. sed in-place shallow,
3. **sed deep validator** (gated on first token == `sed`,
calls `sed_validator::sed_command_is_allowed(cmd, false)`,
catches `e` (exec) / `w` (file-write) flags), 4. **path
extractor** (lists up to 10 paths the command touches with
the matching `PathCommand::action_verb()`). All tiers stay
advisory — final allow/deny rides on the upstream LLM
decider, which is now provider-agnostic across Anthropic /
MiniMax / OpenAI / Gemini / DeepSeek / xAI / Mistral.
4 inline tests in `mcp::tests` cover the wire (skip-non-bash,
simple-sed-no-fp, complex-sed-flagged, path-list).
Doc-comment on `gather_bash_warnings` documents the 4-tier
composition with IRROMPIBLE refs to claude-code-leak
`bashSecurity.ts`/`sedValidation.ts:247-301`/
`pathValidation.ts:27-509`.

**A4.b — should_use_sandbox heuristic wire** — ✅ shipped
2026-04-30 (advisory MVP). `gather_bash_warnings`
(`crates/driver-permission/src/mcp.rs:204-360`) gained a 5th
tier coupled to risk: fires only when at least one prior tier
(destructive / sed-shallow / sed-deep / path-extractor) already
flagged the command AND `SandboxProbe` detected `bwrap` or
`firejail` on PATH. Probe is process-wide via
`static SANDBOX_PROBE: OnceLock<SandboxProbe>` — runs `which
bwrap` + `which firejail` once per process and caches the
backend. Coupling to risk is intentional: leak's
`should_use_sandbox(_, Auto, Some_backend, false, [])` returns
`true` for ANY command, so firing alone would emit advisory on
every Bash call on a sandbox-equipped host. Coupling to
existing warnings keeps the signal-to-noise ratio high.
Refactor split: `pub gather_bash_warnings(tn, i)` resolves the
static probe and delegates to internal
`gather_bash_warnings_with_backend(tn, i, sandbox_backend)`
which accepts the backend explicitly so tests inject
`SandboxBackend::Bubblewrap` / `Firejail` / `None`
deterministically without hitting `which` on the test host.
3 inline tests:
`gather_bash_warnings_appends_sandbox_advisory_when_risky_and_backend_available`,
`gather_bash_warnings_skips_sandbox_when_no_backend`,
`gather_bash_warnings_skips_sandbox_when_no_other_warnings`.
Doc-comment now lists 5 tiers + IRROMPIBLE refs to
claude-code-leak `shouldUseSandbox.ts:130-153` (pure decision
shape) and `:55-58` (`excludedCommands` is "not a security
boundary" disclaimer). Provider-agnostic: probe + decision
operate on command string + PATH, no LLM provider touchpoint.
Tests: `cargo test -p nexo-driver-permission --lib
gather_bash_warnings` → 7/7 (4 from C4.a + 3 new).

**C4.b.b — YAML config schema** — ⬜ open. `runtime.bash_safety.
sandbox.{mode, excluded_commands, dangerously_disable}` config
fields + per-binding override + plumb into the helper. MVP
hard-codes `Mode::Auto` / empty excluded list / `disable=false`
so operators today get advisory whenever bwrap/firejail is
installed and no granular control. Adding the schema needs
defensive validation (mode enum tag), Phase 18 hot-reload
re-validation, and admin-ui surface (Phase A8). Effort:
~half day. Defers also fixed-point env/wrapper stripping
(`stripAllLeadingEnvVars` + `stripSafeWrappers`) which only
matters once `excluded_commands` exists.

**A4.c — rate_limit_info → LlmError::QuotaExceeded** — ✅ shipped
2026-04-30. `crates/llm/src/retry.rs` gained
`LlmError::QuotaExceeded { retry_after_ms, severity, message,
plan_hint, provider, window }` plus the `pub fn
classify_429_error(retry_after_ms, info)` helper that promotes
429s to `QuotaExceeded` when `RateLimitInfo.status == Rejected`
AND `format_rate_limit_message` produces a message; otherwise
returns the legacy `LlmError::RateLimit { retry_after_ms,
rate_limit_info }`. Promotion fires the `record_quota_event`
side-effect into a process-wide `static LAST_QUOTA:
OnceLock<DashMap<LlmProvider, QuotaEvent>>` so
`last_quota_events_all()` reads cleanly from
`setup doctor`. `with_retry` short-circuits on
`QuotaExceeded` (no retry, propagate immediately) — leak's
3-tier 429 model from `services/api/errors.ts:465-548` mapped
to our advisory pipeline. Wired in 4 provider sites:
- `anthropic.rs:381` — already extracted Anthropic info,
  swap to helper.
- `openai_compat.rs:81` — wire `extract_openai_compat_headers`
  (covers OpenAI / xAI / DeepSeek / Mistral via shared
  `x-ratelimit-*` shape).
- `gemini.rs:95` — wire `extract_gemini_headers`.
- `minimax.rs:228` chat path + `:280` finish path — wire
  `extract_openai_compat_headers` (MiniMax speaks OpenAI-compat).
`setup doctor` renders an "LLM quota" section iterating
`last_quota_events_all()`, marking each event with severity
icon + age in minutes + plan_hint when present. 9 tests added:
5 in `retry.rs::tests`
(`quota_exceeded_promoted_when_status_rejected`,
`rate_limit_kept_when_status_allowed_warning`,
`rate_limit_kept_when_no_info`,
`with_retry_does_not_retry_quota_exceeded`,
`quota_exceeded_display_includes_provider_label`) and 4 in
`rate_limit_info.rs::tests`
(`record_quota_event_is_visible_via_last_quota_event_for`,
`last_quota_events_all_returns_one_per_provider`,
`extract_openai_compat_headers_promotes_to_quota_exceeded`,
`extract_gemini_headers_promotes_to_quota_exceeded`).
`LlmProvider` gained `Hash` derive so it can key the cache
DashMap. Provider-agnostic across Anthropic / OpenAI / Gemini
/ MiniMax / Generic (xAI / DeepSeek / Mistral compat-mode).
IRROMPIBLE refs in doc-comment: leak `services/api/errors.ts:465-548`
(3-tier 429 classification), `services/rateLimitMessages.ts:45-104`
(`getRateLimitMessage` ported as `format_rate_limit_message`).
Tests: `cargo test -p nexo-llm --lib` → 167/167 (158 existing +
9 new).

**C4.c.b — notify_origin wire from agent runtime** — ⬜ open.
The catch site for `LlmError::QuotaExceeded` in
`crates/core/src/agent/llm_behavior.rs` should fire
`notify_origin` with the `message + plan_hint` payload so
operators see the quota-exceeded event in their pairing channel
(WhatsApp / Telegram / etc.) without needing to run `setup
doctor`. Needs a `HookDispatcher` handle threaded into the
catch path; bigger surgery. Defer: shipping the variant +
cache + setup-doctor surface (this slice) covers 2 of 3 audit
asks; notify_origin is the third.

**C4.c.c — admin-ui A8 quota panel + Prometheus metric** —
⬜ open. `nexo_llm::rate_limit_info::last_quota_events_all`
already provides the data shape; admin-ui Phase A8 reads it
and renders a per-provider widget. Prometheus gauge
`nexo_llm_quota_exceeded_total{provider="anthropic"}` lands
alongside Phase 9.2 metrics.

**C4.c.d — Anthropic-specific entitlement-reject hint** —
⬜ open. Leak `errors.ts:540-548` carves out
`Extra usage is required for long context` and prints a
model-switch suggestion. Defer until a multi-provider
entitlement-reject case appears (today only Anthropic).

**A5 — C5 SecretGuardConfig YAML never read** — ✅ shipped 2026-04-30
(commits `32d74f2`, `56053cf`, `b6cea87`). Operators now control the
secret-scanner via `memory.secret_guard` in `config/memory.yaml`
(4 knobs: `enabled`, `on_secret: block|redact|warn`, `rules: "all" |
[rule_id...]`, `exclude_rules: [rule_id...]`). Schema lived in
`crates/memory/src/secret_config.rs` since Phase 77.7.

**Pivot from spec**: a direct `nexo-config -> nexo-memory` dep
would form a cycle (`nexo-llm -> nexo-config -> nexo-memory ->
nexo-llm`). Fix uses a wire-shape struct (`SecretGuardYamlConfig`)
in `crates/config/src/types/memory.rs` that mirrors the canonical
`nexo_memory::SecretGuardConfig` schema 1:1; the conversion lives
in `src/main.rs::build_secret_guard_config_from_yaml` (binary holds
both deps). Doc-comment on the wire-shape struct explicitly flags
the dual-write contract: when the schema changes, update BOTH
files.

Sites covered:
- `src/main.rs:837-845` (daemon path) — direct read from `cfg`.
- `src/main.rs:8723-8753` (mcp-server path) — restructured: the
  secret guard now reads from the same `mem_cfg` that the rest of
  the mcp-server bootstrap loads via `load_optional`. Default
  applies when memory.yaml is absent or the `secret_guard` key is
  omitted (best-effort tolerance preserved).
- 2 round-trip tests in `crates/config/tests/load_test.rs` cover
  default-secure (omitted key) + warn-with-excludes (override).
- `docs/src/ops/memdir-scanner.md` extended with full
  Configuration section + table + provider-agnostic clause + IRROMPIBLE
  prior-art citations.

Provider-agnostic — `exclude_rules` operates on rule IDs (kebab-case
like `github-pat`, `aws-access-token`, `openai-api-key`), not on
providers. Scanner covers Anthropic / MiniMax / OpenAI / Gemini /
DeepSeek / xAI / Mistral with the same regex set.

References (validation, not copy):
- claude-code-leak `src/services/teamMemorySync/secretScanner.ts:48,
  596-615,312-324` — hardcoded, no YAML knob. We do better.
- research/ `src/config/zod-schema.ts` — OpenClaw 2-value enums.
  We extended to 3 (block/redact/warn).

**Limitation**: schema duplication between `nexo-config` (wire
shape) and `nexo-memory` (domain). Acceptable cost for breaking
the dep cycle; doc-comment + the dual-test arrangement
(secret_config.rs unit tests + load_test.rs integration tests)
catch drift. Migration to a shared `nexo-config-types` crate
would eliminate this — deferred as A5.b.

**A6 — Major findings (M1–M10)** — ⬜ open, batched here so they
do not get lost.
- **M1 — `tools/list_changed` advertised disabled.** 🟡 partial.
  Slice **M1.a — capability + hot-swap allowlist** ✅ shipped
  2026-04-30 (commit `dba4156`'s successor for M1). Bridge struct
  (`crates/core/src/agent/mcp_server_bridge/bridge.rs:85-200`)
  now holds `allowlist: Arc<ArcSwap<Option<Arc<HashSet<String>>>>>`
  (hot-swap via `swap_allowlist(new)` — atomic, all clones
  observe the new set immediately because they share the
  `Arc<ArcSwap>`) and `list_changed_capability: bool` (default
  `false`; opt-in via `with_list_changed_capability(true)`).
  `capabilities()` reads the flag instead of hard-coding `false`.
  HTTP path (`src/main.rs::start_http_transport`) clones the
  bridge with cap=true so HTTP clients register the
  `tools/list_changed` notification handler per leak
  `useManageMCPConnections.ts:618-665`. Stdio path keeps
  cap=false because stdio cannot push server→client
  notifications mid-session (no bidir transport channel today).
  5 inline tests cover capability defaults, builder flip,
  swap visibility, clone propagation, proxy filter
  invariance. **Slice M1.b.c — daemon-embed MCP HTTP server**
  ✅ shipped 2026-04-30. `Mode::Run` (daemon) now optionally
  starts an MCP HTTP server in-process alongside the agent
  runtime, exposing the primary agent's tools (mirror of
  `nexo mcp-server` standalone). `crates/config/src/types/
  mcp_server.rs` gains `McpServerDaemonEmbedConfig { enabled:
  bool }` + `McpServerConfig.daemon_embed` field with
  `#[serde(default, deny_unknown_fields)]` (back-compat
  preserved — default false → no MCP server in daemon).
  `src/main.rs::Mode::Run` adds `compute_allowlist_from_mcp_server_cfg`
  helper + boot wire just before `reload_coord.start(...)`:
  captures primary agent id+config pre-loop (since the loop
  consumes `cfg.agents.agents`), looks up the primary's
  `Arc<ToolRegistry>` from `tools_per_agent`, builds
  `AgentContext` + `ToolRegistryBridge` with M1.a's
  `with_list_changed_capability(true)`, validates `http.enabled`
  + bails on inconsistent config, calls `start_http_transport`
  to bring up the HTTP server, then registers a reload-coord
  post-hook that re-reads `mcp_server.expose_tools` from disk,
  atomically swaps the bridge allowlist via
  `swap_allowlist(new)`, and emits
  `notify_tools_list_changed()` so connected Claude Desktop /
  Cursor clients refresh tool list automatically on every
  Phase 18 reload — **no SIGHUP required**. `mcp_embed_handle`
  drained on shutdown with 5s timeout. SIGHUP refactored to
  sync (helper was async-but-not-actually); 3 existing helper
  tests adapted from `#[tokio::test]` to `#[test]`. 3 new
  inline tests for `compute_allowlist_from_mcp_server_cfg`:
  `compute_allowlist_returns_set_from_expose_tools`,
  `compute_allowlist_returns_none_for_empty`,
  `compute_allowlist_dedupes_via_hashset`. Doc-comment cites
  `nexo mcp-server` standalone as architectural mirror; same
  primary-agent-only behavior. **Operator UX**:
  ```yaml
  mcp_server:
    daemon_embed:
      enabled: true
    http:
      enabled: true
      bind: 127.0.0.1:8765
      auth: { kind: static_token, token_env: NEXO_MCP_TOKEN }
    expose_tools: [Read, Edit, marketing_lead_route]
  ```
  Boot `nexo run`, MCP server live alongside agents. Edit
  `expose_tools`, file watcher fires reload coord, post-hook
  swaps + notifies, clients refresh — zero downtime, zero
  SIGHUP. **Open follow-ups**: M1.b.c.b (per-agent endpoint
  `/mcp/agent_x` for multi-tenant routing), M1.b.c.c
  (multi-agent union endpoint with collision detection),
  M1.b.c.d (hot-swap primary agent identity mid-run — today
  bridge held for daemon life). Conflict path: running
  `nexo` daemon with embed + `nexo mcp-server` standalone
  with same port → second bind fails OS-level with
  EADDRINUSE; pick one path. Provider-agnostic across
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
  Mistral. Tests: `cargo test --bin nexo compute_allowlist`
  → 3/3, `cargo test --bin nexo reload_expose_tools` → 3/3,
  `cargo test -p nexo-config --lib` → 169/169,
  `cargo build --bin nexo` verde.

  **Slice M1.b — trigger** ✅ shipped 2026-04-30
  (SIGHUP MVP). `nexo-mcp` exposes new `pub struct
  HttpNotifyHandle { sessions: Arc<HttpSessionManager> }` (Clone)
  via `HttpServerHandle::notifier(&self) -> HttpNotifyHandle` so
  background tasks can call `notify_tools_list_changed()`
  without owning the `JoinHandle`. `src/main.rs::run_mcp_server`
  gained `reload_expose_tools(config_dir) -> Result<Option<HashSet>>`
  (re-reads `mcp_server.expose_tools` via
  `AppConfig::load_for_mcp_server`; empty list → `Ok(None)`,
  non-empty → `Ok(Some(set))`, parse error → `Err`) plus a
  `#[cfg(unix)]` SIGHUP handler tokio task that loops on
  `tokio::signal::unix::SignalKind::hangup()` + selects against
  `shutdown.cancelled()` for clean exit. On every SIGHUP: re-read
  YAML, `bridge.swap_allowlist(new)` (atomic, visible to all
  bridge clones via M1.a's `Arc<ArcSwap>`), then
  `notifier.notify_tools_list_changed()` if HTTP transport up.
  Operator UX: `kill -HUP $(pidof nexo)` after editing
  `mcp_server.yaml` → connected Claude Desktop / Cursor refresh
  tool list without reconnect. Atomic swap-then-notify order
  prevents the race where clients refetch before swap completes.
  YAML parse failure → log warn, last-known-good allowlist
  preserved (no broken state). Burst SIGHUPs — multiple swaps +
  notifications, client-side debounces 200 ms (per leak
  `useManageMCPConnections.ts:721-723`). Non-Unix path logs
  warn-once + skip (Windows operators restart for changes).
  3 inline helper tests
  (`reload_expose_tools_returns_set_from_yaml`,
  `reload_expose_tools_returns_none_for_empty_list`,
  `reload_expose_tools_propagates_yaml_parse_errors`). Tests:
  `cargo test --bin nexo reload_expose_tools` → 3/3,
  `cargo build --bin nexo` verde. **Slice M1.b.b ⬜** open:
  cross-platform file watcher (`notify` crate) +
  `ConfigReloadCoordinator` integration when the daemon `Mode::Run`
  also exposes the MCP server in-process (today only standalone
  `nexo mcp-server` subcommand has the bridge). **Slice M1.c —
  stdio notification pump ⬜** open: would let stdio path also
  flip cap=true; needs an `mpsc::Sender<Value>` injected into
  `run_with_io_auth` so external code can push notification
  frames into the stdout writer. Defer until M1.b lands and
  measures whether stdio operators actually need it (most
  stdio deploys are single-process per tool invocation).
  Provider-agnostic across Anthropic / MiniMax / OpenAI /
  Gemini / DeepSeek / xAI / Mistral — protocol-MCP, no LLM
  provider assumption. Already tracked as **79.M.h** in this
  file; cross-reference still applies for daemon in-process
  hot-reload wire.
- **M2 — MCP audit `args_size_bytes` + `args_hash` always 0/None.**
  ✅ shipped 2026-04-30 (commits `9417423`, `279e2ce`, `0191ea9`).
  Discovery surfaced that the infra was already in place
  (`AuditLogConfig::{redact_args, per_tool_redact_args,
  args_hash_max_bytes}` schema validated, SQLite columns mapped) —
  only the compute at `dispatch.rs:706-707` was missing. New
  `audit_log/hash.rs` module exposes
  `args_hash_truncated(&[u8]) -> String` (sha256 → 16 lowercase hex
  chars / 64 bits, manual hex format avoids `hex` crate dep on
  `mcp`) and `compute_args_metrics(&Value, &AuditLogConfig, &str)
  -> (Option<String>, u64)` (single-serialize, applies all 3 config
  knobs). Truncation length matches prior art (claude-code-leak
  `hashMcpConfig`, `pasteStore`, `fileOperationAnalytics`,
  `fileHistory`, `pluginTelemetry` — all `slice(0, 16)`).
  Provider-agnostic — operates on the MCP wire envelope, regardless
  of which LLM client (Claude Desktop / Cursor / Continue / Cody /
  Aider) or backing provider (Anthropic / MiniMax / OpenAI / Gemini
  / DeepSeek / xAI / Mistral) drives the call. Tests: 9 unit (8
  planned + 1 provider-agnostic regression that exercises 4
  provider-shaped JSON envelopes) + 2 integration in
  `audit_log_e2e_test.rs` (happy path + `redact_args=false`
  opt-out). cargo test -p nexo-mcp green (358 lib + 5 audit e2e).
  SQLite schema unchanged — back-compat 100%.
- **M3 — `proactive` ⊕ `coordinator` mutual exclusion not enforced.**
  ✅ shipped 2026-04-30. `BindingValidationError::CoordinatorWithProactive`
  now fires from `validate_agent()` (`binding_validate.rs:407-433`)
  when `role = "coordinator"` and the resolved `proactive.enabled`
  (binding override or agent default) is `true`. 4 unit tests
  cover the agent-level + binding-override paths plus the two
  happy paths.
- **M4 — `extractMemories` + `autoCompact` only run inside
  `driver-loop`.** 🟡 partial. Slice **M4.a — extractMemories
  shared service** ✅ shipped 2026-04-30. New trait
  `nexo_driver_types::MemoryExtractor` (`crates/driver-types/
  src/memory_extractor.rs`) with `tick(&self)` + `extract(
  self: Arc<Self>, goal_id, turn_index, messages_text,
  memory_dir)`. Mirrors `AutoDreamHook` (Phase 80.1.b) cycle-
  break pattern: declared upstream of both `nexo-core` and
  `nexo-driver-loop` so they hold `Arc<dyn MemoryExtractor>`
  without depending on each other. `nexo-driver-loop` ships
  `impl MemoryExtractor for ExtractMemories` re-using the
  inherent `tick` + `extract` methods. `LlmAgentBehavior`
  (`crates/core/src/agent/llm_behavior.rs`) gains
  `memory_extractor: Option<Arc<dyn MemoryExtractor>>` +
  `memory_dir: Option<PathBuf>` + builder
  `pub fn with_memory_extractor(mut self, extractor, dir)
  -> Self`. Post-turn hook (just before
  `Ok(RunTurnOutcome::Reply(reply_text))` at `:1707`) calls
  `extractor.tick()` always; calls `extract(GoalId(session_id),
  0, text, dir)` only when both `memory_dir` is Some AND
  `reply_text` is Some — defensive: no writes outside an
  explicit dir, no extraction without an assistant turn.
  `turn_index = 0` is an MVP sentinel (regular AgentRuntime
  doesn't track per-session turn counters; defer to M4.c).
  3 inline tests in `agent::llm_behavior::tests`:
  `with_memory_extractor_populates_both_fields`,
  `default_behavior_has_no_memory_extractor`,
  `memory_extractor_records_tick_and_extract_calls`. Provider-
  agnostic: `Arc<dyn MemoryExtractor>` keeps any concrete impl
  pluggable (today `ExtractMemories` from `nexo-driver-loop`,
  carrying `Arc<dyn LlmClient>` upstream — works under
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
  Mistral). IRROMPIBLE refs in trait doc-comment to
  claude-code-leak `services/extractMemories/extractMemories.ts:121-148`
  (`hasMemoryWritesSince` cadence semantics) and `QueryEngine.ts`
  (single-turn-engine extract trigger our two engines now share
  via the trait). `research/` no relevant prior art.
  Cumulative tests: `cargo test -p nexo-driver-types` verde,
  `cargo test -p nexo-driver-loop --lib` verde (21 ExtractMemories
  + impl — same tests),
  `cargo test -p nexo-core --lib agent::llm_behavior::tests`
  → 9/9 (6 existing + 3 new).
  **Slice M4.a.b — boot wire** ✅ shipped 2026-04-30.
  `crates/config/src/types/agents.rs` gained
  `extract_memories: Option<ExtractMemoriesYamlConfig>` —
  wire-shape struct mirroring `nexo_driver_types::
  ExtractMemoriesConfig` 1:1. Wire-shape pattern (precedent:
  `SecretGuardYamlConfig` from C5) avoids the cycle that
  `nexo-config -> nexo-driver-types` would create
  (`nexo-driver-types` already depends on `nexo-config`).
  `crates/driver-loop/src/extract_memories.rs` ships
  `LlmClientAdapter { llm: Arc<dyn LlmClient>, model: String }`
  with `impl ExtractMemoriesLlm`. The adapter packages the
  prompt + transcript into `ChatRequest`, calls the upstream
  LLM, and pulls the first `ResponseContent::Text` block;
  `ResponseContent::ToolCalls` returns a clear error.
  `src/main.rs` gained `resolve_extract_memory_dir(agent_cfg)`
  helper (workspace-derived when set, else
  `<state_root>/<agent_id>/memory/`) and the agent-loop boot
  wire just after `let llm = ...`: when
  `agent_cfg.extract_memories.is_some_and(|c| c.enabled)`, the
  loop converts the YAML to `ExtractMemoriesConfig`,
  constructs `LlmClientAdapter` + `Arc<ExtractMemories>`, and
  injects via `LlmAgentBehavior::with_memory_extractor` after
  `mkdir -p` of the dir (warn-and-continue on dir create
  failure). 2 inline driver-loop tests
  (`llm_client_adapter_chat_round_trips`,
  `llm_client_adapter_errors_on_tool_call_response`) and 3
  config tests (`agent_config_yaml_without_extract_memories_parses`,
  `agent_config_yaml_with_extract_memories_parses`,
  `extract_memories_default_disables`). 50-fixture sweep added
  `extract_memories: None,` after `assistant_mode: None,` in
  every existing `AgentConfig { ... }` literal — same
  mechanical pattern used for the Phase 80.15 `assistant_mode`
  sweep. Provider-agnostic: adapter operates on
  `Arc<dyn LlmClient>` so behaviour is identical across
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
  Mistral. Marketing plugin path now ready: opt-in via
  `extract_memories: { enabled: true }` in `agents.yaml`,
  agent processes inbound emails → reply → post-turn extract
  fires → memory persists in `<workspace>/memory/<auto>.md`.
  Tests: `cargo test -p nexo-config --lib` 163/163,
  `cargo test -p nexo-driver-loop --lib` 106/106 (104
  existing + 2 new),
  `cargo test -p nexo-core --lib` 687/687 (sweep clean),
  `cargo build --bin nexo` verde. **Slice M4.b — autoCompact in regular
  AgentRuntime ⬜** open. Bigger surgery: requires session-
  history-replace flow + LlmCompactor wire dentro del turn
  loop. Effort: ~half day. **Slice M4.c — per-session turn
  counter ⬜** open. Replaces `turn_index = 0` sentinel with
  real per-session count. Trivial once `Session` carries the
  counter (most likely already does — verify).
- **M5 — `cron_tool_bindings` frozen at boot.**
  `src/main.rs:3052-3128` captures `Arc::clone(&effective)` once.
  Reload changing `allowed_tools` / `dispatch_policy` for an
  agent → cron firings keep the OLD policy. Fix: post-hook flush
  analogous to PairingGate (`:3492`). Effort: ~1 hr. **Folds
  naturally into A2 (C2)**.
- **M6 — `PostCompactCleanup` is a stub + `CompactSummaryStore::
  forget()` is no-op.** `crates/driver-loop/src/post_compact_cleanup.rs:38-48`
  only ticks the extract counter. Leak's `postCompactCleanup.ts`
  resets MicroCompactState turn counter, surfaced-memory caches,
  `compactWarningState`. `compact_store.rs:68-74` `forget()` is a
  TODO. Effort: ~1 hr to mirror leak.
- **M7 — REPL semantically diverges from leak (Phase 79.12).**
  Leak `claude-code-leak/src/tools/REPL/primitiveTools.ts:21-39`
  makes REPL a VM hosting FileRead/FileWrite/FileEdit/Glob/Grep/
  Bash/NotebookEdit/Agent. Our `repl_registry.rs:59-90` is a
  subprocess pool for python/node/bash. No sandbox isolation,
  no nsjail/firejail/bwrap, `repl_tool.rs` itself has zero unit
  tests. **Decision required**: (a) re-spec as our own
  "Sandbox shell" tool and stop claiming leak parity, or
  (b) commit to porting the VM model. Default recommendation:
  (a) — bash + per-language Bash variants is enough for our
  use cases.
- **M8 — Phase 79.2 deferred-schema only used by MCP catalog.**
  ✅ shipped 2026-04-30 (M8.a slice). New module
  `crates/core/src/agent/built_in_deferred.rs` ships
  `BUILT_IN_DEFERRED_TOOLS: &[(&'static str, &'static str)]` —
  12 canonical `(name, search_hint)` entries for built-in tools
  that match leak's `shouldDefer: true` precedent: `TodoWrite`
  (per leak `TodoWriteTool.ts:51`), `NotebookEdit`
  (`NotebookEditTool.ts:94`), `RemoteTrigger`
  (`RemoteTriggerTool.ts:50`), `Lsp` (`LSPTool.ts:136`),
  `TeamCreate` (`TeamCreateTool.ts:78`), `TeamDelete`
  (`TeamDeleteTool.ts:36`), `TeamSendMessage` (per
  `SendMessageTool.ts:533` precedent), `TeamList` + `TeamStatus`
  (per `TaskListTool.ts:52` list/status precedent), `Repl`
  (local decision — verbose schema, rare use), `ListMcpResources`
  (`ListMcpResourcesTool.ts:50`), `ReadMcpResource`
  (`ReadMcpResourceTool.ts:59`). `pub fn
  mark_built_in_deferred(&ToolRegistry)` helper applies
  `ToolMeta::deferred_with_hint(...)` via `set_meta` (idempotent
  vs gated tools — entries not registered in this boot are
  silently skipped because `set_meta` only writes the
  side-channel meta). Single sweep call wired in
  `src/main.rs:3293-3303` after all `tools.register(...)` calls
  + after MCP registration + before binding validation, so the
  registry is fully assembled when the meta lands. 3 inline
  tests in `tool_registry::tests`:
  `mark_built_in_deferred_excludes_listed_tools`,
  `mark_built_in_deferred_skips_absent_tools`,
  `mark_built_in_deferred_propagates_search_hints`. Doc-comment
  on the module documents the cap+emit coupling rule + 9
  IRROMPIBLE refs to leak (`Tool.ts:438-449` shouldDefer/alwaysLoad
  semantics, `tools/ToolSearchTool/prompt.ts:62-108` decision
  tree, `services/api/claude.ts:1136-1253` token-budget rationale,
  per-tool `shouldDefer:` sites). Provider-agnostic across
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
  Mistral — deferral lives at the `ToolRegistry` layer, not in
  any provider shim. Tests:
  `cargo test -p nexo-core --lib agent::tool_registry::tests`
  → 19/19 (16 existing + 3 new). Note: binary build
  (`cargo build --bin nexo`) blocked by pre-existing dirty
  state from Phase 80.1.d (`nexo_dream` crate not in `Cargo.toml`,
  `DreamRunRow` lacks `Serialize`, `GoalId::as_uuid` removed) —
  M8 changes themselves are isolated, only nexo-core lib +
  `src/main.rs` single-line wire. **Slice M8.b ⬜** open:
  defer plan-mode tools (`EnterPlanMode` / `ExitPlanMode`)
  after re-evaluating mid-turn UX. **Slice M8.c ⬜** open:
  defer 5 cron tools (`CronCreate/List/Delete/Pause/Resume`)
  after Phase 80.2-80.6 cron jitter knobs settle.
  **Slice M8.d ⬜** open: defer `WebSearch` / `WebFetch` after
  Phase 21/25 surface stabilizes. **Provider-shim filtering wire
  ⬜** open: 4 LLM provider shims (anthropic / minimax / gemini /
  openai-compat) still emit the full schema today; the savings
  land when a follow-up wires them to consult
  `ToolRegistry::deferred_tools()`. M8.a ships the registry-side
  marking; the actual token-budget win lands when shims consume
  it (Phase 79.2 follow-up).
- **M9 — `expose_tools` typo path silent.**
  ✅ shipped 2026-04-30 (commit `895b99b`). New
  `crates/core/tests/expose_tools_typo_regression_test.rs`
  maintains a hardcoded `KNOWN_CANONICAL_NAMES_SNAPSHOT` (33
  entries baseline) bidirectionally synced with `EXPOSABLE_TOOLS`.
  Three tests:
  * `every_snapshot_name_resolves_via_lookup` — silent renames /
    removals fail loud with explicit fix paths.
  * `every_catalog_name_in_snapshot` — new catalog entries force
    snapshot update.
  * `snapshot_has_no_duplicates` — merge-conflict sanity.
  Pattern adopted from OpenClaw
  `research/src/channels/ids.test.ts:48-50` snapshot assertion;
  claude-code-leak `src/tools.ts:193-251` ships `getAllBaseTools()`
  without a snapshot test, validating the value of adding one.
  Provider-agnostic — `EXPOSABLE_TOOLS` is wire-spec MCP, indistinto
  de LLM client / provider.
  Limitación: regression guard CODE-side only. Operadores con YAML
  legacy referencing old name siguen viendo el `tracing::warn!`
  runtime al boot (`src/main.rs:9261-9269`). Follow-up **M9.b**
  open: deprecated-alias mechanism (`pub static DEPRECATED_ALIASES:
  &[(&str, &str)]` + `lookup_exposable` extended) preserves
  back-compat through deprecation cycles.
- **M10 — `MUTATING_TOOLS` lists `TeamCreate` / `TeamDelete`
  twice.** ✅ shipped 2026-04-30. Removed the first set of
  duplicates at `crates/core/src/plan_mode.rs:295-296`; the
  Phase 79.6 trio (`TeamCreate` / `TeamDelete` / `TeamSendMessage`)
  is now defined exactly once at `:312-316`. plan_mode tests
  green (70/70).
- **advisory_hook — generic tool advisory extension point** ✅
  shipped 2026-04-30. Generalizes `gather_bash_warnings`
  (Phase 77.8-10 + C4.a-b) into an extensible registry. New
  module `crates/driver-permission/src/advisor.rs` ships
  `pub trait ToolAdvisor { fn id(&self); fn advise(&self,
  tool_name, input) -> Option<String>; }` + `AdvisorRegistry`
  (Vec<Arc<dyn ToolAdvisor>>) with `new()` / `with_default()` /
  `register(...)` / `gather(...)` API. `gather` runs each advisor
  in registration order with `std::panic::catch_unwind`
  isolation (a buggy plugin cannot crash the permission flow —
  panics get `tracing::warn!` + skipped, others continue) and
  composes results into a unified `WARNING — tool advisories:\n
  - [<id>] <line>\n- [<id>] <line>` block (multi-line advisor
  output is split + each line re-prefixed). `BashSecurityAdvisor`
  wraps the existing `gather_bash_warnings` free fn (now
  `pub(crate)`) and strips the legacy `WARNING — bash security`
  prefix so the registry can re-wrap. `PermissionMcpServer`
  gains `advisors: Arc<AdvisorRegistry>` field (defaults to
  `with_default()` so back-compat preserved at the call-shape
  level — bash advisor pre-registered) plus
  `with_advisors(Arc<AdvisorRegistry>)` builder for plugins to
  override. Wire site at `mcp.rs::call_tool` swaps
  `gather_bash_warnings(...)` for `self.advisors.gather(...)`.
  6 inline tests in `advisor::tests` cover empty registry,
  single advisor with `[id]` prefix, multi-advisor join,
  silent advisor skip, panic isolation, and
  BashSecurityAdvisor's legacy-prefix strip.
  Plugin author surface example (informational —
  `nexo-plugin-marketing` will ship its own when constructed):
  ```rust
  pub struct MarketingAdvisor;
  impl ToolAdvisor for MarketingAdvisor {
      fn id(&self) -> &str { "marketing" }
      fn advise(&self, tool_name: &str, input: &Value) -> Option<String> {
          if tool_name == "marketing_lead_route" {
              let kind = input.pointer("/channel/kind")?.as_str()?;
              if kind == "crm" {
                  return Some("external API call to CRM (Hubspot); estimated cost $0.01".into());
              }
          }
          None
      }
  }
  ```
  Output prefix changed from `WARNING — bash security` to
  `WARNING — tool advisories` with per-line `[bash]` bracket —
  operator dashboards / log parsers that match the exact old
  string need updating (documented). All advisories stay
  advisory-only — upstream LLM decider remains authoritative
  allow/deny gate; plugins that want hard blocks integrate with
  `nexo-core::plan_mode::MUTATING_TOOLS`. Provider-agnostic:
  advisors operate on `(tool_name, input)`, no LLM-provider
  assumption. **Open follow-ups**: `advisory_hook.b` async
  trait variant for advisors that need DB/network lookup;
  `advisory_hook.c` per-binding advisor allowlist/disable
  granularity; `advisory_hook.d` Prometheus metrics. IRROMPIBLE
  refs: claude-code-leak `bashSecurity.ts` single-tier-class
  pattern (we generalize for plugins); `research/` no relevant
  prior art. Tests:
  `cargo test -p nexo-driver-permission --lib` → 170/170
  (164 pre-existing + 6 new).

**A7 — Minor / cosmetic (M-cosmetic)** — ⬜ open, batched.
- `crates/mcp/src/server/http_transport.rs:533-535` —
  `Box::leak` on retry-after header per 429. Slow leak (one
  allocation per rate-limit hit); use `Cow<'static, str>` or
  cache.
- `crates/mcp/src/server/event_store/sqlite_store.rs:195-203` —
  `purge_oldest_for_session` is a 3-bind correlated subselect;
  quadratic on the documented 10k cap; only 10-row test
  coverage. Rewrite to single DELETE + LIMIT after measuring.
- No test exists for `BearerJwt` mid-flight JWKS `kid`
  rotation or flapping endpoint.
- No real-provider-swap (Anthropic → MiniMax) round-trip test
  for cache-break cross-provider tracker
  (`crates/core/src/agent/llm_behavior.rs:78-145`).
- No property test on `extractMemories` JSON parser for
  malformed-LLM output (`crates/driver-loop/src/extract_memories.rs`).
- Migrations chain test (`crates/config/src/migrations.rs`)
  only on synthetic fixture; needs v0→v11 on a production-shape
  YAML.
- `Sleep` tool not in `EXPOSABLE_TOOLS`
  (`crates/config/src/types/mcp_exposable.rs:73-308`); operator
  enabling proactive can't expose Sleep over MCP.

**A8 — Doc / admin-ui drift** — ⬜ open. CLAUDE.md mandates
admin-ui/PHASES.md + docs/ in same commit; backfill needed.
- `admin-ui/PHASES.md` missing trackers for: 79.4 TodoWrite,
  79.5 LSP, 79.6 Team*, 79.7 Cron, 79.8 RemoteTrigger,
  79.10 ConfigTool, 79.11 MCP router, 79.13 NotebookEdit.
  Phase 77.18 + 77.20 listed `[ ]` even though code shipped.
- `docs/src/SUMMARY.md` missing pages: 77.1-77.3 compact tiers
  (page exists, not registered prominently), 77.4 cache-break
  diagnostics, 77.5 extractMemories, 77.7 secret-guard, 77.16
  AskUserQuestion, separate Sleep tool primer.
- CLAUDE.md table line "(MVP — Lsp/Team*/Config wiring deferred
  to 79.M.b/c/d)" stale — `mcp_server_bridge/dispatch.rs:371-499`
  shows them all wired. Update the parenthetical.

**A9 — Out-of-band hygiene** — ⬜ open.
- Recent commits include `Co-Authored-By: Claude Opus 4.7`
  trailers (e.g. `8ed115c`, `80bcac9`). User memory prohibits
  this. Don't rewrite history; remove from any commit template
  or future workflow.
- `7619fee chore: sync all local changes` is a 130-file mass
  commit hard to audit. Future practice: split.

### Autonomous mode hardening (audit 2026-04-28)
- No open items.

### MCP server — Phase 79.M follow-ups

**79.M.c.full** — Full Config tool body in mcp-server mode. **SHIPPED 2026-04-28**.
- Cargo feature `config-self-edit` gates the Config arm in
  `boot_exposable`. Boot context carries seven Config-only handles
  (applier + denylist + redactor + correlator + reload + policy +
  proposals_dir). `run_mcp_server` constructs all seven from the
  agent's YAML when `Config` is in `expose_tools`, then plus three
  hard refusals: (1) Cargo feature off → `SkippedFeatureGated`,
  (2) `mcp_server.auth_token_env` / `http.auth` missing →
  `SkippedDenied { config-requires-auth-token }`, (3)
  `agents.<id>.config_tool.self_edit = false` →
  `SkippedDenied { config-self-edit-policy-disabled }`,
  (4) `config_tool.allowed_paths` empty → refuse (operator must
  pick an explicit subset).
- Reload semantics in mcp-server mode: stub `McpServerReloadTrigger`
  warns + returns Ok. The operator-side `nexo run` daemon picks up
  YAML changes via Phase 18 file watcher. mcp-server itself does
  not host a `ConfigReloadCoordinator`.
- Threat model: see
  `docs/src/architecture/mcp-server-exposable.md::Threat-model`.

**79.M.h** — Hot-reload of `mcp_server.expose_tools`.
- Today: boot-time only. Operator must restart the mcp-server
  process to add/remove tools.
- Why deferred: Phase 18 hot-reload coverage doesn't yet drive a
  registry rebuild path. Acceptable: stdio mcp-server processes are
  short-lived (Claude Desktop / Cursor spawn them per-session).
  HTTP mcp-server is the real motivator — track under Phase 18
  coordinator extensions.

~~**79.M.completion** — MCP `completion/complete` returns empty
values for every request.~~ ✅ 2026-04-30
`completion/complete` now walks the target tool's `input_schema`,
extracts the `enum` array from the requested argument, and returns
populated `values`. `total` + `hasMore` fields added per MCP spec.
4 unit tests cover enum extraction, missing tool, no-enum arg, and
missing property. Graceful degradation: any parse failure returns
empty `[]` rather than an error.

**79.M.followup-autonomous** — `nexo mcp-server` cannot run
autonomous wait/retry loops by itself.
- Missing: a durable autonomous loop in mcp-server mode that
  processes due follow-ups/reminders without requiring a separate
  `nexo run` daemon (`AgentRuntime` + heartbeat tick path). Today
  mcp-server exposes control-plane calls (`start_followup`,
  `check_followup`, `cancel_followup`) but does not host the
  runtime turn loop.
- Why deferred: current architecture keeps mcp-server as a
  tool-bridge process; autonomous scheduling/execution lives in
  `nexo run`. Merging both concerns needs clear ownership of broker
  subscriptions, session lifecycle, and tick concurrency in mcp mode.
- Target: 79.M follow-up sub-phase (design + implementation of an
  optional autonomous worker profile for mcp-server).

**79.7.tool-calls** — shipped (opt-in) on 2026-04-28.
- Delivered: `LlmCronDispatcher` now supports an iterative
  tool-call loop (assistant tool_calls -> registry dispatch ->
  tool_result chaining -> follow-up model turn) with bounded
  iterations.
- Policy gates: disabled by default; operators must enable
  `runtime.cron.tool_calls.enabled`. Effective tool surface is
  narrowed by binding policy plus `runtime.cron.tool_calls.allowlist`.
  A stable per-entry `session_id` is injected for tool contexts.
- Minimal runtime profile (marketing follow-ups safe allowlist):
  ```yaml
  schema_version: 11
  cron:
    tool_calls:
      enabled: true
      max_iterations: 6
      allowlist:
        - email_search
        - email_thread
        - email_reply
        - cancel_followup
        - check_followup
  ```
- Manual smoke (reproducible):
  1. Fast dispatcher proof:
     `cargo test -p nexo-core llm_cron_dispatcher::tests::tool_calls_execute_when_executor_enabled -- --nocapture`
  2. Runtime wiring proof:
     run `nexo run` with `config/runtime.yaml` above and confirm startup log:
     `"[cron] tool-call execution enabled"` with expected `allowlist`.
  3. End-to-end follow-up flow (from your MCP client):
     - `start_followup` args example:
       ```json
       {
         "thread_root_id": "<message-id-root>",
         "instance": "ops",
         "recipient": "cliente@example.com",
         "check_after": "24h",
         "max_attempts": 3
       }
       ```
       Save returned `flow.flow_id`.
     - `cron_create` args example (one-shot):
       ```json
       {
         "cron": "*/2 * * * *",
         "recurring": false,
         "prompt": "Usa check_followup con flow_id=<FLOW_ID>. Si flow.status es active, llama cancel_followup con reason='smoke'. Cierra con texto: smoke-ok."
       }
       ```
     - Verify after next fire window:
       `check_followup` on the same `flow_id` returns `flow.status = \"cancelled\"`.
       Optional: `nexo cron list --json` no longer includes that one-shot entry.
- Remaining hardening follow-up: per-tool timeout/idempotency policy
  for high-side-effect tools, plus richer compensation semantics.

**79.M.denied-by-default surface** — shipped on 2026-04-28.
- Delivered: `mcp_server.denied_tools_profile` is now a mandatory
  hardening gate for denied overrides (`Heartbeat`, `delegate`,
  `RemoteTrigger`), with fail-closed defaults (`enabled=false`, all
  allow bits false).
- Policy: denied tool registration now requires:
  1) tool in `expose_denied_tools`,
  2) `denied_tools_profile.enabled=true`,
  3) matching `denied_tools_profile.allow.<tool>=true`.
- Validation checks:
  - `require_auth` (default true) enforces MCP auth before denied
    side-effect tools boot.
  - `require_delegate_allowlist` (default true) requires explicit
    restricted `agents.<id>.allowed_delegates` (non-empty, not `*`)
    for `delegate`.
  - `require_remote_trigger_targets` (default true) requires explicit
    `agents.<id>.remote_triggers` entries for `RemoteTrigger`.

**79.M.taskflow-session-context** — shipped on 2026-04-28.
- Delivered: MCP `tools/call` now forwards request-scoped
  `DispatchContext` to handlers through context-aware trait hooks
  (`call_tool_with_context` / `call_tool_streaming_with_context`).
- Bridge fix: `ToolRegistryBridge` now executes each tool call with a
  per-call `AgentContext` clone that injects `session_id` from MCP
  dispatch context (UUID parse, fallback deterministic UUIDv5 for
  non-UUID ids), instead of always using the fixed boot context.
- Stdio parity: stdio transport now stamps a stable per-process
  implicit `session_id`, so context-dependent tools (`taskflow`) also
  work in stdio MCP sessions.
- Coverage: bridge unit test verifies session-id injection from
  dispatch context.

### Pollers (Phase 19 V2)

P-1. **`inventory!` macro registry for built-in pollers**
- Missing: compile-time auto-discovery so a new built-in lands by
  adding a single `pub mod` line, no `register_all` edit.
- Why deferred: pre-optimisation. The four current built-ins
  (gmail, rss, webhook_poll, google_calendar) plus extension-loaded
  pollers via the new `capabilities.pollers` capability are easy to
  maintain by hand; the explicit `register_all` is a useful audit
  point. Worth revisiting only when the list crosses ~20 entries.
- Target: when poller count grows.

P-2. **Multi-host runner orchestration**
- Missing: a coordinator that decides which host owns which job
  (the cross-process SQLite lease already prevents double-tick;
  what's missing is balanced placement and failover for tens of
  thousands of jobs spread across N daemons).
- Why deferred: speculative without a real multi-host deploy.
  Single-host workloads scale fine on the current model.
- Target: when a deployment actually needs >1 daemon.

P-3. **Push-based watchers (Gmail Push, generic inbound webhooks)**
- Missing: an HTTP server that accepts pushed events and adapts
  them to the same downstream `OutboundDelivery` plumbing the
  poller uses.
- Why deferred: opposite shape from polling — needs a public TLS
  surface (Cloudflare tunnel?) plus auth on inbound. Better as
  its own crate (Phase 20?), not an extension of the poller.
- Target: separate phase; keep notes here while it's only an idea.

### Hardening

H-1. ~~**CircuitBreaker missing on Telegram + Google plugins**~~  ✅ 2026-04-26
- ~~Telegram side fully wired~~ — `BotClient` now owns
  `circuit: Arc<CircuitBreaker>` (one breaker per `BotClient`
  instance, breaker name `telegram.<redacted-host>` so logs
  never carry the bot token). All three HTTP exit points
  (`call_json` JSON POST, multipart `sendDocument`, `download_file`
  GET) flow through a single `run_breakered` helper that maps
  `CircuitError::Open` → `bail!("circuit breaker open")` and
  passes inner errors through. 13 existing telegram tests still
  pass.
- ~~Google general-API side wired~~ — `GoogleAuthClient` now
  owns its own `circuit` field; `authorized_call` (the HOT path
  used by every google_* tool) wraps via `run_breakered` with
  the same map.
- ~~All 5 Google OAuth exit points wired (2026-04-26)~~ —
  `exchange_code`, `request_device_code`, `poll_device_token`,
  `refresh_if_needed`, and `revoke` all flow through the same
  `run_breakered` helper. Each call site rolls the entire
  request → status check → JSON parse block inside the closure
  so a transport failure, malformed body, or 4xx/5xx all count
  the same toward the breaker's failure threshold. The polling
  loop in `poll_device_token` wraps each iteration separately
  so a sustained burst of `authorization_pending` (which is
  expected and not a failure) doesn't trip the breaker.
  `revoke` keeps its best-effort semantics — local state is
  wiped regardless of upstream success.
- Scoping decision locked in: **one breaker per client
  instance** (per BotClient, per GoogleAuthClient). Multi-tenant
  setups holding multiple instances get isolated breakers, so a
  single bad token doesn't cascade across tenants.

H-2. **C1 — `EffectiveBindingPolicy` extension (per-binding override
for `lsp` / `team` / `config_tool` / `repl`)** — ✅ shipped 2026-04-30.
- Surfaced by audit `proyecto/proyecto/AUDIT-2026-04-30.md`.
  `EffectiveBindingPolicy` (`crates/core/src/agent/effective.rs:38`)
  now carries 4 additional resolved fields plus 4 mirror resolvers
  (`resolve_lsp` / `resolve_team` / `resolve_config_tool` /
  `resolve_repl`). `InboundBinding` (`crates/config/src/types/agents.rs`)
  gains 3 new optional override fields (`lsp` / `team` /
  `config_tool`); `repl` was already declared (Phase 79.12) but
  silently inherited because the resolver was missing — closed.
  10 new tests in `effective.rs::tests` (8 golden) and
  `binding_validate.rs::tests` (2 covering 7 sub-cases).
- `binding_validate::has_any_override` extended from 12 to 19
  conditions so the "binding without overrides" warning stops
  lying for `plan_mode` / `role` / `proactive` / `repl` / `lsp` /
  `team` / `config_tool`.
- **Boot-time only** — the new resolved fields are not yet read by
  the per-agent boot loop in `src/main.rs:2326-2680` (which still
  calls `agent_cfg.lsp` / `agent_cfg.team` / `agent_cfg.config_tool`
  / `agent_cfg.repl` directly). That refactor + `ConfigReloadCoordinator`
  post-hooks for `LspManager` / `TeamMessageRouter` /
  `ReplRegistry` / cron-tool bindings is **C2** — see below.
- No YAML breakage: defaults `None` → inherit. The single
  observable runtime change is that `inbound_bindings[].repl`
  overrides will start applying — `grep -rn "repl:" config/` is
  empty in this repo so no config in the tree is affected.

H-3. **C2 — Hot-reload pickup via config-pull at handler entry** —
✅ shipped 2026-04-30 (commits `df857fe`, `4649e99`, `23ef4ed`,
`9baa380`). Tool handlers now read `ctx.effective_policy().<x>` per
call instead of capturing policy at `Tool::new`. Closes the
C1 → C2 loop: per-binding YAML overrides (lsp / team / repl /
config_tool) added by C1 are now observed on the next intake event
without restart.
Scope shipped:
- 10 sitios `agent_cfg.<x>` → `effective_boot.<x>` en `src/main.rs`
  (boot-time reads consolidated through
  `EffectiveBindingPolicy::from_agent_defaults`).
- `LspTool` migrated: drops `policy: ExecutePolicy` field; handler
  reads `ctx.effective_policy().lsp` and converts via private
  adapter `execute_policy_from(&LspPolicy) -> ExecutePolicy`. 3
  new tests.
- `ReplTool` migrated: drops dead `config: ReplConfig` field; new
  per-call allowlist guard reads
  `ctx.effective_policy().repl.allowed_runtimes` before delegating
  to `ReplRegistry`. 2 new tests.
- `TeamTools` migrated: drops `policy: TeamPolicy` field; 5 handlers
  (`TeamCreate` / `TeamDelete` / `TeamSendMessage` / `TeamList` /
  `TeamStatus`) read `policy_for(ctx)` per call. 2 new C2 tests +
  19 existing tests refactored.
- `cron_tool` (`CronCreateTool`) was already config-pull
  (`crates/core/src/agent/cron_tool.rs:111`); confirmed
  C2-compliant, no change.
- `RemoteTriggerTool` was already config-pull
  (`crates/core/src/agent/remote_trigger_tool.rs:226`); confirmed,
  no change.
Limitations documented in `docs/src/ops/hot-reload.md`:
- Boolean enable flips (`lsp.enabled`, `team.enabled`,
  `repl.enabled`, `config_tool.self_edit`, `proactive.enabled`)
  still require restart — `Arc<ToolRegistry>` (`tool_base`) is
  immutable post-boot.
- Subsystem actor lifecycle (LspManager child processes,
  ReplRegistry subprocess pool, TeamMessageRouter broker subs)
  unchanged across reload — matches claude-code-leak
  `src/services/mcp/useManageMCPConnections.ts:624` (invalidate-
  and-refetch, no actor teardown) and OpenClaw
  `research/src/plugins/services.ts:33-78` (services boot-once).
- Mid-session sessions in `runtime.rs:752 session_txs.entry().or_insert_with`
  retain captured ctx until end. NEW sessions/events post-reload
  see new policy. Phase 18 invariant.
References (validation, not copy):
- claude-code-leak `src/tools/BashTool/shouldUseSandbox.ts:53` —
  re-read settings per-call (config-pull pattern).
- claude-code-leak `src/services/mcp/useManageMCPConnections.ts:624` —
  invalidate-and-refetch, no kill.
- research/ `src/agents/channel-tools.ts:95-112` — config-pull
  per turn factory pattern.
Implementation 100% Rust:
`Arc<EffectiveBindingPolicy>` lookup via `AgentContext`,
`ArcSwap<RuntimeSnapshot>` swap, tokio mpsc reload channel,
`From` traits for cross-crate adapters.

H-3.b (M5 + M5.b). **`cron_tool_bindings` registry hot-reload** —
✅ shipped 2026-04-30 fully complete.

**M5 (commit `64136cf`)** — ArcSwap infrastructure:
`RuntimeCronToolExecutor.by_binding` migrated from `Arc<HashMap>`
to `Arc<arc_swap::ArcSwap<HashMap<...>>>` enabling lock-free
atomic hot-swap via the new `replace_bindings(new_map)` API.
`resolve_binding` returns owned `Option<CronToolBindingContext>`.

**M5.b (commits `7a640e7`, `fcaca59`, plus pending docs commit)**
— post-hook wire activates the `replace_bindings` API:
1. Extracted `build_cron_bindings_from_snapshots(snapshots, deps)
   -> HashMap<String, CronToolBindingContext>` free function in
   `src/main.rs` plus `compute_binding_key` + `compute_inbound_origin`
   helpers. Replaces the inline `register_cron_binding` closure
   verbatim (semantic-preserving refactor).
2. New `CronRebuildDeps` struct (Clone) bundles the 10 Arcs/handles
   the rebuild fn consumes.
3. `tools_per_agent: Arc<HashMap<agent_id, Arc<ToolRegistry>>>` and
   `agent_snapshot_handles: Arc<HashMap<agent_id, Arc<ArcSwap<RuntimeSnapshot>>>>`
   aggregated during the boot agent loop. `runtime.snapshot_handle()`
   is `&self -> Arc<...>` (does not consume), called BEFORE
   `runtime.start().await` which moves `self`.
4. `Arc<tokio::sync::OnceCell<Arc<RuntimeCronToolExecutor>>>` cell
   declared near the reload coordinator wire (mirror Phase 79.10.b
   reload_cell pattern at `:1923-1925`). Late-bind via `.set()` at
   the executor construction site so subsequent reloads can call
   `replace_bindings` via the post-hook.
5. Post-hook registered before `reload_coord.start()`. Empty-cell
   case (reload triggered before executor built) is graceful no-op
   with `tracing::debug!`.
6. 3 smoke tests in `src/main.rs::tests`:
   `cron_executor_replace_bindings_atomically_swaps_map` (M5),
   `cron_executor_replace_bindings_with_empty_map_clears_all` (M5),
   `cron_post_hook_no_op_when_cell_empty` (M5.b).

Net result: per-binding policy changes (`team.max_*`,
`lsp.languages`, `repl.allowed_runtimes`,
`config_tool.allowed_paths`, etc.) now apply to cron firings on
the next call after reload, without daemon restart. The
`dead_code` warning on `replace_bindings` from M5 step 1 is
resolved.

**Limitation**: agent add/remove during runtime still requires
daemon restart (Phase 19 scope; `tools_per_agent` and
`agent_snapshot_handles` are populated during the boot agent loop
and never extended). Documented in
`build_cron_bindings_from_snapshots` doc-comment.

References (validation, not copy):
- claude-code-leak `src/utils/cronScheduler.ts:441-448` —
  chokidar-on-file-change rebuild + `:170,251,335-336,356`
  `inFlight` Set with pitfall.
- research/ `src/cron/service/timer.ts:709,697` —
  forceReload-per-tick + long-job pitfall. We rebuild on reload
  only because ArcSwap gives lock-free swap structurally.

**M5.c — full integration test** ⬜ open. The smoke test covers
the empty-cell early-return; full integration with a real
`ConfigReloadCoordinator::reload()` (broker fixture + config
dir manipulation + assertion that `replace_bindings` was called
with the expected map) is deferred. ~45 min.

H-3.c (M11 — full ConfigTool config-pull) — ⬜ open. ConfigTool
struct (`crates/core/src/agent/config_tool.rs:164-189`) captures
`allowed_paths` + `approval_timeout_secs` at construction. The 7
read sites (`config_tool.rs:515,584,624,1024,1027,...`) use
`self.<field>` instead of pulling from
`ctx.effective_policy().config_tool` per call. Same refactor
shape as the four C2 tools just shipped, but the file is 1500+
LOC and the call sites are deeper in the propose/apply state
machine — deferred for focused review. Effort: ~2 hr.

### Phase 21 — Link understanding

L-1. ~~**Telemetry counters for link fetches**~~  ✅ shipped
- `nexo_link_understanding_fetch_total{result}` (ok / blocked /
  timeout / non_html / too_big / error),
  `nexo_link_understanding_cache_total{hit}` (true / false), and a
  single-series `nexo_link_understanding_fetch_duration_ms` histogram
  emitted from `crates/core/src/link_understanding.rs::fetch`.
  Counters update on every fetch attempt; the histogram only fires
  when an HTTP request actually went out (cache hits and host-blocked
  URLs skip it to keep latency stats honest).

L-2. ~~**`readability`-style extraction**~~  ✅ shipped 2026-04-26
- `extract_main_text` now drops the universal boilerplate tag set:
  on top of the original `<script>` / `<style>` / `<noscript>` /
  `<head>`, it also nukes `<nav>`, `<header>`, `<footer>`,
  `<aside>`, `<form>`, `<button>`, `<menu>`, `<iframe>`, `<svg>`,
  `<dialog>`, `<template>`. That alone covers the majority of
  noisy-page article extraction wins.
- New `strip_blocks_by_class_keyword` pass handles sites that
  render boilerplate inside `<div>`s instead of semantic tags:
  drops any element whose `class` / `id` / `role` attribute
  contains `sidebar`, `comment`, `advert`, `share`, `social`,
  `cookie`, `popup`, `newsletter`, `related-article`,
  `related-posts`, `navigation`, `breadcrumb`, `promo`,
  `subscribe`. Tag-agnostic — same logic catches
  `<div class="sidebar">` and `<aside class="sidebar">`.
- 5 new tests cover semantic-boilerplate strip, class-marked
  sidebars, role="navigation" attribute matching, negative
  control (innocent class names like `content` /
  `article-body` / `byline` survive), and form/button clutter
  removal. Runs alongside the existing 13 tests in
  `link_understanding::tests`.
- No new crate dependency; pure-Rust implementation. Real DOM-walk
  readability via the `scraper` crate is the next-step upgrade
  if a specific site shape still leaks.

### Phase 25 — Web search

W-1. ~~**Telemetry counters not wired**~~  ✅ shipped
- `nexo_web_search_calls_total{provider,result}` (result ∈ ok /
  error / unavailable), `nexo_web_search_cache_total{provider,hit}`,
  `nexo_web_search_breaker_open_total{provider}`, and
  `nexo_web_search_latency_ms{provider}` histogram now emitted from
  `crates/web-search/src/telemetry.rs` and stitched into the host
  `/metrics` response by `nexo_core::telemetry::render_prometheus`.
  Latency is recorded only for attempts that actually issued an HTTP
  request — cache hits and breaker short-circuits skip it so
  percentiles reflect real provider work. The "unavailable" label
  distinguishes a breaker-open short-circuit from a real error so
  dashboards can alert without false positives during a self-healing
  cooldown.

W-2. ~~**`web_fetch` built-in tool not shipped**~~  ✅ shipped 2026-04-26
- New `crates/core/src/agent/web_fetch_tool.rs::WebFetchTool`.
  Single-call shape: `web_fetch(urls: [str], max_bytes?: int)`
  → `{ results: [{url, title, body, ok, reason?}] }`.
- Reuses the runtime's existing `LinkExtractor` (Phase 21),
  so the cache, deny-host list, max-bytes cap, timeout, and
  telemetry counters all carry over with zero duplication.
  `nexo_link_understanding_fetch_total{result}` and
  `nexo_link_understanding_cache_total{hit}` cover `web_fetch`
  calls automatically.
- Per-call cap of 5 URLs to keep the prompt budget bounded;
  trims with a warn log and continues. `max_bytes` arg can
  shrink but never grow past the deployment-wide
  `link_understanding.max_bytes`.
- Failures (host blocked / timeout / non-HTML / oversized /
  transport error) return per-URL
  `{ok: false, reason: "..."}` rows instead of bailing the
  whole call, so a single bad URL doesn't drop the rest.
- Registered unconditionally for every agent in `src/main.rs`
  (runtime always boots a `LinkExtractor`); the per-binding
  `link_understanding.enabled` policy still gates whether the
  underlying fetch happens.
- 2 unit tests (`tool_def_shape`, `rejects_empty_urls_array`)
  in the module.
- Distinct from `web_search.expand=true` because the agent
  often knows the URL up front (skill output, RSS poll,
  calendar attachment) and would otherwise have to either
  hallucinate a search query or shell out to a `fetch-url`
  extension.

W-3. ~~**Setup wizard entry not shipped**~~  ✅ shipped 2026-04-26
- New `web-search` ServiceDef in
  `crates/setup/src/services/skills.rs::defs()`. Distinct from
  the existing `brave-search` entry (which configures the
  MCP-based skill); this one writes the keys the in-process
  Phase 25 router consumes.
- Three fields:
    * `brave_api_key` (secret → `web_search_brave_api_key.txt`,
      env `BRAVE_SEARCH_API_KEY`).
    * `tavily_api_key` (secret →
      `web_search_tavily_api_key.txt`, env `TAVILY_API_KEY`).
    * `default_provider` (env-only `WEB_SEARCH_DEFAULT_PROVIDER`,
      default `brave`).
  Both keys are optional individually — the router falls back
  across whichever provider is configured.
- Operator runs `nexo setup` and picks "Web search router (Phase
  25)" from the Skills category, same flow as every other
  service.
- Description text + help strings written in English (per the
  workspace language rules). Existing entries above still have
  Spanish strings — those predate the rule.
- admin-ui Phase A3 web-search panel will surface the same
  fields when it lands.

W-4. **Decision: `nexo-resilience::CircuitBreaker` directly, not via `BreakerRegistry`**
- The `nexo-auth` registry is keyed on `Channel { Whatsapp,
  Telegram, Google }`. Web search isn't a channel; jamming it into
  that enum would force unrelated changes. We instead hold a
  per-provider `Arc<CircuitBreaker>` map inside the router. Worth
  unifying if more "non-channel external HTTP" surfaces land —
  bring it up next brainstorm.

W-5. **Cache `:memory:` SQLite quirk**
- The router cache pins `max_connections=1` when `path == ":memory:"`
  because SQLite's in-memory database is per-connection. File-backed
  paths use the normal pool size. Documented inline; not a defect.

### Phase 26 — Pairing protocol

PR-1. ~~**Plugin gate hooks for WhatsApp + Telegram**~~  ✅ shipped (in agent-core intake)
- The gate now runs in the runtime intake hot path
  (`crates/core/src/agent/runtime.rs`) right before the per-sender
  rate limiter. Plugins do not need bespoke wiring — the gate sees
  every event regardless of source plugin, keyed by
  `(source_plugin, source_instance, sender_id)`. Default
  `auto_challenge=false` keeps existing setups silent.
- Reply-back path deferred: when a sender is challenged the code is
  only logged (operator approves via `nexo pair approve`). Sending
  the code through the channel adapter so the sender sees it in
  their chat is PR-1.1, separate work that needs a per-channel
  outbound publish helper.

PR-1.1. ~~**Challenge reply through channel adapter**~~  ✅ shipped (Phase 26.x, 2026-04-25)
- `PairingAdapterRegistry` lives in `nexo-pairing`; bin registers
  `WhatsappPairingAdapter` + `TelegramPairingAdapter` at boot.
- Per-channel `normalize_sender` is plumbed through
  `PairingGate::should_admit` so store lookup + cache key use the
  canonical form (WA strips `@c.us`, TG lower-cases `@username`).
- Telegram challenges escape MarkdownV2 reserved chars and wrap the
  code in backticks; WhatsApp ships the legacy plain-text shape.
- New counter
  `pairing_inbound_challenged_total{channel,result}` covers the
  delivery outcomes (`delivered_via_adapter`,
  `delivered_via_broker`, `publish_failed`,
  `no_adapter_no_broker_topic`).
- **Still deferred:** direct in-process `Session::send_text` —
  adapters currently publish on
  `plugin.outbound.{channel}[.<account>]` like the rest of the
  system; skipping the broker round-trip is a separate refactor and
  not on the critical path.

PR-2. **Telemetry counters not wired** ✅ Closed 2026-04-25 (Phase 26.y).
- ~~`pairing_requests_pending{channel}`~~ ✅ gauge, push-tracked, with
  `PairingStore::refresh_pending_gauge` exposed for drift recovery.
- ~~`pairing_approvals_total{channel,result}`~~ ✅ counter, three results:
  `ok | expired | not_found`.
- ~~`pairing_codes_expired_total`~~ ✅ counter, bumped from
  `purge_expired` (per row) and from `approve` (per expired hit).
- ~~`pairing_bootstrap_tokens_issued_total{profile}`~~ ✅ counter on
  every `BootstrapTokenIssuer::issue`.
- ~~`pairing_inbound_challenged_total{channel,result}`~~ ✅ shipped
  with Phase 26.x adapter work.
- All four counters live in `nexo-pairing::telemetry` (leaf crate);
  `nexo_core::telemetry::render_prometheus` stitches them in next to
  the web-search block. Consumer: admin-ui Phase A4.

PR-3. ~~**`tunnel.url` integration in URL resolver**~~  🔄 partial 2026-04-26
- ~~`run_pair_start` URL resolver chain wired~~ — priority is
  now (1) `--public-url` CLI flag, (2) `pairing.yaml`
  `public_url`, (3) `NEXO_TUNNEL_URL` env var, (4) loopback
  fail-closed. The `nexo-tunnel` daemon writes its assigned
  `https://*.trycloudflare.com` URL into `NEXO_TUNNEL_URL` at
  startup, which a separately-launched `nexo pair start` picks
  up without IPC plumbing.
- ~~`ws_cleartext_allow` from `pairing.yaml` plumbed into the
  resolver `extras` list~~, so an operator setting that list in
  YAML actually changes the cleartext-host allowlist. Resolves
  the second deferred item from PR-6.
- ~~`pair_paths` consults `pairing.yaml` overrides~~ for both
  store path and secret path so CLI subcommands honour the
  same config the daemon does. Falls back to legacy defaults
  unchanged when the YAML is absent.
- ~~In-process URL accessor across daemon ↔ CLI~~  ✅ shipped
  2026-04-26 via a sidecar file at
  `$NEXO_HOME/state/tunnel.url`. `nexo-tunnel` exposes
  `url_state_path()`, `write_url_file()`, `read_url_file()`,
  `clear_url_file()`. The daemon writes the URL on
  `TunnelManager::start()` success; `nexo pair start` reads it
  with priority above the env-var fallback. Atomic writes
  (`<path>.tmp` + rename) so a CLI reading mid-write never
  sees a torn URL. Round-trip unit test covers happy path +
  whitespace trim + idempotent clear.

PR-4. ~~**Companion-tui not shipped**~~ ✅ 2026-04-27 (PR-4.x WS handshake complete)
- ~~Reference scaffold shipped~~ as `crates/companion-tui`.
- ~~PR-4.x~~ WS handshake shipped 2026-04-27:
  - Server: `GET /pair` detected via `TcpStream::peek()` in
    `handle_health_conn`; `tokio_tungstenite::accept_async` upgrades
    the raw stream without consuming bytes. Server verifies HMAC via
    `SetupCodeIssuer::verify`, issues a 32-byte random session token
    (base64url), persists in `PairingSessionStore` (SQLite,
    `$NEXO_HOME/state/pairing_sessions.db`, 24h TTL), returns
    `{"session_token": "..."}`. Context available via
    `PairingHandshakeCtx` in `OnceLock` in `RuntimeHealth`.
  - Client: `nexo-companion` calls `ws::perform_handshake`, writes
    session token to `$NEXO_HOME/pairing/sessions/<label>.token`
    (0600, atomic rename).
  - `run_pair_start` now embeds the full `/pair` path in the
    setup-code URL so the companion connects directly.
  - 4 session_store unit tests + 3 ws sanitize tests.
- Bugs found and fixed during 2026-04-27 audit (all corrected in-session):
  - `pair_url` variable never applied to `run_pair_start` — `issuer.issue()`
    was still passing `&resolved.url` without `/pair`, so the companion would
    connect to the base URL and the peek-router would never route to `handle_pair_ws`.
  - Session TTL used `default_ttl_secs * 144` formula — if operator set
    `default_ttl_secs = 3600`, sessions lasted 6 days. Fixed to always 86400 s.
  - `remote_triggers: Vec::new()` missing from `run_mcp_server` `AgentConfig`
    initializer — caused compile error when `AgentConfig` gained the field.
  - `insert_session` called `Utc::now()` twice (skew between `issued_at` and
    `expires_at`). Fixed to single capture.
  - `lookup_session` used `unwrap_or_else(Utc::now)` for corrupt timestamp —
    silently returned current time as expiry. Fixed to propagate error via
    `.ok_or(PairingError::Storage(...))? + .transpose()`.
- Remaining open items:
  - Session token validation on subsequent companion requests
    (not yet consumed by any handler — `lookup_session` exists
    but is not wired to any auth gate).
  - `pairing.session_ttl_secs` YAML config field — currently hardcoded 86400 s.
    Add as an optional override in `PairingConfig` so operators can tune
    without rebuilding.

PR-5. **`pair_approve` as scope-gated agent tool**
- Missing: a built-in tool that lets agents approve pending
  pairings from a trusted channel, scoped via
  `EffectiveBindingPolicy::allowed_tools`.
- Why deferred: opens prompt-injection vectors (an agent could be
  coerced into approving an attacker). Operator-driven approve via
  CLI / admin-ui is the safe default. Worth revisiting if a clear
  trust model emerges.
- Target: separate brainstorm.

PR-6. ~~**`nexo-config::pairing.yaml` loader**~~  🔄 partial 2026-04-26
- ~~`config/pairing.yaml` schema + loader shipped.~~
  `crates/config/src/types/pairing.rs` defines
  `PairingConfig { pairing: PairingInner }` with optional
  fields: `storage.path`, `setup_code.secret_path`,
  `setup_code.default_ttl_secs`, `public_url`,
  `ws_cleartext_allow[]`. `deny_unknown_fields` everywhere so
  typos fail loud at boot.
- ~~Loader wired into `AppConfig`~~ —
  `cfg.pairing: Option<PairingInner>` populated by
  `load_optional("pairing.yaml")` (file is optional; absent
  keeps every legacy default).
- ~~Boot integration in `src/main.rs`~~ — the `pairing` block
  consults `cfg.pairing` first for both store path and
  secret path, falling back to the previous hardcoded
  `<memory_dir>/pairing.db` / `~/.nexo/secret/pairing.key`
  defaults when the YAML is absent or doesn't override that
  field. New `from_yaml=true|false` log field reflects which
  path provided the values.
- 4 unit tests cover empty YAML → defaults, full YAML round
  trip, unknown-field rejection at root + nested levels.
- **Still deferred**: `nexo-tunnel` URL accessor exposing the
  active tunnel URL (separate side of PR-6, originally bundled).
  The `pairing.yaml` `public_url` field is wired but the
  `tunnel.url` priority chain (PR-3) still hardcodes the CLI
  fallback. Splitting into PR-6.a (config loader, done) and
  PR-3 (tunnel accessor, separate) keeps the work
  cleanly scoped.
- ~~`default_ttl_secs` honoured by `nexo pair start`~~  ✅
  (commit landed alongside W-3). Resolution priority is now
  (1) `--ttl-secs` CLI flag, (2) YAML `default_ttl_secs`,
  (3) 600s hardcoded fallback. The CLI parser switched to
  `Option<u64>` so absent flag is genuinely "no override"
  rather than the previous baked-in 600 default.
- ~~**`ws_cleartext_allow` not plumbed**~~ ✅ already wired —
  `run_pair_start` reads `yaml_overrides.ws_cleartext_allow` into
  `yaml_cleartext` and passes it to `UrlInputs.ws_cleartext_allow_extra`
  before calling the resolver. FOLLOWUPS entry was stale.

### Phase 67.A–H — Project tracker + multi-agent dispatch

PT-1. **`ToolHandler` adapter for dispatch tools not yet
registered**
- Missing: each `program_phase_dispatch`, `dispatch_followup`,
  `cancel_agent`, etc. is a plain async function. The runtime
  needs a `nexo_core::ToolHandler` adapter that builds the
  context (resolved DispatchPolicy, sender_trusted, dispatcher
  identity) per-binding and forwards to the function.
- Why deferred: the adapter touches the runtime intake hot
  path (`crates/core/src/agent/runtime.rs`) and the per-binding
  cache; landing it in 67.E.1 would have stretched the step.
  Functions are decoupled and tested directly; the adapter
  step is a wiring exercise behind the binding refactor.
- Target: 67.H.x adapter step alongside the binary refactor that
  folds `nexo-driver-tools` into `nexo-driver`.

PT-2. **Runtime intake migration to `get_or_build_with_dispatch`**
- Missing: existing call sites use the old
  `get_or_build(allowed_tools)` API; the new dispatch-aware
  variant is callable but unused.
- Why deferred: switching call sites needs the dispatcher /
  is_admin context plumbed through binding resolution. PT-1
  unblocks this — both land together.
- Target: same as PT-1.

PT-3. **`DispatchTelemetry` not wired into `program_phase` /
hook dispatcher / registry**
- Missing: the trait + payloads + canonical subjects ship in
  Phase 67.H.2 but every call site uses `NoopTelemetry` today.
  No `agent.dispatch.*` / `agent.tool.hook.*` /
  `agent.registry.snapshot.*` traffic is emitted yet.
- Why deferred: emission needs an instance threaded through
  the call sites, which in turn depends on PT-1's adapter
  layer. Pure plumbing — no decision left.
- Target: alongside PT-1 / PT-2.

PT-4. ~~**`HookIdempotencyStore` not consumed by `DefaultHookDispatcher`**~~  ✅ 2026-04-27
- The dispatcher's pre-action claim + post-failure release was already
  implemented in `dispatcher.rs:180-217` (shipped in an earlier pass).
- Boot wiring added in `src/main.rs`: opens
  `$NEXO_HOME/state/hook_idempotency.db` and passes it to
  `DefaultHookDispatcher::with_idempotency()`. Failure degrades to
  idempotency-less mode with `tracing::warn!` — non-fatal.
- `EventForwarder` gains `idempotency: Option<Arc<HookIdempotencyStore>>`
  field + `with_idempotency()` builder. On `GoalCompleted` it calls
  `store.forget_goal(goal_id)` after `hook_registry.drop_goal()` to
  prevent unbounded table growth. Failures are best-effort (warn only).
- 5 existing tests in `hook_idempotency_after_restart.rs` cover the
  full flow (replay skip, restart persistence, B10 retry, forget).

PT-5. ~~**Single-flight cap-counting race in `AgentRegistry::admit`**~~  ✅ already shipped
- `admit_lock: tokio::sync::Mutex<()>` in `registry.rs:71` serialises
  the entire `count_running → cap check → insert` critical section.
- Test `concurrent_admits_do_not_overshoot_cap` validates 10 concurrent
  admits with cap=3 → exactly 3 Running + 7 Queued.
- FOLLOWUPS entry was stale; fix was deployed alongside the registry
  hardening pass. No further action needed.

PT-6. **`nexo-driver` and `nexo-driver-tools` are separate bins**
- Missing: a single binary that exposes both `run` (Claude
  subprocess driver) and `status / dispatch / agents`
  (project-tracker CLI). Folding them needs to break the
  current crate-graph cycle (driver-loop ↔ dispatch-tools).
- Why deferred: cycle-breaking is a refactor (move the bin to
  a new top-level crate that depends on both, or push the
  dispatch surface into a feature flag of driver-loop).
  Separate bins ship today.
- Target: binary refactor pass.

PT-7. **No NATS-backed `DispatchTelemetry` impl**
- Missing: production `DispatchTelemetry` should publish to the
  daemon's `async-nats` client. Currently only `NoopTelemetry`.
- Why deferred: the impl is a thin adapter but lives next to
  `NatsEventSink` in `nexo-driver-loop`, which adds a
  reverse-dep on dispatch-tools. Same cycle-breaking refactor
  as PT-6.
- Target: alongside PT-6.

PT-9. ~~**Non-chat origin discriminator hardcoded as 'console'**~~  ✅ effectively resolved
- `NON_CHAT_ORIGIN_PLUGINS: &[&str] = &["console", "cron", "webhook", "heartbeat"]`
  already exists at `dispatch-tools/src/hooks/dispatcher.rs:21-25` and
  the `run_action()` check uses `.contains()` against it. All four
  non-chat origins are covered — no cron/webhook/heartbeat goal will
  send a spurious chat reply.
- The code comment explicitly notes the constant is a bridge until a
  full `OriginAdapter` trait lands. That trait is better deferred until
  a plugin needs custom behavior beyond a boolean (e.g., per-origin
  render format). Current constant is the right level of complexity.

PT-8. **Multi-agent end-to-end test not shipped**
- Missing: a single integration test that wires
  orchestrator + registry + dispatch-tools + a mock
  pairing-adapter, dispatches two goals concurrently, and
  asserts a `notify_origin` summary lands on the mock adapter
  for each.
- Why deferred: the test needs the adapter wiring (PT-1) so
  the chat origin propagates into the hook payload.
- Target: alongside PT-1 / PT-3 / PT-4.

### ~~Browser plugin leaks zombie child processes~~  ✅ 2026-04-27

- Fixed in `crates/plugins/browser/src/chrome.rs` + `plugin.rs`.
- `RunningChrome::shutdown(self)` now calls `child.kill().await` +
  `child.wait().await` before consuming self — process is reaped
  before the handle is dropped.
- `BrowserPlugin::stop()` calls `chrome.shutdown().await` explicitly
  instead of assigning `None` (which triggered Drop without reaping).
- `Drop` kept as safety-net with a `tracing::warn!` so unexpected
  drops surface in logs rather than silently accumulating zombies.
- Unit test `shutdown_reaps_process` verifies kill(pid, 0) → ESRCH
  after shutdown (blocked on nexo-core Phase 79 WIP compile errors;
  test code is correct and will run once those are resolved).

### ~~`set_active_workspace` state lost on daemon restart~~  ✅ 2026-04-27

- Fixed via text-file sidecar at `$NEXO_HOME/state/active_workspace_path`
  (same pattern as `nexo-tunnel`'s `tunnel.url` sidecar).
- `crates/project-tracker/src/state.rs` — new module with
  `write_active_workspace_to(state_dir, path)` (temp+rename atomic write)
  and `read_active_workspace_from(state_dir)` (reads + verifies path exists).
  Public `write_active_workspace` / `read_active_workspace` convenience
  wrappers resolve `$NEXO_HOME/state/` automatically.
- `src/main.rs::boot_dispatch_ctx_if_enabled` — resolution order is now
  (1) `NEXO_PROJECT_ROOT` env var, (2) saved sidecar, (3) walk-up for
  `PHASES.md`, (4) cwd fallback.
- `dispatch_handlers.rs::SetActiveWorkspaceHandler` + `InitProjectHandler`
  — call `write_active_workspace` after every successful `switch_to()`.
  Failures log `tracing::warn!` and are non-fatal (in-memory state still
  correct; only the restart persistence is lost).
- 3 unit tests: roundtrip, missing-file → None, nonexistent-path → None.

### Phase 27.1 / 27.2 — cargo-dist + GH Actions release deferrals

Resolved by Phase 27.2 (kept here for traceability):
- ~~`NEXO_BUILD_CHANNEL` env stamp defaulted to `source` everywhere.~~
  CI release workflow now exports
  `NEXO_BUILD_CHANNEL=tarball-${target}` per musl runner and
  `NEXO_BUILD_CHANNEL=termux-aarch64` for the Termux job.
- ~~`x86_64-unknown-linux-gnu` host-fallback target.~~ Dropped from
  `dist-workspace.toml` in 27.2 — local builds use musl directly
  (operator must install zig 0.13.0 + cargo-zigbuild 0.22.3 per
  `packaging/README.md`).
- ~~macOS / Windows local validation needs vendor SDKs.~~ Targets
  removed from scope entirely (see backlog item below); no longer
  a deferral.

Open:

- **Local musl validation requires the pinned toolchain.** zig
  0.13.0 + cargo-zigbuild 0.22.3 must be on PATH; newer zig
  (0.14+ / 0.16) is incompatible with cargo-zigbuild 0.22.x.
  `make dist-check` fails loud with a pointer to
  `packaging/README.md` if zig is missing. Track upstream:
  <https://github.com/rust-cross/cargo-zigbuild>.
- **Termux runtime smoke-test.** Phase 27.2 validates the `.deb`
  sha256 sidecar but cannot run the bionic-libc binary on the
  ubuntu runner. Manual install on a device or Android emulator
  is the gate. Watch for headless Termux smoke options
  (proot-distro inside ubuntu? android-emulator GH action?).
- **Smoke-test auto-rollback.** When the post-publish smoke test
  fails, the assets are already up. Workflow goes red, operator
  decides. A rollback step would call `gh release delete-asset`
  per `EXPECTED_TARBALLS` member, idempotent. Risk: race with
  `sign-artifacts.yml` that may have already started.
- **`dist generate` vs hand-rolled `release.yml` drift.** When
  bumping `cargo-dist-version`, run `dist generate` in a scratch
  branch + diff against the hand-rolled file to catch new schema
  requirements. Today no automation flags drift.
- **Apple + Windows targets parked.** Apple
  (`x86_64`/`aarch64-apple-darwin`) and Windows
  (`x86_64-pc-windows-msvc`) dropped from scope in 27.2. Phase 27.6
  (Homebrew) parked with them. To revive: add the targets back to
  `dist-workspace.toml`, restore matrix entries in `release.yml`,
  revive `packaging/homebrew/`, restore PowerShell installer.
- **`/api/info` daemon endpoint to expose build stamps.** Admin UI
  footer / About page wants the same four stamps (`git-sha`,
  `target`, `channel`, `built-at`) over HTTP, not just the CLI.
  Wire when Phase A4 dashboard lands.
- **`nexo self-update` (Phase 27.10).** `install-updater = false`
  in `dist-workspace.toml` keeps `axoupdater` off until the
  GH-releases source-of-truth is wired. Re-evaluate after the
  first live tag push exercises the workflow.
- **CHANGELOG.md root entry vs per-crate.** release-plz generates
  per-crate `CHANGELOG.md` on first release-PR; root file is the
  bin's changelog plus an index. Watch for bullet-style drift —
  acceptable but not desirable.

### Phase 27.4 — Debian + RPM packages deferrals

- **Phase 27.4.b — signed apt/yum repos in GH Pages.** GPG key
  generation + management (encrypt private with `age`, store in
  GH secret, `crazy-max/ghaction-import-gpg@v6` to import in
  runner), repo metadata via `apt-ftparchive` + `createrepo_c`,
  GH Pages publish job (mirror release assets into `apt/` +
  `yum/` paths), `nexo-rs.repo` + `apt sources.list` snippets in
  docs, optional `curl ... | install.sh` bootstrap that auto-detects
  distro. Cosign keyless (Phase 27.3) covers per-asset integrity
  but does NOT satisfy apt/yum trust chains — GPG is a separate
  signing system. New sub-phase entry in `PHASES.md`.
- **`NEXO_BUILD_CHANNEL` drift in `.deb` / `.rpm` packages.** The
  binary inside the deb/rpm is the same musl-static one cargo-dist
  built for the tarball, so `nexo --version --verbose` reports
  `channel: tarball-x86_64-unknown-linux-musl` even when the user
  installed via `apt install ./*.deb` or `dnf install ./*.rpm`.
  Fixing requires a dedicated rebuild per package channel — costs
  ~3 min CI per channel. Accepted today; revisit if support tickets
  surface confusion about install provenance.
- **arm64 install-test via qemu.** Today the install-test matrix
  is x86_64-only. arm64 needs `docker/setup-qemu-action@v3` +
  `--platform linux/arm64` overhead (~3 min per image). Backlog
  until either CI cycle budget tightens or arm64-specific issues
  show up in the wild.
- **Snap / Flatpak.** Out of scope. Reconsider only if community
  asks. Both formats add their own packaging dance + sandbox
  semantics that don't match the system-service shape the deb/rpm
  ship today.
- **systemd boot smoke in CI.** Containers without systemd-as-pid-1
  fail `systemctl enable`. The install-test matrix only validates
  `nexo --version` + `nexo --help`. Real systemd start lives
  manually or in a future VM-based CI lane.

### Phase 82.10.h.b — admin RPC wire-path follow-ups

Phase 82.10.h.b shipped the full wire path (router + reader
routing + audit-tail CLI + `AdminRpcBootstrap` module) but two
items stayed deferred to keep the commit small:

- **Pairing notifier wire-up.** ✅ shipped 2026-05-02 in Phase
  82.10.h.b.pairing. `DeferredPairingNotifier` mirrors the
  `DeferredAdminOutboundWriter` deferred-bind pattern: built
  alongside the response writer, fed to
  `with_pairing_domain(_, Some(notifier))`, then bound to the
  live `mpsc::Sender<String>` post-`spawn_with` from the same
  call site (`PerMicroappWire::bind_writer`). Frames sent
  before bind warn-drop instead of panicking; tests cover
  drop-before-bind, post-bind delivery, and idempotent second
  bind. Microapps now receive `nexo/notify/pairing_status_changed`
  frames in real time without polling.
- **Operator wire-up: `None → Some(&bootstrap)` in
  `src/main.rs`.** ✅ shipped 2026-05-02 in Phase
  82.10.h.b.b.activate. Boot now does a pre-discovery pass
  to learn plugin roots, calls
  `nexo_setup::admin_capability_collect::collect_admin_capabilities`
  + `collect_http_server_capabilities` to surface
  `[capabilities.admin]` and `[capabilities.http_server]` from
  each `nexo-plugin.toml`, then constructs
  `AdminRpcBootstrap::build(...)` with the maps. Result is
  threaded into `run_extension_discovery` so admin RPC pipes
  are alive end-to-end. Reload signal stays a no-op closure
  for now (Phase 18 lands later); deeper integrations
  (`Some(broker)`, `Some(transcript_writer)`,
  `Some(processing_store)`, etc.) stay `None` because those
  types are constructed later in main.rs. Per-domain
  follow-ups thread the rest as the broker + writer + stores
  get hoisted (see "Per-domain main.rs threading" below).

### Per-domain main.rs threading (post-activate cleanup)

`AdminRpcBootstrap` is now constructed in main.rs but several
of its inputs default to `None` because the underlying state
(broker handle, transcripts writer, processing store, tenant
store, skills store, escalation store, agent event log,
firehose-side transcript_reader) is built later in the boot
sequence. Each unwired domain returns the typed
`<domain> not configured` -32603 from admin RPC; microapps
that probe see the negative result and degrade gracefully.

Closing each one is a one-line edit (hand the existing `Arc`
into the right `AdminBootstrapInputs` field) once the state is
hoisted ahead of the bootstrap call. Until then:

- Processing pause/intervention dispatch via admin RPC works
  the moment the runtime starts sharing a
  `ProcessingControlStore` with the bootstrap.
- Channel intervention `Reply` works the moment the broker
  handle is hoisted.
- Operator firehose backfill of non-transcript kinds — ✅
  shipped 2026-05-02 in Phase 82.11.log.thread. boot opens
  `SqliteAgentEventLog::open(state_dir/agent_events.db)` and
  hands `Some(log_arc)` to `agent_event_log` so
  `Tee([Broadcast, Log])` composes internally. Open failure
  warns + degrades to live-only, never blocks boot.
- Durable admin audit log — ✅ shipped 2026-05-02 in Phase
  82.10.h.b.b.audit-db. boot now passes
  `Some(state_dir/admin_audit.db)` to the bootstrap (was
  `None` → in-memory writer that lost rows on restart).
  Same path the `nexo microapp admin audit tail` CLI defaults
  to — operator queries land on the same file the daemon
  writes.

Still pending:
- `MergingAgentEventReader` wrap for `transcript_reader` —
  needs a `TranscriptReaderFs` instance. Boot doesn't
  currently construct one (transcript writer is per-agent;
  the reader builds against the same dir tree). One small
  helper away.
- `Some(processing_store)` thread — wait for the runtime
  hoist that surfaces a shared `Arc<dyn ProcessingControlStore>`.
- `Some(broker)` thread — wait for the broker connection
  hoist that surfaces `Some(AnyBroker)` ahead of bootstrap.
- Per-tenant retention sweep scheduler — wait for the audit
  sweep scheduler so both run from one place.

Each per-domain hoist is independent and can ship as its own
small commit. None require the bootstrap activation refactor
that already landed.

### Phase 82.11 — agent event firehose follow-ups

Phase 82.11 shipped the full pipeline (wire shapes + handlers
+ adapter + emitter + bootstrap subscribe wire + integration
test). Three follow-ups stayed deferred:

- **Operator wire-up: `transcript_reader: Some(...)` and the
  `event_emitter()` swap in `src/main.rs`.** The bootstrap
  field + accessor exist; `run_extension_discovery` already
  threads `AdminRpcBootstrap` through. Activating the firehose
  end-to-end needs three lines in `main()`: build a
  `TranscriptReaderFs` per agent, hand it to
  `AdminBootstrapInputs::transcript_reader`, and call
  `TranscriptWriter::with_emitter(bootstrap.event_emitter())`
  at writer construction. Same boot-order refactor as the
  82.10.h.b operator wire-up — folded into that follow-up
  rather than duplicated here.
- **NATS bridge variant of `AgentEventEmitter` for multi-host
  deployments.** ✅ shipped 2026-05-02 in Phase 82.11.bridge.
  `NatsAgentEventEmitter` impls `AgentEventEmitter` by
  publishing serialised `AgentEventKind` frames to
  `<prefix>.<agent_id>.<kind>` (default prefix
  `nexo.agent_events`). Subscribers route per-agent (`>`/
  `<prefix>.ana.>`), per-kind
  (`<prefix>.*.processing_state_changed`), or both at the
  broker. Subject derivation lives in the pure
  `agent_event_subject(prefix, &event)` fn so boot can
  validate routing without a live NATS client. agent_id is
  sanitised at emit-site (`.`/`*`/`>`/whitespace → `_`) so a
  malformed config can't break wildcard subscriptions.
  Composes with `Tee` so boot wires `[Broadcast, Sqlite,
  Nats]` together as a single `Arc<dyn AgentEventEmitter>`
  without changing emit-site signatures. Boot stitch is
  folded into 82.11.log.b (next main.rs operator wire-up).
  5 unit tests cover subject derivation per variant +
  custom-prefix override + agent_id sanitisation.
- **Future kinds beyond `TranscriptAppended`.** `AgentEventKind`
  is `#[non_exhaustive]` so adding `BatchJobCompleted` /
  `OutputProduced` / `Custom` is a non-breaking additive
  change. Each new kind needs (a) the variant in tool-meta,
  (b) the emit site in whatever subsystem produces it, and (c)
  optionally an FTS index for `agent_events/search` filtering.
  **Two new kinds shipped 2026-05-02**: `EscalationRequested`
  + `EscalationResolved` (Phase 82.14.b.firehose) and
  `ProcessingStateChanged` (Phase 82.13.b.firehose) — both
  emit on the existing `nexo/notify/agent_event` subject; no
  FTS change required (search remains TranscriptAppended-
  only).
- **82.11.log.b — main.rs activation.** Phase 82.11.log
  shipped the `SqliteAgentEventLog` primitive
  (read+write trait, SQLite impl, AgentEventEmitter sink).
  82.11.log.sweep shipped the retention sweep (2026-05-02).
  82.11.log.merge shipped the cross-source
  `MergingAgentEventReader` (2026-05-02). 82.11.log.compose
  shipped the boot-side composition (2026-05-02):
  `AdminBootstrapInputs.agent_event_log: Option<Arc<SqliteAgentEventLog>>`
  is now in place, and when `Some`, build composes
  `Tee([Broadcast, Log])` internally — emit-side wiring
  zero-cost from the perspective of every call site. Only
  deferred: **main.rs activation** — open the SQLite DB at
  `state_dir.join("agent_events.db")` and pass it as the
  field, AND wrap `transcripts_fs` in
  `MergingAgentEventReader::new(transcripts_fs, log)` for
  the `transcript_reader` field so backfill returns durable
  kinds. Boot scheduler also calls
  `sweep_retention(retention_days, max_rows)` on the same
  cadence as the audit-log sweep (defaults 90d / 100k rows).
  Folds with the same boot-order refactor as the other 82.x
  operator wire-ups.

Target phase: 82.10.h.c (folded with 82.10.h.b's main.rs
wire-up) for the operator wire-up; 82.11.log.b for the boot
+ retention + cross-source merge; future phases for the
NATS bridge + new kinds.

### Phase 82.12 — http_server capability follow-ups

Phase 82.12 shipped the building blocks (manifest field +
boot supervisor + bind policy + INVENTORY + token-hash
helper). Two follow-ups stayed deferred:

- **main.rs operator wire-up**: thread `HttpServerSupervisor`
  + the `http_server_capabilities` map into
  `AdminRpcBootstrap` from `main()`. The bootstrap accepts
  the field; activating it is the same boot-order refactor
  as 82.10.h.b / 82.11 (one shared `boot_setup` pass that
  reads every plugin.toml once). Folded into the same
  follow-up — when main.rs gets its single wire-up commit,
  http_server lands alongside.
- **Token rotation trigger**: framework ships `TokenRotated`
  shape + `token_hash` helper, but no code currently calls
  `dispatcher.notify(token_rotated, ...)` — the trigger needs
  a Phase 18 reload-coordinator hook that detects
  `<token_env>` change. Microapps that need rotation today
  must restart. Target phase: alongside the operator
  wire-up, since both depend on the boot reload coordinator.

### Phase 82.13 — operator processing pause follow-ups

Phase 82.13 shipped the wire shapes + store + admin RPC
handlers but four pieces are deferred:

- **Inbound dispatcher hook**: paused conversations should
  log inbounds via 82.11 firehose without firing an agent
  turn. **✅ shipped 2026-05-02 as Phase 82.13.c**.
  Runtime gained `with_processing_store(store)` +
  `with_event_emitter(em)` builders;
  `runtime.rs:780-890ish` (right after `let message_id =
  msg.id`) checks the per-scope state, redacts the body,
  pushes onto the per-scope queue, and emits a firehose
  drop event when the cap evicts. Boot
  (`AdminBootstrapInputs.processing_store`) shares ONE
  `Arc` between the admin RPC dispatcher + every runtime
  so a pause RPC reaches the inbound loop on the next
  message. Fail-open on store errors (broken store doesn't
  freeze the inbound loop). 6 integration tests cover
  buffer-on-pause, passthrough-on-active, fail-open,
  redaction-before-push, cap-eviction firehose, unwired
  legacy. Boot activation in `src/main.rs:1228` still
  hardcoded to `None` — depends on the broader Phase
  82.10.h.b.b boot-order refactor (gates ALL admin RPC).
  Once that lands, the round-trip works without further
  changes.
- **`InterventionAction::Reply` outbound**: ✅ partially shipped
  2026-05-02 in Phase 82.13.b.firehose. Channel send already
  flows through `ChannelOutboundDispatcher` (Phase 83.8.4.a)
  and the transcript stamp already lands as
  `Assistant + sender_id "operator:<hash>" + source_plugin
  "intervention:<channel>"` (Phase 82.13.b.1) — `TranscriptRole`
  has no `Operator` variant by design (the agent reads operator
  replies as Assistant for context coherence, the operator
  prefix on `sender_id` disambiguates). What just landed is
  the missing firehose emit:
  `AgentEventKind::ProcessingStateChanged { agent_id, scope,
  prev_state, new_state, at_ms, tenant_id }` is emitted from
  `processing/pause` + `processing/resume` whenever the
  transition is a real flip (idempotent retries skip the
  emit). Reply (intervention) does not emit
  ProcessingStateChanged — state stays paused — but the
  TranscriptAppended emit on the operator stamp already gives
  subscribers a real-time signal of operator activity. Still
  deferred: per-tenant `tenant_id` look-up at emit time
  (currently `None`); folds into the same boot-order refactor
  that surfaces tenants from agents.yaml.
- **Auto-resolve hook for 82.14**: pausing a scope with a
  pending escalation that targets it auto-flips the
  escalation to `OperatorTakeover`. Lands when 82.14 ships.
- **SQLite-backed durable store**: v0 is in-memory; daemon
  restart drops every pause. Trait + handler are
  store-agnostic so the new impl drops in alongside
  `InMemoryProcessingControlStore`.

Target phase: 82.13.b (chat-takeover wire-up + reply
adapter) and 82.13.c (durable store), folded with the next
main.rs operator wire-up commit.

### Phase 82.14 — escalation tool follow-ups

Phase 82.14 shipped the wire shapes + store + admin RPC
handlers + the auto-resolve hook on 82.13 pause. Four
follow-ups stayed deferred:

- **`escalate_to_human` built-in tool**: register in
  core ToolRegistry as a provider-agnostic / use-case-
  agnostic tool. Dispatch must derive `ProcessingScope`
  from the agent's `BindingContext` (82.1) + scope
  context (chat → contact_id, batch → job_id) so the
  agent passes only `{summary, reason, urgency, context}`
  and the framework fills in scope. Wire-up depends on
  the same boot-order refactor as 82.10.h.b /
  82.11 / 82.12 / 82.13.
- **Firehose event variants**: emit `EscalationRequested
  { agent_id, scope, summary, reason, urgency, context,
  requested_at_ms }` and `EscalationResolved { agent_id,
  scope, resolved_at_ms, by }` on the 82.11 firehose
  when the tool fires / resolve handler runs. Notify-kind
  literals already pinned in tool-meta.
- **Throttle**: max 3 escalations per scope per hour to
  prevent agent loops. Token-bucket from Phase 82.7 reused;
  trait + handler unchanged.
- **SQLite-backed durable store**: v0 is in-memory,
  daemon restart drops every escalation. Trait +
  handler are store-agnostic so the new impl drops in
  alongside `InMemoryEscalationStore`.

Target phase: 82.14.b (built-in tool + firehose event
variants + throttle) and 82.14.c (durable store), folded
with the next main.rs operator wire-up commit.

### Phase 82.6 — state_root env injection follow-up

✅ Shipped 2026-05-02 in Phase 82.6.b. `build_command` in
`crates/extensions/src/runtime/stdio.rs` now calls
`crate::state::ensure_state_dir(extension_id)` and stamps
`NEXO_EXTENSION_STATE_ROOT` onto the child process env so
microapp boot points its on-disk state (SQLite DBs, vault
files, per-tenant artifacts) at the canonical location
(`$NEXO_HOME/extensions/<id>/state`) without reimplementing
the path layout. Idempotent mkdir; failure surfaces as a
warn rather than a spawn error so a permission misconfig
flags loudly without taking down every extension at boot.
Did NOT need the broader 82.10.h.b.b boot-order refactor
because `build_command` already had `extension_id` in
scope — env injection lives at the spawn site, not in
main.rs's bootstrap code. 1 new test
(`build_command_stamps_state_root_env_pointing_at_per_extension_dir`)
confirms the env var lands on the spawned `Command` and
points at the per-extension dir.

### Phase 83.14 — actual crates.io upload + release-plz CI + npm

Phase 83.14 shipped publish-readiness for the four Tier-A
crates: clean dry-run on `nexo-tool-meta`,
`nexo-plugin-manifest`, `nexo-compliance-primitives`. Per-crate
README.md + CHANGELOG.md present. Publishing doc
(`docs/src/microapps/publishing.md`) covers the dependency
order. Three pieces deferred to 83.14.b:

- **Actual crates.io upload**: operator runs the documented
  publish sequence on tag day. `nexo-microapp-sdk` is gated on
  `nexo-tool-meta` propagating to crates.io first (chicken-
  and-egg between path-dep and registry index).
- **release-plz CI integration**: `.github/workflows/publish.yml`
  triggers on `v*.*.*` tags, reads `CARGO_REGISTRY_TOKEN`,
  walks the publish order with index-propagation waits between
  steps.
- **npm package `@nexo/microapp-ui-react`**: lands when 83.13
  ships the React component library. Until then there is
  nothing to publish to npm.
- **out-of-tree consumer migration** (agent-creator from
  path-dep to versioned `0.1` deps) — straightforward
  Cargo.toml edit on the consumer side post-publish.

Target phase: 83.14.b (folded with the v0.1.0 tag day).

### Phase 83.11 — walkthrough docs + admin-ui PHASES entries

Phase 83.11 shipped three docs pages
(`getting-started.md`, `templates.md`, `compliance-primitives.md`)
linked from SUMMARY.md. Two pieces deferred to 83.11.b:

- **`ventas-etb-walkthrough.md`** — annotated full source of
  the reference microapp line-by-line. Lands when 83.8
  (ventas-etb) ships its source.
- **`meta-microapp-walkthrough.md`** — annotated source of the
  agent-creator microapp covering admin RPC + transcript
  firehose + HTTP server hosting. Lands when agent-creator
  out-of-tree repo gets a docs revision.
- **6 admin-ui `PHASES.md` tech-debt entries**: microapp
  registry panel, persona config inspector, compliance event
  feed, microapp doctor, microapp admin audit viewer,
  microapp HTTP health dashboard. Defer with the next
  admin-ui repo touch (no admin-ui work scheduled).

Target phase: 83.11.b (folded with 83.8 source-doc walkthrough
+ next admin-ui sweep).

### Phase 83.15 — MockAdminRpc + reference test + docs

Phase 83.15 already had `MicroappTestHarness::call_tool*` /
`fire_hook` (shipped in 83.4); this turn added
`MockBindingContext` builder + 7 tests covering minimal /
account-less / account-with / session / mcp-channel /
panic-when-no-agent / harness-integration. Three pieces deferred
to 83.15.b:

- **`MockDaemon`**: full async stub that owns an in-memory
  JSON-RPC transport and lets tests push synthetic
  `agents/updated` / `hooks/<name>` notifications. Today the
  harness drives a `Microapp` builder directly without
  simulating the daemon side; richer integration tests need
  the bidirectional mock.
- **`MockAdminRpc`**: programmable responses to `nexo/admin/*`
  requests so microapps consuming admin surfaces can assert
  request shape + handle response. Land alongside the
  `admin` Cargo feature's request side.
- **Reference test** in `extensions/template-microapp-rust/`
  demonstrating the harness end-to-end (1 unit test per tool +
  1 integration test booting `MockDaemon`).
- **Docs page** `docs/src/microapps/testing.md` with a 50-line
  worked example.

Target phase: 83.15.b (folded with the next SDK feature touch).

### Phase 83.17 — CLI integration + derive macro + integration test

Phase 83.17 shipped the validator (`validate_config(config,
schema)`) + skip-env helper + 11 unit tests in
`nexo-plugin-manifest`. Three pieces deferred to 83.17.b:

- **CLI integration**: `nexo extensions install <id>` reads
  `extensions/<id>/config.schema.json` (when present), parses
  the operator-supplied `extensions_config.<id>` from
  `agents.yaml`, runs the validator, aborts install on
  failures with a structured CLI error rendering the JSON
  pointer + message of each failure.
- **Boot pre-flight**: same validation at daemon boot (re-runs
  on hot-reload) — fails fast before spawning the microapp.
- **`#[derive(MicroappConfig)]` macro**: auto-derive a JSON
  Schema from a typed Rust config struct (uses `schemars`).
  Lands in `microapp-sdk-rust` as a proc-macro crate so
  authors don't write JSON Schema by hand.
- **Integration test**: `nexo extensions install` fails clean
  on a deliberately bad config + succeeds on a corrected one.
- **Docs page**: `docs/src/microapps/config-schema.md`
  authoring guide + derive macro walkthrough.

Target phase: 83.17.b (folded with the next CLI / extensions
install touch).

### Phase 83.16 — supervisor emit + admin-ui badge + counter

Phase 83.16 shipped the `MicroappError` wire shape (kinds enum,
payload struct, builder helpers) + 6 unit tests in
`nexo-tool-meta`. Five pieces deferred to 83.16.b:

- **Daemon supervisor emit**: the stdio supervisor in
  `crates/setup/` (or `crates/extensions/`) detects the four
  error categories and publishes
  `nexo/notify/microapp_error` on the broker. Folds with the
  next supervisor boot-order touch.
- **Admin-ui health badge**: per-microapp badge that flips red
  on first error in last 5 min, summary on hover. Defer to
  the next admin-ui sweep.
- **Counter `microapp_errors_total{microapp_id, kind}`**: emit
  via Phase 28 metrics infra in the same supervisor commit as
  the broker publish.
- **Audit log entry `nexo.audit.microapp_error{...}`** —
  publish on broker so external observers (Phase 39 stable
  admin API) see the same signal.
- **Respawn rate limit** (>50 errors / 5 min) → daemon emits
  `MicroappBackoff` event and stops respawning until operator
  clears the badge. New variant for `MicroappErrorKind`
  (`#[non_exhaustive]` makes this non-major).

Target phase: 83.16.b (folded with next supervisor touch).

### Phase 83.3 — dispatch enforcement + audit log + integration test

Phase 83.3 shipped both wire halves:
- SDK side: `HookOutcome::{Block, Transform}` variants + helpers
  + dual-shape serialiser (legacy `abort:bool` + new `decision`).
- Daemon side: `HookResponse.{decision, transformed_body,
  do_not_reply_again}` fields + parser test coverage.

Three pieces deferred to 83.3.b:

- **Dispatch enforcement**: today the daemon's hook-runner
  collects votes but doesn't yet act on `decision: "transform"`
  (still uses the legacy `override_event` path) or
  `do_not_reply_again`. The vote-to-block path via legacy
  `abort:true` already short-circuits dispatch. Wiring needs:
  - `Transform` decision: the host applies `transformed_body`
    in place of the original inbound body (subject to operator
    policy) and audit-logs the diff.
  - `do_not_reply_again`: cancel pending auto-replies for the
    conversation (anti-loop signal).
- **Audit log row** for every applied block / transform —
  emit on the broker so admin-ui + Prometheus see who voted
  what. Same shape as the existing 82.10 admin audit log.
- **Fail-open with warn**: when a hook subprocess crashes /
  times out / returns malformed JSON, the dispatcher MUST
  proceed (never silently fail-closed) and log a `warn!`. Spec
  says "defense in depth"; today the legacy `default()` path
  is fail-open but the new vote semantics need the same
  guarantee.
- **Integration test**: 3 scenarios — block short-circuits +
  audit row; transform rewrites body + audit row; malformed
  hook response fails-open with warn.

Target phase: 83.3.b (folded with the next agent dispatcher
boot-order touch).

### Phase 83.2 — SkillLoader merge + integration test

Phase 83.2 shipped the manifest schema (`Capabilities.skills`)
and the validation helper (`validate_contributed_skills` —
slug rule + filesystem existence check) + 8 unit tests. Two
pieces deferred to 83.2.b:

- **SkillLoader merge**: the daemon's existing skill-loading
  path (today reads only `agents.yaml.skills_dir`) must
  auto-discover skills from each loaded extension's
  `<plugin_root>/skills/<name>/SKILL.md` and merge them into
  any agent that lists the extension under
  `agents.yaml.<id>.extensions: [...]`. Operator-declared
  `skills_dir` still wins on name collision (operator
  override > extension contribution).
- **Integration test**: extension `ventas-etb` ships
  `skills/ventas-flujo/SKILL.md`, the agent declares
  `skills: [ventas-flujo]` without `skills_dir`, the loader
  resolves the skill from the extension. Plus a name-collision
  test verifying operator override.

Target phase: 83.2.b (folded with the next agent-boot skill-
loader touch).

### Phase 83.1 — JSON-RPC propagation + hot-reload + integration test

Phase 83.1 shipped the `AgentConfig.extensions_config: BTreeMap<String, serde_yaml::Value>`
field (with `#[serde(default)]` back-compat) + 2 YAML round-trip tests +
all literal-construct sites updated. Three pieces deferred to 83.1.b:

- **JSON-RPC `initialize` propagation**: the daemon's microapp
  spawn loop must thread `agents_config: { <agent_id>: <config> }`
  into the `initialize` params so the microapp builds its
  `HashMap<agent_id, Config>` lookup on startup.
- **`agents/updated` notification on hot-reload**: when Phase 18
  hot-reload picks up a YAML change affecting a binding, fire
  `agents/updated` to the affected microapps so the in-process
  map refreshes within 1 s without dispatch interruption.
- **Integration test**: 3 personas in `agents.yaml` map to 3
  distinct configs visible to the same subprocess. Hot-reload
  changes one persona's config and asserts the microapp's map
  reflects it without restart.
- **SDK helper**: `ToolCtx::extension_config()` lookup that
  reads the per-agent slice indexed by `BindingContext.agent_id`.

Target phase: 83.1.b (folded with the next microapp-spawn
boot-order touch).

### Phase 87.1 — JudgeBackend wire-up + budget axis + telemetry

Phase 87.1 shipped `AcceptanceCriterion::LlmJudge` variant +
`LlmJudgeEvaluator` + `JudgeBackend` trait + 11 unit tests
covering pass/fail/malformed/timeout + criterion routing. Four
pieces deferred to 87.1.b:

- **Production `JudgeBackend` impl**: dispatch via
  `nexo-fork::DefaultForkSubagent` with the judge persona prompt
  loaded as a Markdown asset (`crates/driver-loop/src/evaluators/llm_judge_prompt.md`,
  `include_str!`). Today the trait is wired only to scripted
  test backends.
- **Budget guard axis**: add `BudgetGuards.max_judge_calls_per_goal`
  (default 5) + `BudgetUsage.judge_calls` counter +
  `BudgetAxis::JudgeCalls` so a runaway judge loop is bounded.
  Skipped from 87.1 because it requires touching every
  `BudgetGuards { ... }` literal site (same disruption as the
  85.1 `consecutive_413` change). Bundle with the next budget
  axis sweep.
- **Integration test**: `crates/driver-loop/tests/` worker emits
  diff → criterion = LlmJudge → mocked judge returns pass → goal
  accepted. Repeat with judge returning fail → orchestrator
  emits `AcceptanceFailure` per Phase 67/68 contract. Today the
  default evaluator's LlmJudge arm returns an explicit "not yet
  wired" failure so the criterion isn't silently passed.
- **Telemetry**: counter `acceptance_llm_judge_total{verdict}`
  + histogram `acceptance_llm_judge_latency_seconds`. Lands with
  the production backend so the metric reflects real fork
  dispatches.

Target phase: 87.1.b (folded with the next fork-as-tool wiring
sweep that Phase 84.3 also depends on).

### Phase 86.1 — fire-site wiring + integration test + docs page

Phase 86.1 shipped the type surface in `crates/memory/src/metrics.rs`
(4 metric families + render_prometheus + 9 unit tests). Three
pieces deferred to 86.1.b:

- **Fire-site wiring**: emit calls in
  - `crates/memory/src/long_term.rs::remember_typed` →
    `record_write(agent_id, type)`.
  - `crates/memory/src/long_term.rs::recall*` (every public recall
    fn) → `record_recall(agent_id, scope, available, selected)`
    + per-memory `record_age_at_recall(seconds)`.
  - `crates/driver-loop/src/extract_memories.rs::store_extracted`
    → `record_write_size(bytes)`.

- **Integration test**: `crates/memory/tests/` write 5 memories of
  mixed types → recall → assert all 4 metric families recorded
  with expected label sets.

- **Docs page**: `docs/src/operations/memory-observability.md` —
  metric inventory + sample Grafana panel JSON for "memory
  health" with selection-rate trend, write-volume by type, age
  histogram.

- **admin-ui sync**: "Memory observability" panel checkbox in
  `admin-ui/PHASES.md` (folded with the broader admin-ui defer
  pile).

Target phase: 86.1.b (folded with the broader long_term recall
sweep + Phase 28 metrics aggregator wire-up).

### Phase 85.2 — orchestrator + provider integration

Phase 85.2 shipped the type surface (MicroCompactPolicy trait,
DefaultMicroCompactPolicy, CompactSummary.cache_pin_keys +
truncated_tool_results, TruncatedToolResult,
TIME_BASED_MC_CLEARED_MESSAGE marker constant) + 10 unit tests
covering policy decisions and serde back-compat. Three pieces
deferred to 85.2.b:

- **Orchestrator wire-up**: per-turn `MicroCompactPolicy::classify`
  call before request body assembly. When triggered, splice the
  marker into the tool result and append a `TruncatedToolResult`
  to the next `CompactSummary`. Idempotency: dedupe by `call_id`
  across consecutive compacts so the same result isn't marked
  twice.

- **Provider client integration**: `crates/llm/src/anthropic` and
  `crates/llm/src/minimax` request builders honor
  `cache_pin_keys` — prepend `cache_control: { type: "ephemeral" }`
  breakpoints at the pinned positions so the provider preserves
  the cached prefix across compact passes.

- **Integration test**: `crates/driver-loop/tests/` two consecutive
  compacts on the same goal — assert (a) the same call_id is not
  double-marked, (b) the cache_pin_keys persist through a daemon
  restart via CompactSummaryStore.

- **Telemetry**: `compact_micro_truncated_bytes_total` counter +
  `compact_micro_cache_hit_ratio` gauge for the Phase 28
  Prometheus surface.

Target phase: 85.2.b (folded with broader provider request-builder
sweep).

### Phase 85.1 — provider 413 detection + integration test

Phase 85.1 shipped the type surface (LlmError::PromptTooLong,
BudgetAxis::Consecutive413, ReplayDecision::CompactAndRetry) and
the orchestrator branch that bumps `consecutive_413` on the
classifier verdict. Three pieces deferred to 85.1.b:

- **Provider 413 detection**: `crates/llm/src/anthropic` and
  `crates/llm/src/minimax` (and future providers) intercept HTTP
  413 responses + extract `tokens_used`/`tokens_limit` from the
  provider's error body, return `LlmError::PromptTooLong` instead
  of generic `ServerError { status: 413 }`. Today the classifier
  routes via `error_message.contains("prompt too long")` /
  variants — this works when the provider's body text reaches
  the orchestrator, but a typed variant is more robust.

- **Forced compact via `Trigger::Reactive413`**: the orchestrator
  arm currently bumps the counter and re-loops, expecting the
  proactive compact policy to fire on the next turn. The spec
  calls for an explicit `Trigger::Reactive413` that bypasses the
  proactive estimator (proactive may have under-counted, that's
  why we got 413 in the first place). Add the variant to
  `CompactTrigger`, plumb it through the orchestrator → compact
  policy contract.

- **Integration test**: `crates/driver-loop/tests/` mock provider
  returns 413 once, then succeeds; assert one compact + one
  successful turn + transcript shows the compact marker between
  attempts.

Target phase: 85.1.b (folded with the broader provider error
typing sweep).

### Phase 84.5 — admin-ui "Agent role" panel

Phase 84.5 shipped CHANGELOG entries for 84.1-4 + cross-link from
multi-agent-coordination.md. Deferred:

- **`admin-ui/PHASES.md` "Agent role" panel** — per-binding role
  view + active persona indicator (coordinator / worker / unset).
  Defer until next admin-ui repo touch (no admin-ui work
  scheduled in current phases).

Target phase: folded with the next admin-ui sweep (same shape
as the 82.9 admin-ui defer).

### Phase 84.3 — fork-as-tool spawn pipeline + transcript resume

Phase 84.3 shipped the `WorkerRegistry` trait + `InMemoryWorkerRegistry`
+ `SendMessageToWorkerTool` with all four spec error scenarios
covered (24 tests). Deferred:

- **Producer side: fork-as-tool spawn pipeline** — the coordinator-
  side wrapper that turns a `TeamCreate` (or analogous) tool call
  into a forked subagent run, registers it as `Running` in the
  WorkerRegistry, and on exit upserts the snapshot with
  `Completed`/`Terminated` plus the message-count from the fork's
  final `messages` vec. Without this, the registry is never
  populated by real usage; today only test code calls `upsert`.

- **Consumer side: transcript resume execution** — when
  `SendMessageToWorker` returns `Continued`, the actual work of
  loading the worker's prior `messages`, appending the operator-
  supplied `message` as a new user turn, running another fork-loop
  turn, and emitting the resulting `<task-notification>` (via
  `ForkResult::to_task_notification` from 84.2) into the
  coordinator's session. The success path's `pipeline_pending:
  true` flag exists so a coordinator can verify the request was
  accepted while this consumer is still under construction.

- **Integration test**: spawn → notification → continue → resumed
  session sees prior tool calls in transcript. Spec calls out
  this as a 84.3 done criterion; today the producer + consumer
  pipeline doesn't exist as a single end-to-end path, so the
  integration test lands when both halves above ship.

Target phase: 84.6 (or wherever fork-as-tool wraps `TeamCreate`).
Folds with the broader fork-spawn-pipeline that emerges around the
worker-persona sub-phase (84.4) when the worker is no longer just
a peer-broker entity.

### Phase 84.2 — task-notification consumer wire-up

Phase 84.2 shipped the `TaskNotification` type (driver-types) +
`ForkResult::to_task_notification` / `fork_error_to_task_notification`
producer helpers (fork). The piece deferred:

- **Consumer wire-up** — the bridge from a fork outcome to the
  coordinator's session as a rendered `<task-notification>` block
  in the next user turn. The fork-pass + TeamCreate exit paths
  do not exist as standalone code today; they emerge naturally
  inside Phase 84.3 (`SendMessageToWorker` continuation tool +
  related fork-as-tool wrapping). Until 84.3 lands, no consumer
  needs the producer helpers — the type is staged, the producers
  are tested, the consumer wires up alongside the tool that needs
  it.

Target phase: 84.3 (folded into the SendMessageToWorker
implementation).

### Phase 82.9 — reference template + admin-ui follow-ups

Phase 82.9 shipped the multi-tenant SaaS walkthrough doc
(`docs/src/extensions/multi-tenant-saas.md`) connecting all
Phase 82 primitives. Two pieces deferred:

- **`extensions/template-saas/` in-tree scaffold** — the
  out-of-tree `agent-creator` microapp (Phase 83.10) is the
  working SaaS reference today, so an in-tree template would
  duplicate maintenance. Re-evaluate once 83.x microapp work
  starts: either promote `agent-creator` to in-tree at
  `extensions/template-saas/`, or strip the scaffold to a
  minimal `plugin.toml + JSON-RPC stub` and let the doc
  walkthrough point at `agent-creator` for the full shape.

- **`admin-ui/PHASES.md` tech-debt entries** — webhook
  receiver panel, per-binding rate-limit panel, per-tenant
  audit filter, BindingContext-aware tool inspector. Defer
  until the next admin-ui repo touch (no admin-ui work
  scheduled in current Phase 82/83).

Target phase: 82.9.b (admin-ui sync) — fold with admin-ui
sweep when a panel needs to ship.

### Phase 82.8 — multi-tenant audit follow-up

Phase 82.8 shipped the schema + filter; one piece is
deferred:

- **`event_forwarder.rs::AttemptResult → TurnRecord`
  builder threads `account_id`** from the active
  `BindingContext` (Phase 82.1). Today the writer hard-codes
  `account_id: None`, so live writes don't populate the
  column. Persistence layer is correct (`tail_for_account`
  returns matching rows; `tail` returns everything for
  operator scope), but until the forwarder threads the
  value, multi-tenant SaaS callers see empty tenant tails on
  fresh data. Same boot-order refactor as the rest of 82.x's
  deferreds — folded with main.rs operator wire-up.

### Phase 82.13.b — IA awareness during/after operator takeover

Cristian asked 2026-05-02 "¿la IA sabe de la conversación
después del resume?". Today's behaviour:

- Pre-pause history: agent transcript persists. ✅
- During pause: inbound user messages are SKIPPED (Phase 82.13
  contract: "agent skips inbounds while paused"). ❌
- During pause: operator replies via `intervention.Reply` reach
  the user via outbound but are NOT stamped in the agent's
  transcript. ❌
- After resume: agent has zero context of what happened during
  the takeover — neither user messages nor operator replies. ❌

Three improvements close the gap (each independent, can ship
incrementally):

1. **Stamp operator replies in transcript.** When
   `processing/intervention` Reply is dispatched, append a
   `TranscriptEntry { role: Assistant, content: body,
   sender_id: Some("operator"), ... }` to the active session.
   Requires a "current-session-for-scope" lookup
   (`TranscriptsIndex` extension or active-session map). Agent
   reads its own transcript on next turn and sees the operator's
   words as if it had said them. **✅ shipped 2026-05-02 as
   82.13.b.1** — `TranscriptAppender` trait + handler hook +
   `TranscriptWriterAppender` production adapter +
   `SendReplyArgs.with_session()` SDK helper. The microapp
   passes the active `session_id` in `intervention` params; the
   daemon stamps `role: Assistant`, `source_plugin:
   "intervention:<channel>"`, `sender_id: "operator:<hash>"`.
   Ack carries `transcript_stamped: Some(bool)`. Production
   wire-up at boot when `AdminBootstrapInputs.transcript_writer`
   is `Some`.
2. **Buffer inbounds during pause.** Instead of dropping inbounds
   when `ProcessingControlState::PausedByOperator`, store them
   in `pending_inbounds` on the state row. On resume, replay them
   as synthetic User entries in the transcript. Agent sees what
   the user said while it was paused. **✅ shipped 2026-05-02
   end-to-end** —
   - 82.13.b.3 (drain side): `PendingInbound` wire shape +
     `ProcessingControlStore.{push_pending,drain_pending,
     pending_depth}` + `InMemoryProcessingControlStore` queue
     with FIFO cap (`NEXO_PROCESSING_PENDING_QUEUE_CAP`,
     default 50) + `AgentEventKind::PendingInboundsDropped`
     firehose variant + resume drain stamps each as a `User`
     transcript entry with original timestamps.
   - 82.13.c (push side): runtime intake hook in `runtime.rs`
     calls `push_pending` when a scope is paused, redacts
     body before push, fail-open on store errors, fires
     drop event via firehose when cap evicts. Shared `Arc`
     between admin RPC dispatcher + runtime via
     `AdminBootstrapInputs.processing_store`.
   Round-trip works once `src/main.rs:1228` gets the boot-
   order refactor that activates `AdminRpcBootstrap`.
3. **`HumanTakeover::release(summary_for_agent)` end-to-end.**
   The `summary_for_agent` parameter exists in the SDK
   (Phase 83.8.6) but the daemon side never injects. When wired,
   the operator's free-form summary lands as a `System` entry
   ("operator summary: …") right before the next agent turn.
   Most flexible — operator can synthesise context the agent
   needs without forcing a literal replay. **✅ shipped 2026-05-02
   as 82.13.b.2** — `ProcessingResumeParams.session_id` +
   `summary_for_agent` wire fields, handler validates (empty /
   > 4096 chars / session_id_required), best-effort stamp as
   `role: System` content `[operator_summary] <body>` with
   `source_plugin: "intervention:summary"`. SDK
   `HumanTakeover::with_session(id).release(Some(summary))`
   forwards both. Validation runs BEFORE state flip so a
   rejected call keeps the pause; appender errors leave the
   scope Active and surface via `transcript_stamped: false`.

Order of value: #1 (highest, ~1.5 commits) > #3 (~1 commit) >
#2 (highest framework refactor, ~3 commits — needs pending
inbound queue + replay machinery).

Not blocker for the agent-creator v1 microapp UI: takeover
already works end-to-end (operator message reaches the user via
the channel plugin). The agent just resumes "blind" from the
last pre-pause turn. Phase 2 SaaS UX polish.

### Phase 83.8.4.c — outbound_message_id correlation ack flow

Plugin outbound dispatchers (`crates/plugins/whatsapp/src/dispatch.rs`
and friends) are fire-and-forget over the broker — they do not
ack with the channel-side message id. Phase 83.8.4.b ships
`OutboundAck { outbound_message_id: None }` for v0; the operator
UI cannot correlate "I sent X" with "agent's transcript shows X
landed at message-id Y". Closing this needs a per-plugin reply
pattern (broker request/response or correlation-id back-channel).
Standalone follow-up; not blocking takeover UX.

### Phase 83.8.4.b.gen — plugin-owned ChannelPayloadTranslator

Today translators (WhatsApp / Telegram / Email) live in
`nexo-setup::admin_adapters` and `setup` re-exports + registers
each at boot. Adding a new channel = edit `setup` again. Future
work: move `ChannelPayloadTranslator` trait to `nexo-tool-meta`
and let each plugin crate own its translator (next to its
existing `dispatch.rs` outbound subscriber). Boot auto-discovers
via inventory crate or explicit registration list. Result:
adding a new channel becomes zero-touch on `nexo-setup`.

Not blocking — current setup-side composition is fine for the
3 channels shipped. Reopen if/when a 4th channel arrives.

### Phase 83.8.12 — multi-empresa framework primitive

Decision 2026-05-02: 1 daemon hosts N empresas (was: 1 daemon =
1 empresa, manual provisioning). The microapp manages every
empresa from a single daemon. Requires a new framework concept
`empresa_id` that sits above the existing `account_id`
(`account_id` is the channel-side discriminator — WhatsApp phone
number — not the SaaS tenant).

Scope (when this sub-fase opens):

- `nexo-tool-meta::admin::empresas` — wire shapes:
  `EmpresaSummary`, `EmpresaDetail`, `EmpresasListResponse`,
  `EmpresasUpsertInput`, `EmpresasDeleteParams`, plus the
  `empresa_id` field on `BindingContext`.
- `nexo-core` admin RPC domain `nexo/admin/empresas/*` with an
  `EmpresaStore` trait. Production adapter writes to
  `empresas.yaml` (or extends `agents.yaml` with an `empresa_id`
  field per agent).
- Filter every multi-tenant-aware admin RPC by `empresa_id`:
  `agents/list`, `agent_events/list`, `escalations/list`.
- LLM providers: scoped per-empresa (each empresa has its own
  `${ENV_VAR}` keys) — operator UI surfaces a per-empresa key
  vault. Global providers stay possible for the operator's own
  use.
- Microapp tools: `empresa_create`, `empresa_list`,
  `empresa_get`, `empresa_update`, `empresa_delete`,
  `empresa_set_active`. Existing `agent_*` tools gain an
  `empresa_id` filter argument.
- Audit log (Phase 82.10.h) gains an `empresa_id` column for
  cross-empresa observability.

**MUST land before** the UI sub-fases 83.12 / 83.13 — the UI
treats empresa as a first-class entity ("create empresa →
inside, create agent → assign channel + LLM").

Cross-references:
- `project_microapp_is_saas_meta_creator.md` constraint #7 was
  REVISED.
- `project_ui_whatsapp_web_react.md` UI scope clarification.

**Status 2026-05-02** — naming + sub-step ship log:

- Code identifier is `tenant_id` (not `empresa_id`). Decision:
  the framework already ships `tenant` since Phase 76.3/76.4
  (MCP auth `TenantId`, JWT claim, static-token `with_tenant`)
  and `crates/memory-snapshot/` (bundle path
  `<state_root>/<tenant>/<agent_id>/`, CLI `--tenant` flag,
  `MemoryMutationHook::on_mutation(agent_id, tenant, ...)`).
  Renaming would break JWT claim contract, CLI flag, and
  bundle layout for existing operators. UI may surface
  "Empresa", "Workspace", "Division", etc. — `tenant_id` is
  the technical handle, not a product noun. Single-tenant
  deployments default to `"default"` or omit `tenant_id`
  entirely (every field is `Option<String>` with
  `#[serde(default, skip_serializing_if = "Option::is_none")]`).
- 83.8.12.1 ✅ wire shapes + `BindingContext.tenant_id` +
  `AgentConfig.tenant_id` (commit dd40fa8 + rename 5b45273).
- 83.8.12.2 ✅ tenants domain handler + `tenants_crud`
  capability + INVENTORY (commit 62858ab).
- 83.8.12.3 ✅ `TenantsYamlPatcher` adapter (commit 780c3c5).
- 83.8.12.4 ✅ `AgentsListFilter.tenant_id` +
  `AgentEventsListFilter.tenant_id` +
  `EscalationsListParams.tenant_id` +
  `AgentEventKind::TranscriptAppended.tenant_id` wire shapes.
  `agents/list` handler honours the filter via
  `agent_tenant_id()` helper that reads
  `agents.yaml.<id>.tenant_id`. Defense-in-depth: agents
  without `tenant_id` filter out under any non-`None`
  request (no leak of existence).
- 83.8.12.5 ✅ LLM providers per-tenant — `LlmConfig.tenants`
  + `TenantLlmConfig.providers` with serde-default empty
  hashmap, `LlmConfig::resolve_provider(tenant_id, name)`
  tenant-first/global-fallback, `LlmRegistry::build_for_tenant`,
  admin RPC `llm_providers/{upsert,delete}` route to tenant
  namespace when `tenant_id.is_some()`, `LlmYamlPatcher`
  trait gains 4 tenant methods, `LlmYamlPatcherFs` overrides
  via `tenants.<tid>.providers.<pid>.*` yaml path. Cron LLM
  build still on legacy `build()` shim — separate scope
  (83.8.12.5.cron).
- 83.8.12.6 ✅ Skills per-tenant layout —
  `<root>/{__global__,<tenant_id>}/<name>/SKILL.md`. All 4
  `SkillsStore` trait methods gain tenant variants;
  `FsSkillsStore` shares 3 `*_in_scope` helpers across
  global/tenant. `__global__` reserved as a tenant id
  sentinel to keep precedence explicit. Runtime SkillLoader
  fallback (read tenant first, then global) still pending —
  separate scope (83.8.12.6.runtime).
- 83.8.12.8 ✅ Microapp `tenant_*` tools — agent-creator
  exposes `tenant_list/get/upsert/delete` over the existing
  `nexo/admin/tenants/*` admin RPC handlers. `tenant_set_active`
  folds into `tenant_upsert` (`active: Some(false)`) so the tool
  surface stays minimal. Existing `agent_*`/`skill_*`/
  `llm_provider_*` tools required no change because their wire
  shapes already carry `tenant_id` (Phase 83.8.12.4/.5/.6) and
  serde forwards transparently. Drive-by: takeover.rs `SendArgs`
  gained `session_id: Option<Uuid>` (was missing since Phase
  82.13.b.1 added the field on the SDK side). Out-of-tree commit
  9f634a9.
- 83.15.b.docs ✅ `docs/src/microapps/testing.md` —
  end-to-end testing reference: `MicroappTestHarness` smoke,
  `MockBindingContext` for binding-aware tools, all three
  `MockAdminRpc::on*` flavours (static Ok, static Err,
  closure responder), error round-trip variant preservation,
  invocation counting via Arc<AtomicUsize>, hook fire pattern,
  and what the harness does NOT do (no real daemon, no
  firehose subscription, no persistence). Linked from
  `docs/src/SUMMARY.md` after Templates. Reference test in
  `extensions/template-microapp-rust/` cited as the runnable
  source.
- 83.15.b.template ✅ Reference test in
  `extensions/template-microapp-rust/` exercising MockAdminRpc.
  Template refactored to expose `build_app()` + new `whoami_tool`
  that calls `nexo/admin/agents/get` (Cargo.toml gains
  `features = ["admin"]` on the SDK; dev-deps add
  `test-harness`). 5 tests in `src/main.rs`: ping smoke,
  greet+binding, whoami routes admin call and surfaces canned
  response, whoami propagates typed AdminError as ToolError,
  before_message hook observes-and-continues. Authors copying
  the template inherit the same wiring; their tool tests run
  without a live daemon.
- 82.14.b.throttle ✅ EscalationThrottle primitive — sliding-
  window per-scope counter (default 3/h) defending against
  agent loops that flood the operator UI with identical
  escalations. `try_acquire(scope, now_ms)` returns
  `Ok(remaining)` or `Err(ThrottleDenied { cap, window_ms,
  retry_after_ms })`. Per-scope (NOT per-agent) so an agent
  flagging two distinct conversations within an hour passes;
  `forget(scope)` resets after a successful resolve. Wire-up
  at the future `escalate_to_human` built-in tool call site;
  trait + handler unchanged. 7 tests cover: default cap-3
  admit-then-deny, window slide drops old entries, per-scope
  isolation, retry_after computed from oldest in-window stamp,
  forget resets, zero-cap denies always, tracked_scopes
  observability.
- 82.14.c ✅ SqliteEscalationStore — durable variant of the
  in-memory escalation store. Single-table design keyed by
  canonical scope JSON; full `EscalationEntry` round-trips as
  JSON so future state variants (`Snoozed { until }` etc.)
  land non-breaking. `agent_id` denormalised onto its own
  column with a `(agent_id, updated_at_ms DESC)` index for
  future server-side filter push-down. `open` / `open_memory`
  + WAL mirror audit_sqlite + processing_sqlite open pattern.
  10 tests: missing-scope→None, upsert+get round-trip,
  idempotent upsert returns false on repeat, resolve flips
  Pending→Resolved, double-resolve no-op, resolve unknown
  scope, list newest-first + truncate by limit, list filters
  by agent_id, on-disk round-trip survives drop+reopen, DDL
  idempotent.
- 82.13.d ✅ SqliteProcessingControlStore — durable variant of
  the in-memory ProcessingControlStore so operator pause /
  resume + per-scope pending inbound queues survive daemon
  restart. Two SQLite tables (states keyed by canonical scope
  JSON; pending FIFO indexed by autoincrement id with per-scope
  index). Open / open_memory / with_pending_cap mirror
  audit_sqlite + InMemoryProcessingControlStore APIs so boot
  swaps `Arc::new(InMemory...)` → `Arc::new(Sqlite::open(path)
  .await?)` without any other change. Trait is store-agnostic so
  the dispatcher + runtime see no difference. 11 tests:
  default-AgentActive, set/get round-trip, idempotent set,
  AgentActive deletes row, clear semantics, FIFO eviction with
  cap=3, cap=0 disables buffering, atomic drain, per-scope
  isolation, on-disk round-trip survives drop+reopen, idempotent
  DDL.
- 82.11.log ✅ SqliteAgentEventLog — durable sink for the
  agent-event firehose so `ProcessingStateChanged`,
  `EscalationRequested`, `EscalationResolved`,
  `PendingInboundsDropped`, and `TranscriptAppended` survive
  daemon restart for operator-dashboard backfill. Single
  table with denormalised columns (kind / agent_id /
  tenant_id / at_ms) + per-axis indexes; full
  `AgentEventKind` round-trips as JSON so future
  `#[non_exhaustive]` variants land non-breaking. Doubles
  as an `AgentEventEmitter` so boot composes
  `Tee([Broadcast, SqliteAgentEventLog])` without changing
  emit-site signatures. Read API
  (`AgentEventLog::list_recent`) supports `agent_id` +
  `kind` + `tenant_id` + `since_ms` + `limit` filters with
  parameterised SQL (defense-in-depth: never interpolates
  user-controlled strings). Mirrors audit_sqlite /
  processing_sqlite / escalations_sqlite open pattern (WAL,
  idempotent DDL, `open_memory()` for tests). 10 tests:
  round-trip, agent / kind / tenant / since_ms filters,
  limit cap + default, emit→append routing, empty-on-unknown,
  pool clone shares rows. Boot wire-up + `agent_events/list`
  cross-source merge are deferred (see 82.11.log.b below).
- 82.11.log.compose ✅ Boot-side Tee composition —
  `AdminBootstrapInputs.agent_event_log: Option<Arc<SqliteAgentEventLog>>`
  field added. When `Some`, `build_with_firehose` composes
  `Tee([BroadcastAgentEventEmitter, SqliteAgentEventLog])`
  via `TeeAgentEventEmitter::with_sinks` so every emit
  reaches both live subscribers AND the durable log without
  changing emit-site signatures. Concrete `Arc<SqliteAgentEventLog>`
  type (not `Arc<dyn AgentEventLog>`) so boot can use the same
  handle for both the emitter side (Tee composition via the
  `AgentEventEmitter` impl) and the read side (constructing
  `MergingAgentEventReader` via the `AgentEventLog` impl) —
  MSRV 1.80 doesn't support trait object upcasting yet.
  1 integration test confirms the durable side captures
  emissions driven through `bootstrap.event_emitter()`.
  11 fixture sites updated with `agent_event_log: None`.
  Only main.rs activation remains — see 82.11.log.b above.
- 82.11.bridge ✅ NatsAgentEventEmitter — multi-host
  firehose bridge. Impls `AgentEventEmitter` by publishing
  serialised `AgentEventKind` to
  `<prefix>.<agent_id>.<kind>` (default prefix
  `nexo.agent_events`). Pure `agent_event_subject(prefix,
  &event)` fn exposes the routing key without a live client.
  agent_id sanitisation (`.`/`*`/`>`/whitespace → `_`)
  defends wildcard subscribes. Best-effort: publish errors
  log + drop, broker crate's circuit breaker + disk queue
  protect against NATS being down. Composes with `Tee` so
  boot wires `[Broadcast, Sqlite, Nats]` together. async-nats
  moved from dev-deps to regular deps on nexo-core (1 line).
  5 new tests in `agent::agent_events::tests`: subject for
  TranscriptAppended / ProcessingStateChanged /
  EscalationRequested / EscalationResolved, custom-prefix
  override, agent_id sanitisation against `.` separator.
  Boot stitch (alongside Sqlite log + Tee composition) is
  folded into 82.11.log.b.
- 82.11.log.merge ✅ MergingAgentEventReader — `TranscriptReader`
  impl that composes a transcripts source (JSONL via
  `TranscriptReaderFs`) with a durable `AgentEventLog` (SQLite
  firehose backfill) behind the same trait so the existing
  `agent_events/list` handler returns merged results without
  changing. `kind` filter is pushed down: `transcript_appended`
  routes to JSONL only, other kinds route to the log only,
  `None` queries both then merges by `at_ms` desc + truncates
  to `filter.limit`. The boot wiring of `Tee([Broadcast,
  SqliteAgentEventLog])` means the log captures TranscriptAppended
  too; the merger drops those on the log side to avoid
  double-counting. `read_session_events` + `search_events`
  pass through to transcripts (session_id + FTS5 are transcript-
  only). 6 new tests: kind-none interleaves both sources by
  at_ms desc, kind=transcript_appended routes to transcripts
  only, kind=processing_state_changed routes to log only,
  duplicate transcripts dedup'd from log side, limit truncation
  picks newest after merge, read_session pass-through.
- 82.11.log.sweep ✅ Retention sweep on `SqliteAgentEventLog`
  — `sweep_retention(retention_days, max_rows)` mirrors the
  audit-log sweep shape so boot can run both with one shared
  scheduler. Two-pass DELETE: (1) age-based by `at_ms`
  cutoff, (2) cap-based (oldest first by `at_ms ASC, id ASC`)
  when total > max_rows. Returns total rows deleted. 3 new
  tests: 100d-old row deleted under 60d retention while 30d
  survives; 5-row cap-to-2 drops 3 oldest with newest
  preserved; idempotent no-op when already under both
  thresholds. Boot scheduler wire-up folded into 82.11.log.b
  (alongside `Tee` composition).
- 82.14.b.firehose ✅ Escalation firehose variants —
  `AgentEventKind::EscalationRequested` + `EscalationResolved`
  variants land on the wire (with `tenant_id` skip-when-None for
  multi-tenant routing). Emit-sites: `escalations::resolve` fires
  `EscalationResolved` when the store transition flips
  (`changed = true`); auto-resolve on `processing/pause` fires
  the same shape so subscribers can't tell the two paths apart.
  Dispatcher gains `with_event_emitter` builder; threads emitter
  through to both call sites. `EscalationRequested` emit lands
  alongside the future `escalate_to_human` built-in tool (boot-
  blocked on the BindingContext→scope derivation). 3 new tests:
  resolve emits with right shape + agent_id, no-op skips emit,
  auto-resolve on pause emits.
- 83.15.b ✅ MockAdminRpc — programmable in-process replacement
  for `nexo/admin/*` so microapp tool/hook tests run without a
  daemon. `MockAdminRpc::on(method, value)` /
  `on_err(method, AdminError)` / `on_with(method, |params| ...)`
  register canned responses; `requests_for(method)` exposes the
  request log for assertions. `MicroappTestHarness::with_admin_mock`
  injects the mock's `AdminClient` so `ctx.admin()` returns
  `Some(...)`. Variant-preserving error round-trip (snapshot →
  wire frame → typed AdminError) so mock and daemon paths are
  byte-identical from the caller's POV. 8 mock-module tests + 2
  harness integration tests, 86 SDK tests green. Reference test
  in `extensions/template-microapp-rust/` and dedicated docs
  page (`docs/src/microapps/testing.md`) deferred to next
  template touch.
- 82.14.b + 83.8.2.b ✅ Skills + escalations admin_bootstrap
  wire-up — same gap as 83.8.12.2.b: dispatcher had
  `with_skills_domain` / `with_escalations_domain` builders
  but `admin_bootstrap` never threaded a store, so production
  always returned the typed "domain not configured" -32603.
  `AdminBootstrapInputs` gains `skills_store: Option<Arc<dyn
  SkillsStore>>` + `escalation_store: Option<Arc<dyn
  EscalationStore>>`; build_inner installs both when wired.
  13 fixture sites picked up the new fields.
- 83.8.12.2.b ✅ Tenants admin RPC dispatcher routing —
  Phase 83.8.12.2 shipped the `domains::tenants` handlers + the
  `TenantStore` trait but the dispatcher never routed to them
  (`tenant_store` field was dead, `nexo/admin/tenants/*`
  returned MethodNotFound, microapp tools shipped in .8 hit a
  rejection). Closed: `with_tenants_domain` builder + 4
  handler arms (list/get/upsert/delete) + `tenants_crud`
  capability gate + `AdminBootstrapInputs.tenant_store` so
  production wires the `TenantsYamlPatcher` adapter.
  3 new dispatch tests (capability denial, unwired typed gap,
  routed-to-store success).
- 83.8.12.5.cron ✅ Tenant-aware cron LLM build —
  `CronEntry.tenant_id: Option<String>` (serde-skip when None)
  + idempotent `ALTER TABLE` for legacy DBs +
  `cron_create` tool stamps `ctx.config.tenant_id` at schedule
  time. `RoutedClientResolver` cache key extends with tenant
  scope (provider/model/tenant separator); build call switches
  to `LlmRegistry::build_for_tenant(cfg, model, entry.tenant_id)`
  → tenant-A and tenant-B fires use distinct LlmClients even
  for the same `provider:model` pair. 4 new tests
  (round-trip with tenant + legacy None + build_new_entry stamp
  + dispatcher per-tenant cache isolation).
- 83.8.12.6.runtime ✅ SkillLoader fallback chain — `SkillLoader`
  gains `with_tenant_id(Option<String>)` builder + per-call
  fallback `<root>/<tid>/<name>/` → `<root>/__global__/<name>/`
  → legacy `<root>/<name>/` (logged with deprecation warning).
  `llm_behavior.rs` threads `ctx.config.tenant_id.clone()`. 5
  new tests (global, tenant precedence, tenant→global fallback,
  legacy fallback, not-found).
- 83.8.12.6.b ✅ On-disk migration helper —
  `nexo_setup::skills_migrate::migrate_legacy_skills_to_global`
  moves `<root>/<name>/SKILL.md` (where `<name>` ≠
  `__global__` and is a legacy skill dir, detected by direct
  `SKILL.md` presence) into `<root>/__global__/<name>/SKILL.md`.
  Idempotent, leaves tenant-scope dirs untouched, reports
  conflicts. 6 new tests. CLI sub-command exposure deferred —
  helper is callable from Rust ops scripts today.
- 83.8.12.7 ✅ Audit log `tenant_id` column +
  `tail_for_tenant` — `AdminAuditRow.tenant_id:
  Option<String>` (serde-skip when None);
  `AuditTailFilter.tenant_id`; idempotent ALTER for pre-
  83.8.12.7 DBs (suppresses duplicate-column-name error so
  `open()` round-trips on legacy + fresh paths); SQLite
  index `idx_microapp_admin_audit_tenant`. Dispatcher sniffs
  `tenant_id` from `params.tenant_id` (string only — non-
  string defensive None) and stamps every audit row from
  routing/denial/dispatch sites. CLI `nexo microapp admin
  audit tail --tenant <id>` flag added. New tests: 6 in
  audit.rs + 6 in audit_sqlite.rs (round-trip, filter, null
  exclusion, since_ms combo, limit floor clamp, DDL
  idempotence on existing DB).

### Phase 83.8.12.4.b ✅ — handler-level tenant filter + TranscriptWriter tenant_id (shipped 2026-05-02)

All three deferreds from 83.8.12.4 closed:

1. **`agent_events/list` handler filter ✅** —
   `TranscriptReaderFs.tenant_id` (set via `with_tenant_id`) gates
   `list_recent_events` defense-in-depth: cross-tenant filter
   returns `Vec::new()`; legacy un-tagged readers reject any
   non-`None` tenant filter. `read_session_events` keeps the
   existing `agent_id` pin (params carry no `tenant_id` field on
   the wire today).
2. **`escalations/list` handler filter ✅** —
   `escalations::list(store, patcher, params)` signature gains
   `Option<&dyn YamlPatcher>`. Dispatcher injects the existing
   agents-yaml patcher; handler filters rows by joining
   `EscalationEntry.agent_id` against
   `agents.yaml.<id>.tenant_id` via the existing
   `agents::agent_tenant_id` helper. Tests cover patcher-wired,
   patcher-absent (back-compat pass-through), and
   agents-without-tenant_id-filtered cases.
3. **`TranscriptWriter` tenant_id population ✅** —
   `TranscriptWriter.tenant_id: Option<String>` + `with_tenant_id`
   builder. Emit site stamps every `TranscriptAppended` from the
   writer's own field (no per-event lookup). `llm_behavior.rs`
   threads `ctx.config.tenant_id.clone()` into the writer
   construction so multi-tenant agents stamp automatically.

Original deferred description for posterity:

1. **`agent_events/list` handler filter**: today the
   `tenant_id` field on the filter struct round-trips on the
   wire but the handler ignores it — events are returned
   regardless. Wire-up needs `YamlPatcher` injected into the
   `agent_events` domain so it can cross-reference
   `agents.yaml.<agent_id>.tenant_id` per row. Same shape as
   the `agents/list` filter, just at a different domain.
2. **`escalations/list` handler filter**: identical pattern
   to (1). Trait method already accepts the param; the store
   adapter's `list()` impl needs to filter by joining on
   `agents.yaml`.
3. **`TranscriptWriter` tenant_id population**: today the
   writer emits `AgentEventKind::TranscriptAppended { tenant_id:
   None, .. }` because it does not know its owning tenant.
   Constructor needs an `Option<String> tenant_id` parameter
   (passed by the agent runtime that already knows
   `AgentConfig.tenant_id`). Until wired, multi-tenant
   firehose subscribers must fall back to re-querying
   `agents.yaml` per-event — works but defeats the point of
   the field.

Standalone, can ship incrementally. Not blocking the rest of
83.8.12.5-9 (LLM/skills per-tenant + audit empresa_id
column + microapp tools + docs) since those don't depend on
event-level filtering.

### Phase 83.12 / 83.13 — UI WhatsApp-Web look + React stack

UI of the agent-creator SaaS (operator + tenant) MUST visually
imitate WhatsApp Web so users feel at home from minute one — the
operator and tenant already live inside WhatsApp Web (same
channel as the agents they manage). Cuts onboarding cognitive
load + reduces error rate.

Constraints (apply to brainstorms / specs / plans of 83.12 +
83.13):

- Split-pane: conversation list left, chat panel right.
- Palette: green `#00a884`, grey `#f0f2f5`, white panel,
  light-green `#d9fdd3` outbound bubbles.
- Sans-serif typography (Helvetica / Segoe family).
- React + TypeScript stack. Tailwind for palette consistency.
  Vite or Next.js TBD in 83.12 spec.
- Extensions on top of the WhatsApp Web shape: top-bar tenant
  switcher (operator), right-side drawer for CRUD
  agents/skills/LLM keys + takeover + escalation badge.
- Component library (83.13): `ConversationList`, `ChatPanel`,
  `MessageBubble`, `TopBar`, `TenantSwitcher`, `TakeoverDrawer`,
  `EscalationBadge`.
- DO NOT copy Meta/WhatsApp assets (logos, names). Layout + palette
  imitation only — keep the trademark line clean.
- Comms: frontend ↔ daemon over HTTP server capability
  (Phase 82.12) + transcripts firehose (Phase 82.11) +
  agent-creator microapp tool surface (Phase 83.8 — 22 tools).
- Packaging: bundle inside Rust binary via `rust-embed`, serve
  from microapp HTTP server, OR ship as separate app with
  CORS — pick in 83.12 spec.

Logged in user-memory: `project_ui_whatsapp_web_react.md`.

### Phase 83.8.10 — per-agent compliance toggle propagation

The agent-creator microapp ships a `before_message` compliance
hook (Phase 83.8.10) that runs `OptOutMatcher` +
`AntiLoopDetector` + `PiiRedactor` on every inbound. Today the
toggles are hard-coded defaults — the hook does not honour the
per-agent `extensions_config.compliance` block (Phase 83.1)
because `BindingContext` (the only per-turn context the hook
sees) does not surface that block.

Fix path:

1. Add an optional `extensions_config: BTreeMap<String,
   serde_yaml::Value>` (or specifically `compliance: ComplianceCfg`
   wire shape) to `nexo_tool_meta::BindingContext`.
2. Producer side (whatsapp-rs and friends) populates it from
   the agent's `extensions_config.compliance` slice when emitting
   the inbound `_meta`.
3. SDK `parse_binding_from_meta` reads it back.
4. Microapp hook reads `ctx.binding().extensions_config.compliance`
   and overrides the defaults.

Additionally, the SDK `HookOutcome::Transform` variant is not yet
piped through the dispatch loop's typed return, so PII redaction
silently logs but does not rewrite the body. Closing the
`Transform` wire is a sister follow-up (Phase 83.8 helper sweep).

### Phase 83.8 — domain kill-switch env vars are advisory-only

Discovered while wiring `nexo/admin/skills/*` (83.8.2): the
`NEXO_MICROAPP_ADMIN_*_ENABLED` env-var entries listed in
`crates/setup/src/capabilities.rs::INVENTORY` (`AGENTS`,
`CREDENTIALS`, `PAIRING`, `LLM_KEYS`, `CHANNELS`, `SKILLS`) are
documented as global kill switches but no consumer reads them. A
microapp granted the operator capability still gets the domain
even when the operator exports
`NEXO_MICROAPP_ADMIN_<DOMAIN>_ENABLED=0`.

Fix is a small one-shot: have `admin_bootstrap` consult each toggle
when constructing the dispatcher and, when off, omit the domain
adapter so the relevant arms of `call_handler` fall through to
`-32601 method_not_found` (or the existing
"<domain> not configured" `-32603`). Same pattern
`NEXO_MICROAPP_AGENT_EVENTS_ENABLED` already follows. Predates this
phase but only surfaced now while scanning INVENTORY for the
`SKILLS` slot. Target: small framework hardening sub-phase
(suggest `83.8.x` after the agent-creator v1 close-out).

## Resolved (recent highlights)

- 2026-04-28 — MCP denied-tool override now supports `Heartbeat`
  (`schedule_reminder`) with explicit hardening. In `nexo mcp-server`,
  `Heartbeat` can be exposed only when listed in both
  `mcp_server.expose_tools` and `mcp_server.expose_denied_tools`,
  auth is configured (`auth_token_env` or `http.auth`), the agent has
  `heartbeat.enabled = true`, and memory is available. The tool now also
  accepts MCP-friendly explicit route fields
  (`session_id`, `source_plugin` + optional `source_instance`,
  `recipient`) and falls back to `AgentContext` (`session_id`,
  `inbound_origin`) when present.

- 2026-04-28 — Cron tool/docs descriptions are now aligned with shipped
  semantics (A-8 closure). Updated `cron_*` `ToolDef` descriptions to
  explicitly cover origin-tagged binding scope, 60-second minimum
  interval, per-binding cap, and one-shot retry/drop behavior. Also
  removed stale "follow-up not shipped" wording in
  `cron_schedule`/`cron_runner`/`llm_cron_dispatcher` module docs and
  refreshed `docs/src/architecture/cron-schedule.md` to include
  `cron_pause`/`cron_resume`, origin tagging, model pinning, and the
  current plan-mode classification.

- 2026-04-28 — Cron one-shot dispatch now supports bounded retries
  instead of drop-on-first-failure only. `runtime.yaml` gained
  `cron.one_shot_retry` (`max_retries`, `base_backoff_secs`,
  `max_backoff_secs`; defaults `3 / 30 / 1800`). `CronRunner`
  schedules exponential-backoff retries on one-shot dispatch failure,
  increments durable `failure_count` per row, and drops the entry only
  after budget exhaustion. Store schema now includes
  `nexo_cron_entries.failure_count` with idempotent migration for
  existing DBs. Coverage added in `cron_schedule` + `cron_runner`
  tests.

- 2026-04-28 — `RemoteTrigger` now honors per-binding overrides.
  `InboundBinding` gained `remote_triggers` (replace semantics over
  `agents[].remote_triggers`), `EffectiveBindingPolicy` now resolves
  and carries that list, and `RemoteTriggerTool` reads from the
  session-effective policy instead of agent-level config only. Tool
  registration now considers both agent-level and binding-level
  remote-trigger lists so binding-only configs still expose the tool.
  Hardening included rate-limit bucket scoping by `(binding_index,
  trigger_name)` to avoid cross-binding interference when names match.
  Coverage added in `remote_trigger_tool` tests plus parse coverage in
  `crates/config/tests/binding_overrides.rs`.

- 2026-04-28 — Runtime now stamps interactive turn context from the
  inbound message (not session bootstrap only). `flush()` in
  `crates/core/src/agent/runtime.rs` builds a per-message context
  carrying `inbound_origin` and `sender_trusted`, so `EnterPlanMode`
  and trusted dispatch gates read real channel/account/sender data on
  live inbound turns. `sender_trusted` is asserted from pairing-gate
  `Decision::Admit` and defaults fail-closed elsewhere. Coverage added
  in `crates/core/tests/pairing_gate_intake_test.rs`.

- 2026-04-28 — Config approval subscriber now accepts both
  `plugin.inbound.<channel>` and
  `plugin.inbound.<channel>.<instance>` topics. No-instance events map
  to account `default`, which unblocks approvals from single-instance
  plugin routes.

- 2026-04-28 — `ConfigTool` now resolves proposal actor origin from the
  current `AgentContext.inbound_origin` when available, instead of
  always using a boot-time fallback binding. Approval correlation and
  staged proposal YAML now carry the real
  `(channel, account_id, sender_id)` of the turn that proposed the
  change. Coverage added in
  `agent::config_tool::tests::propose_uses_inbound_origin_from_context_when_available`
  (`--features config-self-edit`).

- 2026-04-28 — `ConfigTool` pending proposal recovery now survives
  process restarts. On boot, each tool instance rehydrates unexpired
  staged proposals from disk into both the correlator and
  `pending_receivers`; expired staging files are cleaned up. `apply`
  also has a lazy fallback that rebuilds a receiver from staging when
  the in-memory map is missing. Additional hardening kept from the
  earlier patch: propose-time staging failures now clean up both maps,
  and apply staging read/parse failures requeue the receiver instead of
  consuming it. Coverage added in
  `agent::config_tool::tests::boot_recovery_rehydrates_pending_proposals_from_staging`
  and
  `agent::config_tool::tests::apply_no_pending_can_recover_receiver_from_staging_file`
  (`--features config-self-edit`).

- 2026-04-28 — MCP resource URI allowlist now enforces hard reject
  before dispatch (no warn-only bypass). Both per-server
  `mcp_<server>_read_resource` and router `ReadMcpResource` paths
  share the same scheme gate, emit a `warn`, increment
  `mcp_resource_uri_allowlist_violations_total{server=...}`, and
  return an explicit error when the URI scheme is outside
  `mcp.resource_uri_allowlist`. Integration coverage updated in
  `crates/core/tests/mcp_resource_tool_test.rs` including router-path
  rejection/success cases.

- 2026-04-26 — `skills_dir: ./skills` in every agent YAML now points
  at `../skills` so the `resolve_relative_paths` step in
  `crates/config/src/lib.rs` (which roots relative paths at
  `<config_dir>/`) hits the project-level `skills/` tree instead of
  the non-existent `config/skills/`. Also dropped `web-search` from
  `agents.d/cody.yaml::skills` because no `skills/web-search/SKILL.md`
  ships in this checkout. Removes the WARN flood on every Cody turn
  and stops "missing SKILL.md" entries from masking real errors.

- 2026-04-26 — `nexo-driver-loop`'s `substitute_env_vars` no longer
  mangles UTF-8 in `config/driver/claude.yaml`. The loader copied
  bytes as `char` one at a time, so any multi-byte codepoint (e.g.
  the em-dash on line 1 of the shipped reference config) split into
  raw bytes — including C1 control bytes 0x80–0x9F that YAML
  rejects with "control characters are not allowed". Driver boot
  failed silently with a WARN, which Cody surfaced as "in-process
  driver isn't booted" and disabled every dispatch tool. Now the
  substitution copies the unmodified UTF-8 around each `${VAR}`
  span instead.



- 2026-04-26 — Admin first-run wizard at `/api/bootstrap/finish` now
  refuses to create `agents.d/<slug>.yaml` when an agent with that id
  already exists (either at the same path or in `config/agents.yaml`).
  Combined with the strict drop-in override rule below, this closes
  the loophole that produced a truncated `kate.yaml` next to a
  full definition and silently nuked the agent's bindings.
- 2026-04-26 — Runtime no longer treats "agent without
  `inbound_bindings`" as a wildcard. The empty-bindings branch in
  `crates/core/src/agent/runtime.rs` was removed; events go through
  `match_binding_index` unconditionally. The "legacy wildcard"
  fallback was the actual mechanism that let a single bot's
  messages reach every agent that subscribed to `plugin.inbound.>`.
  Tests updated in `crates/core/tests/runtime_test.rs` and
  `per_binding_override_test.rs` to lock in the strict rule.
- 2026-04-26 — `agents.d/<id>.yaml` drop-in overrides now REPLACE the
  base entry by `id` instead of appending a duplicate. Earlier the
  loader did `base.agents.extend(extra.agents)`, leaving two
  definitions for the same agent in the loaded config — when the
  override happened to omit `inbound_bindings`, the truncated copy
  fell into the runtime's "no bindings → legacy wildcard" branch and
  silently caught every plugin event. Fixed in
  `crates/config/src/lib.rs::merge_agents_drop_in`.



- 2026-04-26 — Telegram inbound fan-out now respects bot/agent
  isolation. `match_binding_index` in
  `crates/core/src/agent/runtime.rs` was tightened so a binding with
  `instance: None` only catches no-instance topics; per-bot setups
  must scope bindings with explicit `instance:`. Previously a
  no-instance binding swallowed every instance, fanning a single
  bot's messages out to every agent that listed the channel. Tests
  in `crates/core/tests/runtime_test.rs` and the inline unit suite
  updated to lock in the strict semantics.
- 2026-04-26 — Setup wizard now writes the per-instance allowlist on
  the right path everywhere. `telegram_link::run` accepts an
  `agent_id`, and `yaml_patch::telegram_append_chat_id` mutates the
  exact `telegram[<i>].allowlist.chat_ids` entry whose `allow_agents`
  matches. The CLI grew `agent setup telegram-link [<agent>]`. The
  legacy bug — `upsert("telegram.allowlist.chat_ids", …)` treating
  `telegram` as a map — is gone. `services_imperative::run_telegram`
  and `services/channels_dashboard::run_telegram_flow` already
  routed through `telegram_upsert_instance` and now also call the
  new `yaml_patch::upsert_agent_inbound_binding` helper so the
  agent's `inbound_bindings` carry the matching `instance:` (required
  under the tightened topic-match rule above).
- 2026-04-26 — Setup wizard seeds `pairing_allow_from` for every
  chat_id captured during onboarding (`telegram_link.rs` +
  `services/channels_dashboard.rs`). Operators that disable the YAML
  allowlist and rely solely on pairing no longer face a redundant
  challenge for an identity the wizard already approved. New
  `nexo-pairing` dependency added to `nexo-setup`; failures are
  logged but don't abort the wizard since the YAML allowlist still
  admits the chat.
- 2026-04-26 — Telegram plugin long-poll observes the shutdown
  cancellation token. `spawn_poller` in
  `crates/plugins/telegram/src/plugin.rs` now races the
  `bot.get_updates(...)` future against `shutdown.cancelled()` so
  Ctrl+C exits in <100 ms instead of waiting the full ~25 s
  long-poll. `offset` is only persisted on a successful round-trip,
  so cancelled updates are simply redelivered on next start.



- Streaming telemetry and streaming runtime wiring completed.
- Per-agent credentials hot-reload completed.
- Browser CDP reliability hardening completed.
- Shared extension resilience helpers extracted.
- Docs sync gate and mdBook English checks enabled.
- 2026-04-25 — SessionLogs tool registered in agent bootstrap and mcp-server (gated on non-empty `transcripts_dir`).
- 2026-04-25 — Skill dependency modes (`strict`/`warn`/`disable`) with per-agent `skill_overrides` + `requires.bin_versions` semver constraints (custom `command`/`regex` per bin). Probes are concurrent and process-cached. Banner inline for `warn` mode so the LLM sees missing deps.
- 2026-04-25 — 1Password `inject_template` tool (template-only with reveal gate, exec mode with `OP_INJECT_COMMAND_ALLOWLIST`, `dry_run` validation, stdout cap, redacted stdout/stderr) + append-only JSONL audit log (`OP_AUDIT_LOG_PATH`) covering `read_secret` and `inject_template` with `agent_id` / `session_id` context.
- 2026-04-25 — `agent doctor capabilities [--json]` CLI + `crates/setup/src/capabilities.rs` inventory: enumerates every write/reveal env toggle across bundled extensions (`OP_ALLOW_REVEAL`, `OP_INJECT_COMMAND_ALLOWLIST`, `CLOUDFLARE_*`, `DOCKER_API_*`, `PROXMOX_*`, `SSH_EXEC_*`) with state, risk, and revoke hints. Doc page `docs/src/ops/capabilities.md`.
- 2026-04-25 — TaskFlow runtime wiring: shared `FlowManager`, `WaitEngine` tick loop, `taskflow.resume` NATS bridge, and tool actions `wait`/`finish`/`fail` with guardrails (`timer_max_horizon`, non-empty topic+correlation).
- 2026-04-25 — Transcripts FTS5 index + redaction module: `transcripts.yaml` config, write-through index from `TranscriptWriter`, `session_logs search` uses FTS when present (substring fallback otherwise), opt-in regex redactor with 6 built-in patterns (Bearer JWT, sk-/sk-ant-, AWS access key, hex token, home path) and operator-defined `extra_patterns`.

- 2026-04-27 — **Phase 48 (Email channel) deferrals.** Phase 48 closed
  with sub-phases 48.1–48.10 ✅ but ten knobs were intentionally
  parked rather than bloat the closing slice:
  - **Interactive setup wizard.** ✅ Shipped 2026-04-27.
    `crates/setup/src/services/email.rs::run_email_wizard(
    config_dir, secrets_dir)` walks the operator through
    address → provider auto-detect via `provider_hint(domain)`
    (preset accept / override) → auth kind (Password /
    OAuth2Static / OAuth2Google) → secret entry.
    `upsert_email_account_yaml` upserts into `email.yaml`
    (idempotent on instance id, accounts beside it preserved)
    and `write_secret_toml` writes the TOML at mode 0o600
    (Unix) via temp+rename so a partial write never lands.
    Pure helpers (`derive_default_instance`,
    `serialise_secret_toml`, `render_account_block`) ship 10
    unit tests; the interactive shell still requires a TTY so
    e2e of the dialoguer flow is out of scope.
  - **Tool registration in `src/main.rs`.** ✅ Shipped 2026-04-27.
    `OutboundDispatcher` extracts a cheap `Arc<DispatcherCore>` that
    `EmailPlugin::dispatcher_handle()` returns post-start; main.rs
    builds an `EmailToolContext` after `plugins.start_all()` and the
    per-agent loop calls `register_email_tools(&tools, ctx)` when
    `agent.plugins` lists `email`. Six handlers (send / reply /
    archive / move_to / label / search) now reach the LLM.
  - **greenmail e2e** harness. 🔄 Partial 2026-04-27.
    `tests/pipeline_in_process.rs` covers the in-process slice:
    `OutboundDispatcher::enqueue_for_instance` →
    JSONL queue + Message-ID idempotency, `parse_eml` →
    `resolve_thread_root` → `session_id_for_thread` →
    `enrich_reply_threading`, `BounceStore` upsert + count
    increment, loop_prevent self-from skip. Five integration
    tests; broker is the local in-process bus, so the SMTP
    `DATA` round-trip and IMAP IDLE / FETCH / MOVE wire calls
    still need a Docker compose with greenmail in CI to land
    fully ✅.
  - **Hot-reload account diff.** ✅ Shipped 2026-04-27.
    `reload.rs::compute_account_diff(old, new) -> AccountDiff
    {added, removed, changed}` is the pure helper.
    `InboundManager` and `OutboundDispatcher` now hold per-
    instance `WorkerSlot { handle, cancel }` maps so a single
    worker can be torn down without touching siblings —
    parent cancel still kills the union, child cancel kills
    just one. `EmailPlugin::apply_account_diff(new_cfg, broker)`
    is the runtime entry: removes outbound first (so an in-
    flight job lands on disk before the inbound that read it
    disappears), then inbound; respawns `changed` accounts on
    both sides; spawns `added` last. The deprecated
    `apply_added_accounts` alias is preserved for back-compat
    but now forwards to the surgical implementation.
  - **Persistent bounce history.** ✅ Shipped 2026-04-27.
    `bounce_store.rs` ships a sqlx-sqlite `BounceStore` keyed on
    `(instance, recipient)` (recipient lowercased on insert /
    lookup). `inbound::drain_pending` now upserts every parsed
    bounce before publishing the wire event, incrementing a
    `count` column so a flapping recipient surfaces as a single
    row. `EmailToolContext.bounce_store: Option<Arc<BounceStore>>`
    is wired by main.rs from `plugin.bounce_store_handle()`;
    `email_send` consults it for every recipient (to + cc + bcc)
    and includes a `recipient_warnings` array in its success
    envelope when it finds prior bounces. Advisory only — the
    operator may have fixed the destination since the bounce, so
    the tool doesn't refuse to send.
  - **IMAP STARTTLS.** ✅ Shipped 2026-04-27.
    `ImapConnection::connect` now accepts `TlsMode::Starttls`:
    plain TCP dial, consume `* OK` greeting, run `STARTTLS`,
    upgrade the underlying `TcpStream` in place via the
    `tokio_util::compat` shim's `into_inner`, then resume the
    normal LOGIN / CAPABILITY flow on the TLS-wrapped session.
    `Plain` (no encryption) still rejects at connect — that's
    the security default we keep.
  - **Multi-selector DKIM probe.** ✅ Shipped 2026-04-27.
    `spf_dkim::DKIM_SELECTORS = ["default", "google", "selector1",
    "selector2", "mail"]` — first match wins. `AlignmentReport`
    carries `dkim_selector: Option<String>` so the matched selector
    surfaces; the `dkim_missing` WARN now logs the full list of
    probed selectors so the operator chasing a custom one knows
    what's already covered.
  - **`/healthz` HTTP integration.** ✅ Shipped 2026-04-27.
    `RuntimeHealth.email_plugin: Option<Arc<EmailPlugin>>` and a
    new `/email/health` route on the existing health server emit
    a sorted JSON array — one row per account with `state`
    (connecting / idle / polling / down), the IDLE / poll /
    connect timestamps, `consecutive_failures`,
    `messages_seen_total`, `last_error`, and the outbound
    queue/DLQ/sent/failed totals. Returns `[]` (not 404) when
    the plugin isn't configured so monitoring scripts can hit
    the route unconditionally.
  - **Dedicated Prometheus metrics** for email
    (`email_imap_state{instance}` gauge,
    `email_imap_messages_fetched_total{instance}` counter,
    `email_loop_skipped_total{reason}`,
    `email_bounces_total{instance, classification}`).
  - **Phase 16 binding-policy auto-filter.** ✅ Shipped 2026-04-27.
    `register_email_tools_filtered(registry, ctx, allow)` accepts
    an optional list of tool names to register; the no-arg
    `register_email_tools` is preserved as the all-six wrapper.
    `EMAIL_TOOL_NAMES` is the public canonical list.
    `filter_from_allowed_patterns(allowed)` derives the filter
    from `agent.allowed_tools` honouring the `*` / `email_*` /
    empty-list "register everything" semantics. main.rs's
    per-agent loop now passes the derived filter so
    `allowed_tools: ["email_send", "email_search"]` only
    registers those two handlers — instead of registering all
    six and pruning at LLM turn time.
  - **Cross-account attachment GC.** ✅ Shipped 2026-04-27.
    `attachment_store.rs` ships `AttachmentStore` (sqlx-sqlite,
    `email_attachments` table keyed on sha256 with first_seen /
    last_seen / count). `inbound::drain_pending` records every
    attachment after a successful parse so `last_seen` reflects
    the most recent message that referenced the file.
    `EmailPlugin::start` spawns a daily GC task that calls
    `gc(attachments_dir, retention_secs)` — sweeps both the row
    and the on-disk file when `last_seen < now - retention`.
    Missing files (manual cleanup, fs error) drop the row
    anyway so we don't keep retrying. New
    `EmailPluginConfig.attachment_retention_days` (default 90,
    `0` disables GC entirely).

## Phase 79.1 — Plan mode follow-ups

  - **Operator-approval scope check.** ⬜ Pending. Phase 79.1
    pairing approval (`[plan-mode] approve|reject plan_id=<ulid>`)
    currently authorises any sender on the binding's pairing
    channel. OpenClaw's `research/src/gateway/exec-approval-ios-push.ts:55-89`
    enforces a `roleScopesAllow({role: 'operator',
    requestedScopes: ['operator.approvals']})` check before
    accepting an approval message. When 79.10 ships
    `approval_correlator`, port that pattern: per-binding
    `operator.approvals` scope on the `(channel, account_id)`
    tuple, refusal logs `[plan-mode] approval rejected:
    sender lacks operator.approvals`. Hard prereq before the
    config-self-edit flow (79.10) opens up.
  - **`final_plan_path` variant.** ⬜ Pending if 8 KiB cap
    proves restrictive. The leak's `ExitPlanModeV2Tool.ts`
    reads the plan from disk via `getPlanFilePath(agentId)`;
    add an `ExitPlanMode { final_plan_path: PathBuf }` arm
    that points at a file written via `FileWrite` during
    plan mode. Only pursue when real workloads hit the cap.
  - **Acceptance retry policy.** ⬜ Pending. Phase 79.1
    fire-and-forget acceptance can be flaky (slow tests,
    transient network). Add bounded retry (1 retry after 30 s)
    before publishing `[plan-mode] acceptance: fail`.
  - **Acceptance hook fire-and-forget integration.** ⬜
    Pending (was step 14 of original 79.1 plan, parked at MVP).
    `ExitPlanMode` should spawn a tokio task on approve that
    runs the Phase 75 acceptance autodetect against the plan
    and posts `[plan-mode] acceptance: pass|fail (<summary>)`
    to `notify_origin` asynchronously. Today the unlock is
    inline; acceptance integration is a pure addition.
  - **Auto-enter-on-destructive (cfg-gated).** ⬜ Pending
    (was step 15 of original 79.1 plan). When
    `auto_enter_on_destructive: true` and the next call is
    classified destructive by Phase 77.8, the dispatcher
    pre-empts with a refusal carrying
    `entered_reason: AutoDestructive { tripped_check }` and
    flips state to On in the same step. Hard dep on Phase
    77.8 destructive-command warning shipping first.
  - **Pairing parser for `[plan-mode] approve|reject plan_id=…`.** ✅ 2026-04-30
    `parse_plan_mode_approval()` regex-based parser in `plan_mode_tool.rs`
    extracts `PlanModeApprovalCommand::{Approve|Reject}` from inbound
    chat messages. Process-shared `PlanApprovalRegistry` injected via
    `AgentRuntime::with_plan_approval_registry()` into all goal contexts.
    Broker subscriber in `main.rs` routes parsed `[plan-mode]` commands
    to `registry.resolve()`. 7 unit tests cover approve/reject/no-reason/
    whitespace/malformed/extra-text/empty-body.
  - **Notify_origin actual delivery (not just tracing).** ⬜
    Pending. The canonical `[plan-mode]` notify lines emit
    via `tracing::info!` today; production deployments need
    them surfaced through the pairing channel that owns the
    goal. Wire via the existing `HookDispatcher` /
    `PairingAdapterRegistry` plumbing that
    `notify_origin` already uses for completion hooks.
  - **End-to-end integration tests via dispatcher.** ⬜
    Pending (was step 16 of original 79.1 plan). Unit tests
    cover individual pieces (37 across `plan_mode`,
    `plan_mode_tool`, `tool_registry`, registry persistence,
    reattach). A dispatcher-level e2e — "goal calls Bash
    mutating while plan-mode On → receives PlanModeRefusal
    as `tool_result`" — would prove the wired-up gate
    end-to-end. Lives in
    `crates/dispatch-tools/tests/plan_mode_*.rs`.

## Phase 79.2 — ToolSearch follow-ups

  - ~~**LLM provider filtering of deferred schemas.**~~ ✅ 2026-04-30
    `ToolRegistry` gained `to_tool_defs_non_deferred()` and
    `deferred_tools_summary()`. `llm_behavior.rs::run_turn` now
    filters deferred tools from `req.tools` and appends a
    `<deferred-tools>` stub block to `system_blocks` so the model
    sees names + descriptions without paying for full schemas.
    `ToolSearch` stays non-deferred (registered via plain
    `register()`, not `register_with_meta()`).
  - ~~**MCP catalog auto-marks imported tools as deferred.**~~ ✅ already shipped
    (verified `mcp_catalog.rs:240-257` — `register_into` calls
    `registry.set_meta(&prefixed, ToolMeta::deferred())` for every
    inserted MCP tool).
  - ~~**Per-turn rate limit on `ToolSearch` itself.**~~ ✅ already shipped
    `ToolSearchRateLimiter` (sliding window, keyed by agent_id, default
    5 calls/min) lives in `tool_search_tool.rs:54-88`. Follow-ups entry
    was stale.
  - **Result format `<functions>` block parity with leak.** ⬜
    Pending. Current MVP returns matches as a JSON object with
    `name`/`description`/`parameters` per match. The leak instead
    returns `<tool_reference>` blocks that the SDK expands into
    real `<function>` declarations on the next turn. Useful for
    Anthropic-native callers that want zero JSON-parsing on the
    model side.

## Phase 79.7 — ScheduleCron follow-ups

  - ~~**Runtime firing not wired.**~~ ✅ shipped 2026-04-27.
    `crates/core/src/cron_runner.rs::CronRunner` polls
    `store.due_at(now)` every 5 s, dispatches via
    `Arc<dyn CronDispatcher>`, and advances state per-entry:
    recurring always advances (even on dispatch failure), while
    one-shot uses bounded retry policy
    (`runtime.cron.one_shot_retry`) before final drop. Spawned in
    `src/main.rs` right
    before `shutdown_signal().await` with a `LoggingCronDispatcher`
    (emits `[cron] fired` per dispatch).
  - ~~**LLM-call cron dispatcher.**~~ ✅ shipped 2026-04-27.
    `crates/core/src/llm_cron_dispatcher.rs::LlmCronDispatcher`
    builds `ChatRequest` from `entry.prompt`, calls
    `LlmClient::chat`, logs response with id + binding +
    cron + 200-char preview. `with_system_prompt` +
    `with_max_tokens` knobs. Runtime resolves the client from the
    entry's pinned `model_provider`/`model_name` with legacy
    fallback for rows created before model pinning. Falls back to
    `LoggingCronDispatcher` when no agents configured or
    LLM-client build fails (degraded boot stays observable).
    7 unit tests cover system-prompt prepended/empty/skipped,
    max-tokens propagation, LLM failure → error, empty
    response → ok, model_id taken from client, user-prompt
    routed.
  - ~~**Outbound publish to binding's channel.**~~ ✅ shipped 2026-04-27.
    `LlmCronDispatcher::with_publisher(Arc<dyn ChannelPublisher>)`
    routes the model's response to the user-facing channel when
    the entry carries both a `channel` (`<plugin>:<instance>`) and
    a `recipient` (JID / chat-id / email). Production wiring uses
    `BrokerChannelPublisher` which emits
    `{"kind": "text", "to": <recipient>, "text": <body>}` on
    `plugin.outbound.<plugin>.<instance>` — same envelope the
    WhatsApp / Telegram / Email outbound tools already speak.
    `parse_channel_hint` rejects malformed `<plugin>:<instance>`
    strings so the broker never sees `plugin.outbound.whatsapp.`
    (trailing dot). Publisher errors are logged via
    `tracing::warn!` but never fail `fire()` — the runner still
    advances state so a stuck downstream channel cannot deadlock
    the cron loop. `CronEntry.recipient: Option<String>` was added
    with an idempotent `ALTER TABLE` for older DBs and threaded
    through `cron_create` (new `recipient` arg). 5 publisher tests
    + 5 `parse_channel_hint` tests cover the happy path and edge
    cases (missing channel, missing recipient, publisher error,
    no publisher, malformed hints).
  - ~~**CLI `nexo cron list / drop / pause / resume`.**~~ ✅ shipped 2026-04-28.
    Operator-side cron admin now ships in `src/main.rs`:
    `agent cron list [--json] [--binding <id>]`,
    `agent cron drop <id>`, `agent cron pause <id>`, and
    `agent cron resume <id>`.
    This removes the need for direct SQL access for routine cron
    inspection and pause/resume/delete actions.
  - **Capability gate `cron.enabled` per binding.** ⬜ Pending.
    The MVP registers the tools globally — every agent gets
    them regardless of role. Spec called for `cron.enabled:
    bool` per binding (default `true` only for `coordinator` /
    `proactive` roles). Wire when 77.18 coordinator role
    lands.
  - ~~**Jitter on firing.**~~ ✅ 2026-04-30
    `RuntimeCronConfig.jitter_pct` (default 10). `CronRunner`
    applies `apply_jitter()` on recurring advance + one-shot retry
    timestamps. Zero-jitter by default in tests (deterministic).
    Plumbed from `runtime.yaml` → `CronRunner::with_jitter_pct()`.
    `apply_jitter()` already existed, ported from
    `claude-code-leak/src/utils/cronJitterConfig.ts` — wiring was
    the only missing piece.
  - ~~**`cron_pause` / `cron_resume` tools.**~~ ✅ shipped 2026-04-28.
    The `paused` column is now operator-reachable through tools:
    `cron_pause {id}` sets `paused=true` and `cron_resume {id}`
    sets `paused=false` without dropping the entry.

## Phase 79.11 — McpAuth follow-up

  - **`McpAuth` tool not shipped.** ⬜ Pending. Spec called for
    `McpAuth { server, op: refresh|status }` so the model can
    trigger an OAuth refresh or report auth state on a connected
    MCP server. The `McpClient` trait
    (`crates/mcp/src/client_trait.rs`) does not yet expose a
    `refresh_auth` / `auth_state` method — refresh is currently
    transparent inside the client. Once the trait grows the
    method (lift from
    `claude-code-leak/src/services/mcp/oauthPort.ts`), wire a
    third tool into `agent/mcp_router_tool.rs` and register it
    in `src/main.rs` next to the other two router tools.

## Phase 76.16 — expose_tools deferred items

  - **`Config` tool gated.** ⬜ Pending. `expose_tools: [Config]`
    emits a `tracing::warn!` and skips registration at startup.
    The Config tool (Phase 79.10) requires the full approval-correlator
    + plan-mode op-aware gating before it can safely be exposed to
    external MCP clients. Wire it once Phase 79.10 ships the
    approval workflow end-to-end and the `config_tool.self_edit` gate
    is validated against the originating channel.
  - **`Lsp` tool gated.** ⬜ Pending. `expose_tools: [Lsp]` emits
    a `tracing::warn!` and skips. LSP (Phase 79.5) requires spawning
    and managing a language server process; the tool itself is
    registered correctly for agent goals but the process lifetime
    is not safe to share across arbitrary MCP client sessions
    without additional session isolation. Defer until Phase 79.5
    follow-up lands per-session LSP process management.

## Maintenance note

If a future historical import includes non-English notes, keep them in `archive/spanish/*.txt` and update this Markdown tracker in English only.
