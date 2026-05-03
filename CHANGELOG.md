# Changelog

All notable changes to this project are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project adheres to [Semantic Versioning](https://semver.org)
**once `v1.0.0` is tagged**. Until then breaking changes may land on
`main` between any two commits; see the commit history for detail.

## [Unreleased]

### Added

- **Phase 81.14 — `SubprocessNexoPlugin` host-side adapter (spawn +
  stdio JSON-RPC handshake).** First slice of out-of-tree plugin
  infrastructure. Plugin authors will eventually ship binaries that
  speak newline-delimited JSON-RPC 2.0 over stdin/stdout; the
  daemon owns the child process via this adapter and exposes it as
  `Arc<dyn NexoPlugin>` so the existing `wire_plugin_registry` boot
  path drives lifecycle without any new trait-level hook on the host.
  New manifest section `[plugin.entrypoint]` (additive, every field
  defaults so the four 81.12.a-d in-tree manifests parse without
  changes): `command: Option<String>` (`None` = in-tree Rust plugin —
  legacy shape), `args: Vec<String>`, `env: BTreeMap<String, String>`.
  `EntrypointSection::is_subprocess()` returns `true` only when
  `command` is `Some` and non-empty. New file
  `crates/core/src/agent/nexo_plugin_registry/subprocess.rs` (~480 LOC)
  ships `SubprocessNexoPlugin` + `subprocess_plugin_factory` helper.
  `init()` body: spawns child via `tokio::process::Command` (stdio
  piped, stderr null, kill_on_drop), then writes one
  `initialize { nexo_version }` request line and awaits a reply with
  matching id on stdout. Reply must carry `manifest.plugin.id` equal
  to the factory's registered id — out-of-tree binary pretending to
  be a different plugin gets rejected. Defense-in-depth on env keys:
  manifest `entrypoint.env` cannot redefine reserved `NEXO_*` names
  (a plugin author overriding `NEXO_STATE_ROOT` from a manifest gets
  a hard boot failure, not silent confusion). Three background tasks
  per plugin: stdin writer task (single mpsc consumer, bounded depth
  64 — drop-on-full matches at-most-once broker semantics), stdout
  reader task (parses each line, demuxes id-tagged responses to
  pending oneshots vs untagged notifications which are debug-logged
  this slice — broker bridge wiring lands in 81.14 follow-up), and
  the daemon-wide cancellation token cascades cancellation into
  child tokens. `shutdown()` sends a JSON-RPC `shutdown` request,
  waits 5s for reply, then 1s grace before SIGKILL via `Child::kill`,
  finally joins all background tasks with 1s each. Idempotent across
  multiple calls (a never-started plugin returns `Ok(())`).
  Configurable timeout via `NEXO_PLUGIN_INIT_TIMEOUT_MS` env (default
  5000). 9 unit tests: `entrypoint_section_serde_roundtrip`,
  `is_subprocess_returns_false_for_in_tree_default`,
  `subprocess_plugin_manifest_returns_cached`,
  `init_fails_when_command_not_found`,
  `init_fails_when_env_collides_with_nexo_reserved`,
  `init_times_out_when_child_silent` (uses `/bin/cat` as the canonical
  silent child),
  `init_fails_when_manifest_id_mismatch_on_initialize_reply` (writes
  a tiny shell script fixture into tempdir that returns a wrong id),
  `factory_helper_produces_arc_dyn_nexoplugin`,
  `shutdown_is_idempotent_when_never_started`. **Out of scope this
  slice** (deferred to follow-up sub-phases already tracked in
  PHASES-curated.md): broker → child topic bridge (81.14.b),
  child-side `PluginAdapter` SDK helper (81.15), versioned
  `nexo-plugin-contract.md` spec (81.16), real out-of-tree plugin
  extraction (81.17), `memory.recall` / `llm.complete` / `tool.dispatch`
  host RPC handlers (81.20), supervisor + respawn + resource limits
  (81.21), sandbox (81.22), stdio → tracing bridge (81.23). IRROMPIBLE
  refs: internal `extensions/openai-whisper/src/main.rs:1-79` +
  `protocol.rs:1-23` (proven nexo subprocess plugin shape, reuses
  same JSON-RPC envelope); claude-code-leak
  `src/utils/computerUse/mcpServer.ts` + `src/commands/mcp/addCommand.ts`
  (MCP stdio transport pattern via `@modelcontextprotocol/sdk`); OpenClaw
  absence stated — `research/extensions/{whatsapp,telegram,browser}/`
  ran in-process Node, no subprocess channel-plugin precedent there.

### Changed

- **Phase 81.12.e deferred → superseded by Phase 81.17** (extract
  `plugin-browser` to standalone repo via subprocess infra). With
  81.12.a-d shipped, the original 81.12.e scope (delete the legacy
  registration block from `src/main.rs:1855-1941`, ~87 LOC) collides
  with three realities only visible post-implementation:
  (1) the legacy block builds concrete `Arc<BrowserPlugin>` /
  `Arc<EmailPlugin>` / per-instance `WhatsappPlugin::pairing_state`
  references that downstream code (email tool ctx, HTTP server
  `/whatsapp/<instance>/pair*` endpoints, per-agent tool registration)
  consumes directly — not via the `NexoPlugin` trait. Removing
  construction breaks downstream.
  (2) Activating `factory_registry` without removing the legacy
  `plugins.register*()` + `start_all()` calls would cause
  `Plugin::start` to fire twice (once via legacy `start_all`, once
  via `NexoPlugin::init` delegation) → double-init breakage.
  (3) For `factory_registry` to fire, discovery walker must find
  `nexo-plugin.toml` manifests, but they're dormant inside
  `crates/plugins/<id>/` — solving requires bundled-manifest discovery
  search_paths or synthetic injection (~1-2 d of work that Phase 81.17
  obsoletes). With out-of-tree plugins (Phase 81.14 →
  81.17 → 81.18 → 81.19), main.rs no longer constructs `Arc<BrowserPlugin>`
  at all; downstream code accesses the plugin via daemon-mediated RPC.
  Phase 81 dual-trait migration tally: 12/13 (a/b/c/d ✅), e absorbed
  by subprocess work.

### Added

- **Phase 81.12.d — Email plugin dual-trait migration to `NexoPlugin`.**
  Fourth per-plugin slice of the `81.12` migration (after 81.12.a/b/c).
  New file `crates/plugins/email/nexo-plugin.toml` (~30 LOC) declares
  `id = "email"`, version `0.1.1`, name `"Email"`, description referencing
  IMAP/SMTP and the multi-account-internal model,
  `min_nexo_version = ">=0.1.0"`, and `requires.nexo_capabilities = ["broker"]`.
  Manifest is **dormant** — top-of-file warning matches the prior
  slices. `EmailPlugin` struct gains a `cached_manifest: PluginManifest`
  field parsed once via `include_str!("../nexo-plugin.toml")` +
  `toml::from_str` in `EmailPlugin::new()`. `impl NexoPlugin for
  EmailPlugin` ships alongside `impl Plugin for EmailPlugin` (dual-trait):
  delegating shape mirrors browser/telegram/whatsapp slices.
  **Single-plugin / multi-account-internal model** — unlike telegram /
  whatsapp where N plugin instances each carry one account's
  `registry_name`, email is ONE plugin with `EmailPluginConfig.accounts:
  Vec<>` driving internal fan-out via `InboundManager` +
  `OutboundDispatcher`. No per-instance label divergence:
  `manifest().plugin.id` and legacy `Plugin::name()` both equal
  `"email"` at all times. Public factory builder
  `pub fn email_plugin_factory(cfg, creds: Arc<EmailCredentialStore>,
  google: Arc<GoogleCredentialStore>, data_dir: PathBuf) -> PluginFactory`
  in `crates/plugins/email/src/lib.rs` returns a closure that clones
  all four arguments per invocation. **Credential injection avoided
  extending `PluginInitContext`** — factory closes over `creds` /
  `google` / `data_dir` at registration time (analog to browser closing
  over `BrowserConfig`). Same pattern lets future plugins with non-config
  dependencies stay factory-side without touching the trait.
  `enabled = false` or empty `accounts` short-circuits inside
  `Plugin::start` returning `Ok(())`, so init-disabled plugins still
  report success through the NexoPlugin path — same observable behavior
  as the legacy `register_arc` + `start_all` combination. The factory
  is exported but no caller registers it today — main.rs's legacy block
  at lines 1914-1937 stays untouched. **Behavior identical to pre-81.12.d
  until Phase 81.12.e flips main.rs**. New deps:
  `nexo-plugin-manifest = { path = "../../plugin-manifest" }` + `toml = "0.8"`
  on the email crate's `Cargo.toml`. No cycle introduced. 4 unit tests
  in `nexo_plugin_tests`: manifest parses + id correct; cached_manifest
  reachable via `&dyn NexoPlugin`; 4-arg factory builder produces a
  usable `Arc<dyn NexoPlugin>`; dual-trait dispatch agrees on identity
  (no multi-instance test — single plugin model). Compatibility audit:
  `EmailPluginConfig: Clone` already derived
  (`crates/config/src/types/plugins.rs:522-529`); zero struct-literal
  callsites for `EmailPlugin` outside the crate (only `EmailPlugin::new`
  callers); hot-reload `apply_account_diff` API untouched; GC ticker
  untouched; `register_email_tools` per-agent untouched (defer Phase 81.3).

- **Phase 81.12.c — WhatsApp plugin dual-trait migration to `NexoPlugin`.**
  Third per-plugin slice of the `81.12` migration (after 81.12.a / browser
  and 81.12.b / telegram). New file `crates/plugins/whatsapp/nexo-plugin.toml`
  (~28 LOC) declares `id = "whatsapp"`, version `0.1.1`, name `"WhatsApp"`,
  description referencing `wa-agent + Signal Protocol`,
  `min_nexo_version = ">=0.1.0"`, and `requires.nexo_capabilities = ["broker"]`.
  Manifest is **dormant** — top-of-file warning matches the prior slices:
  do not add to `plugins.discovery.search_paths` until 81.12.e flips the
  boot wire. `WhatsappPlugin` struct gains a `cached_manifest: PluginManifest`
  field parsed once via `include_str!("../nexo-plugin.toml")` + `toml::from_str`
  in `WhatsappPlugin::new()`. `impl NexoPlugin for WhatsappPlugin` ships
  alongside the existing `impl Plugin for WhatsappPlugin` (dual-trait):
  `manifest()` returns `&self.cached_manifest`; `init(ctx)` calls the
  legacy `Plugin::start(self, ctx.broker.clone()).await` and maps any
  `anyhow::Error` to `PluginInitError::Other { plugin_id, source }`;
  `shutdown()` mirrors via `Plugin::stop` mapped to
  `PluginShutdownError::Other`. Public factory builder
  `pub fn whatsapp_plugin_factory(cfg: WhatsappPluginConfig) -> PluginFactory`
  in `crates/plugins/whatsapp/src/lib.rs` returns a closure that clones
  `cfg` per invocation and constructs an `Arc<dyn NexoPlugin>`. **Multi-account
  is operator-side**: the factory captures one `WhatsappPluginConfig` per
  call, so multi-account setups invoke it once per config (matching the
  shape of the existing `src/main.rs:1880-1897` loop). Distinct
  `session_dir` per instance keeps Signal Protocol keys isolated — the
  multi-instance test verifies the per-instance fixture builds disjoint
  paths. Crucially, `manifest().plugin.id == "whatsapp"` for every
  instance — the per-instance label (`acct_a`, `acct_b`, …) lives in
  `WhatsappPlugin::registry_name`, NOT in the manifest. The factory
  differentiates instances by closing over distinct configs.
  `enabled = false` short-circuits inside `Plugin::start` returning
  `Ok(())`, so init-disabled plugins still report success through the
  NexoPlugin path — same observable behavior as the legacy `register` +
  `start_all` combination. The factory is exported but no caller registers
  it today — main.rs's legacy loop stays untouched. **Behavior identical to
  pre-81.12.c until Phase 81.12.e flips main.rs**. New deps:
  `nexo-plugin-manifest = { path = "../../plugin-manifest" }` + `toml = "0.8"`
  on the whatsapp crate's `Cargo.toml`. No cycle introduced. 5 unit tests
  in `nexo_plugin_tests`: manifest parses + id correct; cached_manifest
  reachable via `&dyn NexoPlugin`; factory builder produces a usable
  `Arc<dyn NexoPlugin>`; dual-trait dispatch agrees on identity for
  single-account; multi-instance factory yields distinct `registry_name`s
  but identical `manifest().plugin.id`. Compatibility audit
  pre-implementation: `WhatsappPluginConfig: Clone` already derived
  (`crates/config/src/types/plugins.rs:145-190`); zero struct-literal
  callsites for `WhatsappPlugin` outside the crate (only `WhatsappPlugin::new(cfg)`
  callers); `WhatsappPairingAdapter` (registered separately at main.rs)
  untouched; `pairing_state()` accessor for HTTP server polling untouched;
  `register_whatsapp_tools` per-agent at main.rs untouched (defer Phase
  81.3 tool namespace runtime enforcement).

- **Phase 81.12.b — Telegram plugin dual-trait migration to `NexoPlugin`.**
  Second per-plugin slice of the `81.12` migration (after 81.12.a / browser).
  New file `crates/plugins/telegram/nexo-plugin.toml` (~28 LOC) declares
  `id = "telegram"`, version `0.1.1`, name `"Telegram Bot"`,
  `min_nexo_version = ">=0.1.0"`, and `requires.nexo_capabilities = ["broker"]`.
  Manifest is **dormant** — top-of-file warning matches the browser slice:
  do not add to `plugins.discovery.search_paths` until 81.12.e flips the
  boot wire. `TelegramPlugin` struct gains a `cached_manifest: PluginManifest`
  field parsed once via `include_str!("../nexo-plugin.toml")` + `toml::from_str`
  in `TelegramPlugin::new()`. `impl NexoPlugin for TelegramPlugin` ships
  alongside the existing `impl Plugin for TelegramPlugin` (dual-trait):
  `manifest()` returns `&self.cached_manifest`; `init(ctx)` calls the
  legacy `Plugin::start(self, ctx.broker.clone()).await` and maps any
  `anyhow::Error` to `PluginInitError::Other { plugin_id, source }`;
  `shutdown()` mirrors via `Plugin::stop` mapped to
  `PluginShutdownError::Other`. Public factory builder
  `pub fn telegram_plugin_factory(cfg: TelegramPluginConfig) -> PluginFactory`
  in `crates/plugins/telegram/src/lib.rs` returns a closure that clones
  `cfg` per invocation and constructs an `Arc<dyn NexoPlugin>`. **Multi-instance
  is operator-side**: the factory captures one `TelegramPluginConfig` per call,
  so multi-bot setups invoke it once per `TelegramPluginConfig` (matching the
  shape of the existing `src/main.rs:1902-1910` loop). Crucially,
  `manifest().plugin.id == "telegram"` for every instance — the per-instance
  label (`bot_a`, `bot_b`, …) lives in `TelegramPlugin::registry_name`, NOT
  in the manifest. The factory is what differentiates instances by closing
  over distinct configs. The factory is exported but no caller registers it
  today — main.rs's legacy loop stays untouched. **Behavior identical to
  pre-81.12.b until Phase 81.12.e flips main.rs**. New deps:
  `nexo-plugin-manifest = { path = "../../plugin-manifest" }` + `toml = "0.8"`
  on the telegram crate's `Cargo.toml`. No cycle introduced. 5 unit tests:
  manifest parses + id correct; cached_manifest reachable via &dyn NexoPlugin;
  factory builder produces a usable `Arc<dyn NexoPlugin>`; dual-trait dispatch
  agrees on identity for single-bot; multi-instance factory yields distinct
  `registry_name`s but identical `manifest().plugin.id`. Compatibility audit
  pre-implementation: `TelegramPluginConfig: Clone` already derived; struct
  literal callsites are zero outside the crate (only `TelegramPlugin::new(cfg)`
  callers); `TelegramPairingAdapter` registered separately and is out of
  81.12.b scope; `register_telegram_tools` per-agent at main.rs:3350 is out
  of scope (defer Phase 81.3 tool namespace runtime enforcement).

- **Phase 81.12.a — Browser plugin dual-trait migration to `NexoPlugin`.**
  First per-plugin slice of the `81.12` migration. New file
  `crates/plugins/browser/nexo-plugin.toml` (~30 LOC) declares the
  plugin's id (`"browser"`), version, name, description,
  `min_nexo_version = ">=0.1.0"`, and `requires.nexo_capabilities = ["broker"]`.
  The manifest is **dormant** — top-of-file comment instructs operators
  not to add the directory to `plugins.discovery.search_paths` until
  Phase 81.12.e ships the boot wire flip. `BrowserPlugin` struct gains
  a `cached_manifest: PluginManifest` field parsed once via
  `include_str!("../nexo-plugin.toml")` + `toml::from_str` in
  `BrowserPlugin::new()`. `impl NexoPlugin for BrowserPlugin` ships
  alongside the existing `impl Plugin for BrowserPlugin` (dual-trait):
  `manifest()` returns `&self.cached_manifest`; `init(ctx)` calls the
  legacy `Plugin::start(self, ctx.broker.clone()).await` and maps any
  `anyhow::Error` to `PluginInitError::Other { plugin_id, source }`;
  `shutdown()` mirrors via `Plugin::stop` mapped to
  `PluginShutdownError::Other`. Public factory builder
  `pub fn browser_plugin_factory(config: BrowserConfig) -> PluginFactory`
  in `crates/plugins/browser/src/lib.rs` returns a closure that clones
  `config` per invocation and constructs an `Arc<dyn NexoPlugin>` from
  it. The factory is exported but no caller registers it today —
  `src/main.rs`'s legacy plugin registration block (lines 1855-1872
  for browser) stays untouched. **Behavior identical to pre-81.12.a
  until Phase 81.12.e flips main.rs**. New deps: `nexo-plugin-manifest
  = { path = "../../plugin-manifest" }` + `toml = "0.8"` on the
  browser crate's `Cargo.toml`. No cycle introduced (manifest crate
  is standalone). 4 unit tests cover: manifest parses + id correct;
  cached_manifest populated at construction; factory builder produces
  a usable `Arc<dyn NexoPlugin>`; same instance dispatched through
  both `&dyn Plugin` and `&dyn NexoPlugin` agrees on identity.
  Compatibility audit pre-implementation: `PluginInitError::Other` +
  `PluginShutdownError::Other` already exist (no enum variant
  additions needed); `BrowserConfig: Clone` already derived; struct
  literal callsites for `BrowserPlugin` are zero outside the crate
  (only `BrowserPlugin::new(cfg)` callers).

- **Phase 81.12.0 — `PluginFactoryRegistry` foundation (no plugin
  migrations).** New module
  `nexo_core::agent::nexo_plugin_registry::factory` ships the
  manifest-driven plugin instantiation infrastructure that 81.12.a-e
  will populate. `BoxError = Box<dyn std::error::Error + Send + Sync + 'static>`
  type-erases plugin authors' error types. `PluginFactory =
  Box<dyn Fn(&PluginManifest) -> Result<Arc<dyn NexoPlugin>, BoxError> + Send + Sync + 'static>`
  is the closure shape per-plugin authors register at boot. Closures
  capture references to `&AppConfig` (or specific config slices) via
  move-closure so they can build plugin-specific config structs from
  the running daemon's state — sidesteps the `PluginInitContext`
  config-injection gap. `PluginFactoryRegistry { factories: BTreeMap<String, PluginFactory> }`
  with `register / instantiate / is_registered / kinds / len /
  is_empty` methods and two thiserror enums:
  `FactoryRegistrationError::AlreadyRegistered` (duplicate id;
  first-registers-wins) and `FactoryInstantiateError::{NotRegistered, FactoryFailed { source: BoxError }}`.
  Sibling fn `run_plugin_init_loop_with_factory(snapshot, factory_registry, ctx_factory)`
  ships in `init_loop.rs` next to the existing handles-map-based
  `run_plugin_init_loop`; both coexist. Per-plugin behavior:
  unregistered → `InitOutcome::NoHandle` (preserves backward-compat
  during partial migration); registered + factory ok → `Ok { duration_ms }`
  after `init()` succeeds; factory closure errors → `Failed { error }`.
  `wire_plugin_registry` (Phase 81.9 helper) gains a 6th parameter
  `factory_registry: Option<&PluginFactoryRegistry>`. `None` →
  existing path verbatim (every plugin records `NoHandle`). `Some`
  → factory-driven path. `main.rs`'s legacy plugin registration
  block at lines 1855-1941 stays untouched; both callsites
  (`Mode::Run` boot wire + `run_doctor_plugins` handler) pass
  `None` until 81.12.a-e flips them. 5 unit tests in `factory::tests`
  cover register-first / register-duplicate / instantiate-unregistered /
  instantiate-factory-error / instantiate-success. 1 init-loop
  unit test exercises the routing path (registered → Failed,
  unregistered → NoHandle). 2 integration tests in
  `crates/core/tests/plugin_factory_registry_integration.rs`
  cover Some / None paths through the full `wire_plugin_registry`
  pipeline.

  **81.12 split into 6 sub-slices** (one foundation + one per
  legacy plugin + one cleanup):
  - 81.12.0 ✅ — Foundation (this commit)
  - 81.12.a ⬜ — Browser plugin migration (~2h)
  - 81.12.b ⬜ — Telegram plugin migration (~3h)
  - 81.12.c ⬜ — WhatsApp plugin migration (~4h)
  - 81.12.d ⬜ — Email plugin migration (~5h, may extend
    `PluginInitContext` for credential injection)
  - 81.12.e ⬜ — Remove legacy registration block from main.rs
    (~87 LOC removal; only after 81.12.a-d ship dual-trait)

  Total estimated effort to close 81.12 fully: ~16h spread across
  4-5 future sessions.

- **Phase 81.11 — Plugin doctor + capability inventory integration
  (library + tests).** New module
  `nexo_core::agent::nexo_plugin_registry::capability_aggregator`
  exposes `aggregate_plugin_gates(snapshot, core_env_vars, available) -> PluginCapabilityAggregation`.
  Iterates each plugin manifest's `[plugin.capability_gates.gate]`
  array, catalogs gates as `AggregatedGate { plugin_id, env_var,
  kind, risk, effect, hint, state, raw_value }` keyed by `env_var`,
  and computes runtime state per `GateKind` (Boolean / Allowlist
  via `env::var`; CargoFeature always `Disabled` because runtime
  env lookup is meaningless for compile-time gates). Conflict
  detection at aggregate time: plugin gate `env_var` matches a
  core INVENTORY entry → drop the plugin gate + emit
  `CapabilityGateConflictsCore` Error diagnostic; plugin gate
  `env_var` matches another plugin's gate → drop the new gate +
  emit `CapabilityGateConflictsPlugin` Error (first-plugin-wins);
  plugin's `requires.nexo_capabilities` entry not in `available`
  set → emit `RequiredCapabilityNotGranted` Warn diagnostic +
  record `UnmetRequirement` (graceful degraded). Three new
  `DiscoveryDiagnosticKind` variants ship. `PluginDiscoveryReport`
  extended with `plugin_capability_gates: BTreeMap<String, AggregatedGate>`
  and `unmet_required_capabilities: Vec<UnmetRequirement>`
  (`#[serde(default, skip_serializing_if = ...)]` for backward-compat
  with 81.5/81.6/81.7/81.8 consumers). Helper
  `fold_capability_aggregation` mirrors `fold_agent_merge` /
  `fold_skill_merge` / `fold_init_outcomes`. `wire_plugin_registry`
  signature gains two new params: `core_env_vars: &[(&str, &str)]`
  + `available_capabilities: &BTreeSet<String>`. The aggregator
  takes an explicit `core_env_vars` slice instead of reading
  `nexo_setup::capabilities::INVENTORY` directly because
  `nexo-core` cannot depend on `nexo-setup` (workspace topology:
  `nexo-setup → nexo-core`). Main.rs bridges via two new helpers:
  `core_capability_env_vars()` (calls `evaluate_all`, projects
  `(env_var, extension)`) and `build_available_capabilities(&cfg)`
  (returns the set of framework capabilities the running daemon
  has wired: always `broker` / `memory` / `sessions`; conditional
  `long_term_memory` when `cfg.memory.long_term.backend` non-empty).
  Boot wire updated + 81.9.b doctor handler updated to thread the
  new args. Drift-prevention contract preserved: `INVENTORY` const
  stays private + immutable; the bridge layer reads through the
  public `evaluate_all` API. 4 unit tests cover single-gate
  aggregation, core conflict, cross-plugin conflict, and unmet-
  requirement Warn emission. 1 integration regression: existing
  `wire_plugin_registry_full_pipeline` fixture updated to pass
  the two new args + asserts empty aggregation when no plugins
  declare gates.

  **Deferred follow-ups (Phase 81.11.b)**:
  - `doctor_render` TTY sections `PLUGIN CAPABILITY GATES` +
    `PLUGIN REQUIRED CAPABILITIES` (data is in the snapshot;
    render layer follows when working tree quiets).
  - `DoctorPluginsJsonReport` field extension for the two new
    aggregator outputs.
  - `agent doctor capabilities --json` envelope mode mixing core
    INVENTORY + plugin gates.
  - Phase 18 reload coord re-aggregation slice (today the 81.10
    hook only re-discovers; capability re-aggregation lands in
    81.11.b).

- **Phase 81.10 — Plugin hot-load via Phase 18 reload coord.** New
  helper `register_plugin_registry_reload_hook(coord, registry,
  discovery_cfg, version)` in
  `nexo_core::agent::nexo_plugin_registry::boot` registers a single
  `PostReloadHook` (sync, captured-state-only per Phase 18 contract)
  with the `ConfigReloadCoordinator`. On every successful reload the
  hook re-runs `discover()`, computes deltas vs the previous
  snapshot's report (loaded / invalid counts), atomically swaps the
  fresh snapshot into `Arc<NexoPluginRegistry>` via the existing
  ArcSwap, and emits a single `tracing::info!` line with six fields
  (prev_loaded / new_loaded / delta_loaded / prev_invalid /
  new_invalid / delta_invalid). Errors are swallowed per the coord's
  best-effort contract — the hook never aborts a reload. The
  snapshot's `skill_roots` field is intentionally left empty
  post-reload: re-merging skill roots would not affect running
  agents (their `LlmAgentBehavior.plugin_skill_roots` is cloned at
  boot), so we stay honest about the runtime state until Phase
  81.10.b ships per-agent skill rebuild. `merge_plugin_contributed_agents`
  is NOT re-run on reload — Phase 18 does not support runtime agent
  removal. `run_plugin_init_loop` is NOT re-run — init handles map
  is empty today; when the manifest-driven factory ships
  (81.7.b / 81.12) its hook slice augments this one. Boot wire is a
  single `await` line in `src/main.rs::Mode::Run` immediately
  before `reload_coord.start(...)`. Two test-only helpers added to
  `ConfigReloadCoordinator` behind `#[cfg(test)]`:
  `post_hooks_len_for_test` and `fire_post_hooks_for_test` so unit
  tests exercise the hook contract directly without spinning a
  real reload pipeline. 3 unit tests cover the registration count,
  the happy-path snapshot replacement, and the failure-swallowing
  edge case (search_path missing on disk → diagnostic recorded,
  no panic).

  **Deferred follow-ups (Phase 81.10.b)**:
  - Skill roots rebuild + per-agent `LlmAgentBehavior.plugin_skill_roots`
    re-clone so running agents pick up new plugin-contributed skills
    without restart.
  - Live `discovery_cfg` updates (operator changes
    `plugins.discovery.search_paths` or `disabled` list) — today
    captured immutable at boot.
  - Init re-run when 81.7.b / 81.12 populate the
    `Arc<dyn NexoPlugin>` handles map.

- **Phase 81.9.b — `nexo agent doctor plugins` CLI subcommand.** Closes
  the deferred CLI piece of Phase 81.9. New `Mode::DoctorPlugins { json: bool }`
  variant + parser arm `[cmd, sub] if cmd == "doctor" && sub == "plugins"`.
  Handler `run_doctor_plugins(config_dir, json) -> Result<i32>` loads
  the daemon config in-process via `AppConfig::load`, runs
  `wire_plugin_registry`, and renders the resulting snapshot via the
  `doctor_render` module shipped in 81.9. Renders 8 sections (LOADED
  PLUGINS / DIAGNOSTICS / PLUGIN AGENTS CONTRIBUTED / AGENT MERGE
  CONFLICTS / PLUGIN INIT OUTCOMES / PLUGIN SKILLS CONTRIBUTED / SKILL
  CONFLICTS / CHANNEL ADAPTERS) plus a header with config path +
  daemon version + a SCANNED-counts line + trailing `EXIT 0|1`. Exits
  1 when error-level diagnostics, `LastPluginWins` agent conflicts, or
  `Failed` init outcomes surface; exits 0 on warn-only states.
  `--json` flag re-emits a `DoctorPluginsJsonReport` (single-line JSON)
  for CI pipelines and admin-ui consumption (Phase 81.11). Help text
  in `print_usage` updated. Operator runs the command BEFORE booting
  to validate plugin discovery + merge + init wiring without a live
  daemon.

- **Phase 81.9 — `wire_plugin_registry` boot helper + boot wire integration
  + `doctor_render` module (library + tests).** New module
  `nexo_core::agent::nexo_plugin_registry::boot` exposes
  `wire_plugin_registry(&mut cfg, discovery_cfg, version) -> WirePluginRegistryOutput { registry, skill_roots, channel_adapter_registry }`.
  The helper runs the four-step pipeline atomically: `discover` →
  `merge_plugin_contributed_agents` → `merge_plugin_contributed_skills`
  → `run_plugin_init_loop` (empty handles → every plugin records
  `InitOutcome::NoHandle` until 81.12 ships the manifest-driven
  factory). All three reports fold into a fresh
  `NexoPluginRegistrySnapshot` whose `last_report` carries the
  full audit + whose `skill_roots` is the runtime routing data.
  Single `tracing::info!(target: "plugins.discovery")` summary at
  end with eight fields (loaded / invalid / disabled / duplicates
  + contributed-agents / contributed-skills / merge-conflicts /
  init-failed totals). `LlmAgentBehavior` (`crates/core/src/agent/llm_behavior.rs`)
  gains `plugin_skill_roots: Vec<PathBuf>` field + `with_plugin_skill_roots(roots)`
  builder; `prepare_system_prompt` chains
  `.with_plugin_roots(self.plugin_skill_roots.clone())` onto the
  existing `SkillLoader::new(...)` builder so plugin-contributed
  skills become discoverable to every agent without the operator
  having to symlink them into `skills_dir`. Operator priority is
  preserved by `candidate_paths` search order — operator's tenant
  / global / legacy chain still wins. `src/main.rs::Mode::Run`
  replaces the existing 81.5.b block (lines 1928-1954) with a
  single `wire_plugin_registry` call. New module
  `nexo_core::agent::nexo_plugin_registry::doctor_render` ships
  `render_text` + `render_json` + `determine_exit_code` +
  `DoctorPluginsJsonReport` struct — the future `nexo agent
  doctor plugins` CLI handler (deferred — see below) consumes
  these. 8 unit tests cover empty / loaded / diagnostics by
  level / agent conflict resolution / init outcome rendering /
  exit code rules (Error diag OR LastPluginWins OR Failed init →
  exit 1; warns alone → exit 0) / JSON shape / EXIT-line
  termination. 1 integration test verifies the full pipeline
  against an on-disk fixture. **Out of scope (deferred follow-up
  81.9.b)**: `Mode::DoctorPlugins` CLI subcommand + parser arm
  + `cmd_doctor_plugins` handler. The CLI body would be a
  ~30-line wrapper that calls `wire_plugin_registry` in-process
  + invokes `render_text` / `render_json`, but main.rs has
  pre-existing in-progress diagnostic noise that made the CLI
  surgery risky in this commit. The render module + helper are
  ready; the CLI ships when the working tree quiets. The
  "reduce ~500 LOC boot wire" headline goal remains open until
  81.12 migrates whatsapp/telegram/email/browser plugins to the
  `NexoPlugin` trait.

- **Phase 81.8 — `ChannelAdapter` trait + registry + `PluginInitContext`
  handle (library + tests).** New module
  `nexo_core::agent::channel_adapter` defines a minimal async trait
  with 4 methods (`kind / start / stop / send_outbound`) so
  plugins can ship new channel kinds (SMS, Discord, IRC, Matrix,
  custom webhooks) without modifying nexo-core. `OutboundMessage`
  enum carries 3 variants — `Text { to, body }`, `Media { to,
  url, caption }`, `Custom(serde_json::Value)` — with `Custom` as
  the escape hatch for adapter-specific shapes (Discord embeds,
  SMS template params). `OutboundAck { message_id, sent_at_unix }`
  is the minimal acknowledgement. `ChannelAdapterError` thiserror
  enum has 6 variants (Connection / Authentication / Recipient /
  RateLimited / Unsupported / Other) so callers discriminate
  retry-vs-fail-vs-fallback without parsing strings.
  `ChannelAdapterRegistry` (process-wide, `std::sync::RwLock`-
  backed) implements **first-registers-wins-rest-rejected**:
  channels compete for broker topic exclusivity, so the second
  plugin attempting to register an already-claimed kind receives
  `ChannelAdapterRegistrationError::KindAlreadyRegistered` (the
  rejected plugin's other registrations — tools / advisors /
  hooks — are untouched). This is asymmetric vs Phase 81.6/81.7's
  first-plugin-wins for agents/skills, by design and documented.
  `PluginInitContext` (Phase 81.2) gains a new field
  `channel_adapter_registry: Arc<ChannelAdapterRegistry>` —
  additive, no `NexoPlugin` trait method change. Plugin's
  `init()` calls
  `ctx.channel_adapter_registry.register(Arc::new(MyAdapter), self.manifest().plugin.id.clone())?`.
  `DiscoveryDiagnosticKind` extended with
  `ChannelKindAlreadyRegistered { channel_kind,
  prior_registered_by, attempted_by }` variant (field renamed
  from `kind` to `channel_kind` to avoid colliding with the
  enum's serde tag). 6 unit tests + 2 integration tests cover
  registry semantics + serde round-trip + duplicate rejection +
  `OutboundMessage` Text / Media / Custom dispatch. Legacy
  `Plugin` trait (whatsapp / telegram / email / browser plugins)
  stays untouched; migration to `ChannelAdapter` is Phase 81.12's
  scope. **Out of scope (deferred bundle)**: boot wire that
  threads the registry into the agent runtime's outbound
  dispatcher + `nexo agent doctor plugins` CHANNEL ADAPTERS
  section. The bundle ships alongside 81.5.b / 81.6 / 81.7
  wires when the working tree's pre-existing diagnostics quiet.

- **Phase 81.7 — Plugin-contributed skills_dir cataloging + `SkillLoader::with_plugin_roots`
  (library + tests).** New module
  `nexo_core::agent::nexo_plugin_registry::contributes_skills`
  exposes `merge_plugin_contributed_skills(snapshot) -> SkillsMergeReport`.
  Walks each loaded plugin's `skills.contributes_dir` (already in
  Phase 81.1's manifest), indexes any subdir containing `SKILL.md`
  as a contributed skill named after the dir. Records per-plugin
  root + first-plugin-wins attribution map + per-plugin contributed
  list. `SkillConflict { skill_name, plugin_ids }` simple struct
  (no resolution enum — only one outcome since search order is the
  resolver). `SkillLoader` extended in `crates/core/src/agent/skills.rs`
  with `plugin_roots: Vec<PathBuf>` field + `with_plugin_roots(roots)`
  builder; `candidate_paths()` appends each plugin root AFTER the
  existing tenant + global + legacy chain so operator content
  always wins by search order. **NO `allow_override` for skills** —
  security stance: skills exec subprocesses, plugin replacing
  operator skill is an escalation vector. `NexoPluginRegistrySnapshot`
  gains `skill_roots: BTreeMap<plugin_id, PathBuf>` for runtime
  routing data (separate from audit data in `last_report`).
  `PluginDiscoveryReport` extended with
  `contributed_skills_per_plugin` + `skill_conflicts` (both
  `#[serde(default, skip_serializing_if = ...is_empty)]` for
  backward-compat with 81.5/81.6 consumers). `fold_skill_merge`
  helper mirrors `fold_agent_merge`. 6 unit tests + 1 integration
  test (`crates/core/tests/plugin_skills_integration.rs`) covering
  the full discover → merge → load pipeline. **Out of scope
  (deferred bundle)**: boot wire in `src/main.rs::Mode::Run`
  threading `snap.skill_roots` into per-agent `SkillLoader`
  instantiation + `nexo agent doctor plugins` CLI sections (PLUGIN
  SKILLS CONTRIBUTED, SKILL CONFLICTS). The boot wire bundle ships
  alongside 81.5.b / 81.6 wires when the working tree quiets.

- **Phase 81.6 — Plugin-contributed agent merge + `NexoPlugin::init()` driver
  (library + tests).** New module
  `nexo_core::agent::nexo_plugin_registry::contributes` exposes
  `merge_plugin_contributed_agents(snapshot, &mut base) -> AgentMergeReport`.
  Walks each loaded plugin's `agents.contributes_dir` (already in
  Phase 81.1's manifest schema), reads `*.yaml` agent configs, folds
  them into the runtime `AgentsConfig` honoring operator-priority via
  load order. Conflict resolution: typed `MergeResolution { OperatorWins
  | PluginOverrideAccepted | LastPluginWins }`. Per-plugin
  `agents.allow_override = true` flips precedence so plugin
  contribution replaces the operator's. Attribution lives in a
  sidecar `BTreeMap<agent_id, plugin_id>` returned in the merge
  report — `AgentConfig` schema unchanged. Companion module
  `init_loop` ships `run_plugin_init_loop(snapshot, handles, ctx_factory)
  -> BTreeMap<plugin_id, InitOutcome>` async sequential driver: each
  plugin's `NexoPlugin::init()` is called in registry order; failures
  log `tracing::warn!` and the loop continues; absence of a handle
  yields `InitOutcome::NoHandle`. `PluginDiscoveryReport` extended
  with `contributed_agents_per_plugin`, `agent_merge_conflicts`,
  `init_outcomes` — all `#[serde(default, skip_serializing_if = ...)]`
  so the 81.5 wire format stays backward-compat. 8 unit tests + 1
  integration test in `crates/core/tests/plugin_contributes_integration.rs`
  cover the full discover → merge → init pipeline. **Out of scope
  (deferred follow-up alongside 81.7)**: boot wire in
  `src/main.rs::Mode::Run` + `nexo agent doctor plugins` CLI
  subcommand. Today every plugin records `NoHandle` because no
  consumer constructs `Arc<dyn NexoPlugin>` from a manifest yet —
  81.7 ships that handle factory.

- **Phase 81.5 — `NexoPluginRegistry` filesystem discovery (library + tests).**
  New module `crates/core/src/agent/nexo_plugin_registry/` consumes the
  Phase 81.1 manifest schema + 4-tier validator. `discover()` walks
  operator-configured `search_paths` (with `$NEXO_HOME` / `$HOME` env
  expansion), reads `<plugin_dir>/nexo-plugin.toml` at fixed path with
  `WalkDir::max_depth(2)`, validates each manifest, and produces a
  `NexoPluginRegistrySnapshot` keyed by plugin id. Snapshot held in an
  `ArcSwap` for Phase 18 hot-reload zero-contention reads. Typed
  `DiscoveryDiagnostic` enum covers 10 kinds: SearchPathMissing /
  ManifestParseError / ValidationFailed / SymlinkEscape /
  PermissionDenied / DuplicateId / VersionMismatch / Disabled /
  AllowlistRejected / UnresolvedEnvVar. Symlink-escape detection via
  canonicalize-boundary check when `follow_symlinks=false`. Operator
  config under `plugins.discovery` block in YAML (loaded from
  `<config_dir>/plugins/discovery.yaml` if present; absent file ⇒ empty
  search_paths ⇒ no scan). 16 unit tests + 1 integration test (ArcSwap
  swap semantics + on-disk tree round-trip). **Out of scope (deferred
  to 81.6)**: boot wire in `src/main.rs::Mode::Run` + `nexo agent
  doctor plugins` CLI subcommand — 81.6 will wire both alongside
  `NexoPlugin::init()` invocation. Library + tests ship now so
  downstream sub-phases (81.6/7/8/9) have a stable consumer surface.

- **Phase 84.1 → 84.4 — Coordinator + Worker personas + worker
  continuation primitives.** Closes the gap where `BindingRole`
  (Phase 77.18) only restricted the tool surface but didn't
  shape the agent's behavior. Four sub-phases shipped:
  - **84.1**: `crates/core/src/agent/personas/coordinator.rs`
    exposes `coordinator_system_prompt(CoordinatorPromptCtx)`.
    `EffectiveBindingPolicy::resolve` prepends the persona block
    when `BindingRole::Coordinator`. Order: persona → agent
    prompt → optional `# CHANNEL ADDENDUM`. Sections cover role
    declaration, curated tool list, `<task-notification>` envelope
    handling, continue-vs-spawn decision matrix, synthesis
    discipline, verification rigor, parallelism, optional
    scratchpad (gated on `TodoWrite`), optional known-workers.
  - **84.2**: `nexo-driver-types::TaskNotification` + `TaskStatus`
    + `TaskUsage` with `to_xml()` / `parse_block()`. All five XML
    entities escaped; `<result>` and `<usage>` collapse out when
    None. `parse_block` returns `None` for plain text so legacy
    callers keep working. `nexo-fork::fork_handle` gains
    `ForkResult::to_task_notification` +
    `fork_error_to_task_notification` so producer paths render
    via one canonical helper.
  - **84.3**: `WorkerRegistry` trait +
    `InMemoryWorkerRegistry` keyed by
    `(coordinator_binding_key, worker_id)`. `SendMessageToWorker`
    coordinator-only LLM tool with all four spec error scenarios:
    `Continued` (success), `UnknownWorker`, `WorkerStillRunning`,
    cross-binding probe returns byte-identical `UnknownWorker`
    (defense-in-depth — no existence oracle). Success path
    returns `pipeline_pending: true` until the fork-as-tool
    spawn pipeline ships (logged in FOLLOWUPS).
  - **84.4**: `crates/core/src/agent/personas/worker.rs` with
    sister `worker_system_prompt(WorkerPromptCtx)` builder.
    Boot-path `apply_persona_prefix` dispatches on role
    (Coordinator → coord, Worker → worker, Proactive/Unset →
    no-op). Worker block emphasizes terse parseable output,
    self-verification before reporting done, and explicit
    "you don't have TeamCreate" reminder.

  Coverage: 12 (84.1 — 7 builder + 5 wire) + 19 (84.2 — 11
  envelope + 8 producer) + 24 (84.3 — 7 registry + 17 tool) +
  9 (84.4 — 6 builder + 3 wire) = 64 net-new tests across
  Phase 84.1-4. Docs: `docs/src/agents/coordinator-mode.md`
  and `docs/src/agents/worker-mode.md`. Deferred follow-ups
  (fork-as-tool spawn pipeline, transcript resume execution,
  e2e integration test, admin-ui "Agent role" panel) tracked
  in FOLLOWUPS.md under Phases 84.2/84.3.

- **Phase 80.1.b.b.b.c — per-goal_id multi-runner auto_dream
  dispatch.** Closes the multi-tenant gap left open by Phase
  80.1.b.b.b.b: when N agents have `auto_dream.enabled = true`
  the orchestrator now dispatches the per-turn hook against the
  runner that owns the active goal, instead of the
  first-non-None primary. `DriverOrchestrator::auto_dream`
  swapped from `Mutex<Option<Arc<dyn AutoDreamHook>>>` to
  `Mutex<HashMap<String, Arc<dyn AutoDreamHook>>>` keyed by
  owning `agent_id`. Routing key flows via
  `goal.metadata["agent_id"]`, populated at goal-construction
  time using new `Goal::with_agent_id` /
  `Goal::agent_id` helpers (no breaking schema change — the
  metadata bag already existed). `DreamContext.agent_id` field
  added so runners receive the resolved key alongside
  `goal_id`. Public API: `register_auto_dream(agent_id, hook)`
  (returns the displaced hook), `unregister_auto_dream`,
  `auto_dream_agents` (sorted ids — stable for assertions),
  `has_auto_dream`. Boot wire in `src/main.rs::Mode::Run` now
  iterates every active runner and registers it under its
  `agent_id`; single `tracing::info!` summary lists
  `agents = N, registered = [...]`. Empty agent_id with
  non-empty registry → warn (the goal didn't declare its
  owner); unknown agent_id → debug (multi-tenant SaaS
  legitimately carries stale metadata). Backward compat shim
  `set_auto_dream(Option<...>)` from Phase 80.1.b.b.b.b retained
  behind `#[deprecated]` and routes to the sentinel `"_default"`
  key with warn-once via `OnceLock`. Verification: 5 integration
  tests in `crates/driver-loop/tests/orchestrator_auto_dream_registry_test.rs`
  plus 4 unit tests on the `with_agent_id` / `agent_id()`
  helpers. Hot-reload propagation (Phase 18 reload loop calling
  the new register/unregister APIs), lifecycle event for
  admin-ui, and Prometheus gauge for `auto_dream_agents.len()`
  remain open follow-ups.

- **Phase 80.1.b.b.b.b — AutoDreamRunner now attaches to the
  orchestrator at runtime.** Fixes the structural gap that
  Phase 80.1.b.b.b consumer left open: the
  `DriverOrchestrator::builder().auto_dream(hook)` call site
  lives inside `boot_dispatch_ctx_if_enabled` (~line 2255 of
  `Mode::Run`), which runs **before** the per-agent loop
  builds the runners (~line 2827). The fix moves the
  `auto_dream` field behind a stdlib `Mutex<Option<Arc<dyn
  AutoDreamHook>>>` and exposes a public `set_auto_dream`
  setter on `DriverOrchestrator`. The boot wire picks the
  primary runner after the per-agent loop closes and attaches
  it via `dispatch_ctx.orchestrator.set_auto_dream(Some(primary))`.
  When `dispatch_ctx` is `None` (no agent has
  `dispatch_capability=full`) the runner stays reachable via
  the `dream_now` LLM tool only — operators see the state in
  `tracing::info!` so the limitation is visible. Per-goal_id
  multi-runner routing stays open as Phase 80.1.b.b.b.c; MVP
  picks the first runner with a single warn listing the
  skipped agent ids. `arc_swap::ArcSwapOption` was the
  zero-cost alternative but requires `T: Sized` and
  `dyn AutoDreamHook` is unsized — stdlib `Mutex` is the
  lowest-friction wrap (per-turn read cost = one uncontended
  lock acquire).

- **Phase 80.1.b.b.b — AutoDreamRunner consumer wired in
  `Mode::Run`.** Per-agent loop in `src/main.rs` constructs an
  `AutoDreamRunner` for every agent with `auto_dream.enabled =
  true`. Wires the full constellation:
  `nexo_fork::AgentToolDispatcher` (new bridge between the
  parent's `Arc<ToolRegistry>` and the fork loop's
  `ToolDispatcher` trait), `parent_ctx_template`,
  `MemoryGitCheckpointer`, and the **Phase 36.2 MS-3
  `PreDreamSnapshotAdapter`** when
  `memory.snapshot.auto_pre_dream = true`. Closes MS-3: every
  fork pass now captures a rollback bundle before mutating
  memdir.
  - The `dream_now` LLM tool (Phase 80.1.c) registers per-agent
    when `NEXO_DREAM_NOW_ENABLED=true` (capability inventory
    entry, Phase 80.1.c.b) and `transcripts_dir` is non-empty.
  - Per-agent tracing emit at boot:
    `auto_dream_enabled`, `has_pre_dream_snapshot`,
    `has_git_checkpointer`, `dream_now_registered`.
  - `nexo_fork::AgentToolDispatcher` lives in `nexo-fork`
    (cycle-clean — nexo-fork already depends on nexo-core).
    Routes `tool_name → handler.call(...)` against a cloned
    `AgentContext`. Failure modes mapped to `Err(String)`:
    unknown tool / handler error / serialization error.
  - Coverage: 6 new dispatcher tests in `nexo-fork`, 2 new
    boot-pre-dream-snapshot tests in `nexo-dream`.
  - Out of scope (Phase 80.1.b.b.b.b follow-up):
    `DriverOrchestrator.builder().auto_dream(primary)` wire +
    multi-runner routing — the builder lives inside
    `boot_dispatch_ctx_if_enabled` which runs before the
    per-agent loop, so the runner Vec isn't populated when the
    orchestrator constructs. Refactor needed.

- **Phase 36.2 — Agent memory snapshots (`nexo-memory-snapshot`
  crate, AGENT_MEMORY_SNAPSHOT feature).** Atomic point-in-time
  bundle of an agent's full memory state — git memdir + four
  SQLite stores (long_term / vector / concepts / compactions) +
  extractor cursor + last dream-run row — packaged as a
  `tar.zst` (or `.tar.zst.age`) archive sealed with two
  independent SHA-256 checks (per-artifact manifest seal +
  whole-file sibling). Designed for rollback after a corrupt
  dream, forensic audit, portable export between hosts, and
  pre-restore safety nets in autonomous mode.
  - **Trait surface**: `MemorySnapshotter` (snapshot / restore /
    list / diff / verify / delete / export). Default impl
    `LocalFsSnapshotter` with a per-agent `tokio::sync::Mutex`
    lock map, atomic `<id>.tar.zst.partial` → final rename, and
    auto-pre-snapshot on every restore so the operation is
    reversible.
  - **Codec**: `tar` + `zstd` (level 19) for the bundle body,
    SQLite via `VACUUM INTO` (online, atomic, WAL-safe — no extra
    deps on top of `sqlx`), git memdir captured as raw `.git/**`
    tar entries. Optional age encryption behind Cargo feature
    `snapshot-encryption`; manifest stays plaintext so integrity
    is verifiable without the identity.
  - **Operator CLI**: `nexo memory {snapshot, restore, list, diff,
    export, verify, delete}`. Each subcommand is standalone.
    `verify` exits with code 2 on any integrity failure; `restore`
    is gated on `NEXO_MEMORY_RESTORE_ALLOW=true` (capability
    inventory entry registered as `Critical, Boolean`).
  - **LLM tool**: `memory_snapshot` (write-only, deferred schema
    per Phase 79.2) registered in `MUTATING_TOOLS` and
    `EXPOSABLE_TOOLS`. Restore is intentionally **not** exposed
    as a tool — destructive surface stays operator-only.
  - **Boot wire**: `src/main.rs::Mode::Run` reads
    `cfg.memory.snapshot` directly, builds a single
    `Arc<dyn MemorySnapshotter>` shared across every agent's tool
    registry, and spawns `RetentionWorker` with the daemon's
    shutdown token (initial sweep + periodic GC + orphan staging
    cleanup left by SIGKILL).
  - **YAML config**: `nexo_config::types::memory::SnapshotYamlConfig`
    (with sub-blocks for encryption / retention / events / memdir
    / sqlite roots) ships as a wire-shape mirror of the in-crate
    `MemorySnapshotConfig` (cycle-break pattern Phase 77.7
    introduced for SecretGuard).
  - **Adapters**: `PreDreamSnapshotAdapter` bridges the
    snapshotter to `nexo_driver_types::PreDreamSnapshotHook` for
    `AutoDreamRunner::with_pre_dream_snapshot`;
    `MemoryMutationPublisher` bridges `EventPublisher` to
    `nexo_driver_types::MemoryMutationHook` so memory writes
    stream onto `nexo.memory.mutated.<agent_id>` NATS subject.
    `crates/memory/src/long_term.rs::LongTermMemory` has the
    first fire-site wired (`remember_typed` → `Insert`,
    `forget` → `Delete`).
  - **Metrics**: `nexo_memory_snapshot_total{agent,tenant,outcome}`,
    `_restore_total`, `_gc_total`, `_bytes_total`, `_duration_ms`
    histogram (8 buckets 50ms→60s) with 256-label cardinality
    cap, stitched into the runtime's `/metrics` aggregator.
  - **Documentation**: `docs/src/ops/memory-snapshot.md` covers
    bundle layout, CLI surface, YAML config, threat model,
    retention semantics, and restore mechanics.
    `admin-ui/PHASES.md::Phase A7` extended with the
    snapshot-panel checklist.
  - **Coverage**: 364 tests across the feature graph
    (143 `nexo-memory-snapshot`, 105 `nexo-memory`, 28
    `nexo-driver-types`, 8 `nexo-core`, 3 `nexo-config`, 77
    `nexo-dream`).

- **Phase 80.9 — MCP channel routing + 5-step gate.** MCP
  servers can now act as inbound surfaces (Slack bots, Telegram
  chats, iMessage relays) — they declare a capability, push
  user messages via `notifications/nexo/channel`, and the
  runtime routes the content into the agent's conversation as
  a wrapped `<channel source="...">...</channel>` user input.
  The gate is the trust boundary: a server only registers when
  it survives 5 ordered checks.
  - Schema (`crates/config/src/types/channels.rs`, ~250 LOC,
    10 tests verde): `ChannelsConfig { enabled, approved,
    max_content_chars }` + `ApprovedChannel { server,
    plugin_source }`. Per-binding `InboundBinding.
    allowed_channel_servers` closes the loop.
  - Gate (`crates/mcp/src/channel.rs`, ~700 LOC, 39 tests
    verde): pure-fn `gate_channel_server` with typed `SkipKind`
    (`Capability` / `Disabled` / `Session` / `Marketplace` /
    `Allowlist`). `has_channel_capability(experimental)` parses
    `experimental['nexo/channel']` truthiness across object /
    bool / non-empty string / array shapes.
    `wrap_channel_message(server, content, meta)` produces the
    XML wrapper with three injection-defence layers:
    identifier-shape meta-key whitelist (`[A-Za-z_][A-Za-z0-9_]*`),
    XML attribute-value escape (control chars + line breaks
    rendered as numeric refs), separate source-attr escape.
    Inner content stays verbatim — the model has to read the
    user's text as-is.
  - Notification parsing: `parse_channel_notification` →
    `ChannelInbound { server_name, content, meta, session_key }`,
    `ChannelParseError` thiserror-typed.
  - Session correlation: `ChannelSessionKey::derive` picks
    the first known threading meta key (`thread_ts`,
    `chat_id`, `conversation_id`, `room_id`, `channel_id`,
    `thread_id`, `to`) and renders `server|key=value`.
    Deterministic across processes — process A's dispatcher
    and process B's registry agree.
  - Cross-process routing: `channel_inbox_subject(binding,
    server)` → `mcp.channel.<binding>.<server>` (dots
    replaced); wildcard `mcp.channel.>`. `ChannelDispatcher`
    async trait + `DispatchError`. `ChannelEnvelope { schema=1,
    binding_id, server_name, content, meta, session_key,
    rendered (XML pre-render), sent_at_ms, envelope_id (Uuid) }`.
  - Per-process registry: `ChannelRegistry` backed by
    `RwLock<BTreeMap<(binding, server), RegisteredChannel>>`
    with `register/unregister/get/list_for_binding/list_all/
    count` + `SharedChannelRegistry = Arc<...>` typedef.
  - LLM tool (`crates/core/src/agent/channel_list_tool.rs`,
    ~140 LOC, 3 tests verde): `channel_list` returns
    `{ binding_id, count, servers: [ChannelSummary] }`.
    Read-only, auto-approve-friendly. `register_channel_list_tool`
    boot helper.
  - Counts: 52 new tests verde (39 channel + 10 schema + 3
    tool). Workspace totals: 397 nexo-mcp + 763 nexo-core +
    193 nexo-config.
  - Deferred 80.9.b permission relay (capability + outbound
    method already reserved), 80.9.e operator CLI, 80.9.f
    hot-reload integration, 80.9.g per-channel rate limits,
    80.9.h turn-log audit marker, 80.9.i `channel_send` LLM
    wrapper.

- **Phase 80.9.c — Live MCP-client channel wire.**
  Closes the seam from "MCP server emits notification" to
  "envelope lands on `mcp.channel.<binding>.<server>` NATS
  subject" without any operator-side wiring beyond the
  existing 80.9 config.
  - `McpCapabilities.experimental: Value` retains the raw
    `experimental` block from the MCP `initialize` response
    so `has_channel_capability` can inspect it. Older
    servers that don't emit `experimental` parse cleanly
    (default `Value::Null`).
  - `ClientEvent::ChannelMessage { params }` variant +
    `channel_message_event(params)` constructor so the
    typed event dispatch carries the user-visible payload.
    `method_to_event` keeps its payload-free shape for
    every other variant.
  - `client.rs:750-787` detects
    `CHANNEL_NOTIFICATION_METHOD` (`notifications/nexo/channel`)
    and emits the typed event with captured params.
  - `BrokerChannelDispatcher` impl of `ChannelDispatcher`
    that serialises `ChannelEnvelope` and publishes through
    `AnyBroker` on `mcp.channel.<binding>.<server>`.
    Configurable source tag for audit-log distinction.
  - `ChannelInboundLoop` + `ChannelInboundLoopConfig` +
    `ChannelInboundLoopHandle` drive the per-server pump:
    one-shot gate at spawn → register → consume
    `ChannelMessage` events → `parse_channel_notification`
    → `dispatcher.dispatch`. Survives parse errors +
    dispatch failures + slow-consumer `Lag`; cleans up the
    registry on cancel and on events-closed.
  - 8 new tests verde (5 loop + 1 broker dispatcher + 2
    events).

- **Phase 80.9.g — Per-channel rate limit.** Token bucket
  per `(binding, server)` consulted before each dispatch;
  empty bucket drops the message with a structured
  warn so a noisy MCP server can't flood the conversation.
  - `ChannelRateLimit { rps: f64, burst: u32 }` schema with
    `is_active()` + `validate(label)`. `0` on either field is
    "no throttle". 1000 rps soft cap catches typo bugs
    (rps vs rps-per-minute).
  - `ChannelsConfig.default_rate_limit: Option<ChannelRateLimit>`
    sets the global ceiling; `ApprovedChannel.rate_limit`
    overrides per-server. `resolve_rate_limit(server)`
    helper picks the effective value.
  - `TokenBucket::new(rl)` + `try_acquire()` async helper
    with lazy refill on each call — no background task. Bucket
    state is `Mutex<{tokens: f64, last_refill: Instant}>`;
    contention bounded by inbound rps.
  - `ChannelInboundLoop` builds `Option<Arc<TokenBucket>>`
    once at spawn from the resolved rate limit and consults
    it before each dispatch.
  - 5 runtime tests + 8 schema tests verde (burst-then-blocks /
    refill-rate / cap-at-burst / drops-when-empty /
    unrate-limited / negative / nan / excessive / override /
    fallback / inactive / yaml-round-trip).

- **Phase 80.9.j — Per-binding channel-tool granularity.**
  Channel tools resolve `binding_id` from `ctx.effective`
  at call time instead of being hard-wired to one binding.
  - `ChannelListTool::new_dynamic(registry)` +
    `ChannelSendTool::new_dynamic` +
    `ChannelStatusTool::new_dynamic` constructors leave the
    binding_id unset; `resolve_binding_id(ctx)` shared
    helper produces `<plugin>:<instance>` from
    `ctx.effective.binding_index`, falls back to
    `ctx.agent_id` for paths without a binding match
    (heartbeat, delegate receive, tests).
  - `main.rs`: per-agent registration uses the dynamic
    constructors; `ChannelInboundLoop` spawn site iterates
    `agent_cfg.inbound_bindings` and keys each loop with
    `binding_id = "<plugin>:<instance>"` — the registry view
    each tool sees scopes to the active binding instead of
    the agent.
  - 2 new tests verde (static-binding-stored /
    dynamic-binding-deferred).

- **Phase 80.9 main.rs hookup — channels live end-to-end.**
  Closes the seam from "MCP server emits notification" to
  "agent receives `<channel>` user message". The four
  pieces shipped:
  - `ChannelBootContext::in_memory(broker)` instantiated
    once at boot right after the broker is ready. Holds the
    shared `ChannelRegistry` + `InMemorySessionRegistry` +
    `BrokerChannelDispatcher`.
  - `IntakeChannelSink` impl of
    `nexo_mcp::channel_bridge::ChannelInboundSink`: serialises
    each `ChannelInboundEvent` into a JSON envelope and
    publishes on `agent.channel.inbound` so the existing
    intake task picks it up under the same pairing /
    dispatch / rate-limit gates as every other channel
    inbound (WhatsApp, Telegram, email).
  - `channel_boot.spawn_bridge(sink, channel_shutdown)`
    spawns one consumer + one GC ticker per process. Both
    stop cleanly on the shared cancellation token. Boot
    failures warn-log but never block the daemon.
  - Per-agent: when `agent_cfg.channels` is `Some(cfg)` AND
    any binding has a non-empty
    `allowed_channel_servers`, register `channel_list` +
    `channel_send` + `channel_status` tools agent-scoped
    (per-binding granularity is the 80.9.j follow-up).
  - Per-agent post-MCP: walk `rt.clients()` and spawn a
    `ChannelInboundLoop` per `(server, binding=agent_id)`
    when the union of binding allowlists contains the
    server. The loop reads
    `client.capabilities().experimental` to detect the
    `nexo/channel` (and optional
    `nexo/channel/permission`) capability flags, runs the
    one-shot gate, and either registers + consumes
    `ChannelMessage` events or surfaces a typed
    `Skipped { kind, reason }` log.
  - CLI smoke verde: `nexo channel list --json` enumerates
    every agent (channels off by default surfaces clean
    output); `nexo channel doctor` reports
    "(no channel-using bindings found)" against an empty
    config without panicking.

- **Phase 80.9.f — Channel hot-reload re-evaluation.**
  YAML edits flow into the registry on the next Phase 18
  reload — operators don't have to restart the daemon to
  flip `channels.enabled` or drop a server from `approved`.
  - `ChannelRegistry::reevaluate(&ReevaluateInputs)` walks
    every active registration and re-runs the static half
    of the gate against the operator's current config.
    Entries that no longer pass get unregistered with a
    typed `SkipKind` reason.
  - `ReevaluateInputs { by_binding }` +
    `ReevaluateBinding { cfg, allowed_servers }` —
    snapshot caller builds from the freshly-loaded YAML.
    Missing bindings count as deletions.
  - `ReevaluateReport { kept, evicted }` exposes the
    delta so `setup doctor` and audit logs can surface
    what changed on the reload.
  - `channel_boot::build_reevaluate_inputs(iter)` builder
    helper accepting flat `(binding_id, Arc<ChannelsConfig>,
    Vec<String>)` tuples.
  - 6 new tests verde: keeps-passing /
    killswitch-evict / removed-from-approved-evict /
    binding-disappears-evict / plugin-source-mismatch-evict /
    partial-some-kept-some-evicted.
  - 465 nexo-mcp tests verde (was 459, +6).
  - **Caller wiring deferred**: the Phase 18 reload
    coordinator post-hook still has to call
    `registry.reevaluate(...)` after each successful
    reload — that's a one-line addition once the upstream
    main.rs hookup lands.

- **Phase 80.9.h — Channel turn-log audit marker.**
  Audit tooling can now answer "what came in via Slack
  today?" with a single SQL filter. `TurnRecord.source:
  Option<String>` carries `"channel:<server>"` when the
  inbound that drove the turn arrived via an MCP channel
  server; `None` for every other intake path.
  - `crates/agent-registry/src/turn_log.rs`:
    - `TurnRecord.source` field with `#[serde(default)]`
      so legacy JSON / older callers parse cleanly.
    - Idempotent `ALTER TABLE goal_turns ADD COLUMN
      source TEXT` migration; "duplicate column" errors are
      tolerated. New `idx_goal_turns_source` index keeps
      per-server filtering cheap.
    - INSERT + UPSERT on `(goal_id, turn_index)` carry the
      column; both `tail` and `tail_since` SELECTs include
      it.
    - Pure-fn helpers `format_channel_source(server)` and
      `parse_channel_source(source)` keep the `channel:`
      prefix stable across the codebase.
  - `crates/dispatch-tools/src/event_forwarder.rs` builder
    documents that the forwarder always emits `source:
    None`; the channel inbound sink is the only writer of
    a non-`None` value.
  - 4 new tests verde (round-trip / default-none / replay
    idempotency / prefix render + parse). Total
    nexo-agent-registry: 51 lib tests verde.

- **Phase 80.9.d.b — Persistent SQLite SessionRegistry.**
  `session_key → session_uuid` mapping now survives daemon
  restarts. Slack threads, Telegram chats, iMessage
  conversations no longer have to re-introduce themselves
  on every reboot.
  - `crates/mcp/src/channel_session_store.rs` (~250 LOC + 9
    tests verde): `SqliteSessionRegistry` impl of
    `SessionRegistry` trait. Schema
    `mcp_channel_sessions(key PRIMARY KEY, session_id,
    last_seen_ms)` + `last_seen` index, idempotent
    `CREATE … IF NOT EXISTS`, WAL + `synchronous=NORMAL`
    matching Phase 71/72 stores.
  - `resolve` is a single `UPSERT … RETURNING` (SQLite ≥
    3.35) — first-seen and refresh take one round-trip.
    Concurrent-safe verified by a parallel-resolve test.
  - `gc_idle(max_idle_ms)` is a bulk `DELETE` keyed on
    `last_seen_ms < cutoff`; `0` and negative values are
    no-op sentinels.
  - Fail-safe: UPSERT errors warn-log + return an ephemeral
    uuid for the in-flight turn so threading-this-turn is
    preserved even when persistence is degraded.
  - 9 tests verde including schema-idempotent-across-reopens
    (real tempfile, not `:memory:`) and concurrent-UPSERT
    determinism.
  - 459 nexo-mcp tests verde (was 450, +9 sqlite store).

- **Phase 80.9.b — Channel permission relay protocol.**
  An MCP channel server can now act as a *second* approval
  surface for tool prompts. The runtime emits
  `notifications/nexo/channel/permission_request` to the
  server; the user replies on the underlying platform; the
  server parses the reply and emits
  `notifications/nexo/channel/permission` with
  `{request_id, behavior}`. The runtime races that response
  against the local approval prompt — first to claim wins.
  - `crates/mcp/src/channel_permission.rs` (~620 LOC + 27
    tests verde):
    - Wire constants (`PERMISSION_REQUEST_METHOD`,
      `PERMISSION_RESPONSE_METHOD`, schema version).
    - `PermissionBehavior { Allow, Deny }` + serde round-trip
      + `parse(raw)` case-insensitive.
    - `PermissionRequestParams` (outbound) +
      `PermissionResponseParams` (inbound) +
      `PermissionResponse` (audit bundle with `from_server`).
    - `short_request_id(tool_use_id)` — FNV-1a + base-25
      5-letter ID, alphabet a-z minus `l`, substring
      blocklist with re-hash on hit (verified against 2000
      sampled inputs).
    - `truncate_input_preview(value)` — JSON-serialise +
      200-char cap with `…` suffix.
    - `parse_permission_reply(text)` server-side helper —
      lowercase prefix tolerated, ID itself must be
      lowercase to preserve the anti-confusable alphabet.
    - `PendingPermissionMap` — process-local rendezvous
      (`Mutex<HashMap<id, oneshot::Sender>>`); `register`,
      `resolve`, `cancel`, `len`. Race-tolerant: `resolve`
      after a dropped receiver returns `false` (lost the
      race) without erroring.
    - `parse_permission_response` with typed
      `PermissionParseError`.
    - `PermissionRelayDispatcher` async trait +
      `McpPermissionRelayDispatcher<C>` adapter scaffold.
  - `ClientEvent::ChannelPermissionResponse { params }`
    variant + `channel_permission_response_event(params)`
    constructor. `client.rs:750-787` detects
    `PERMISSION_RESPONSE_METHOD` and emits the typed event
    (mirror of 80.9.c channel-message detection).
  - 450 nexo-mcp tests verde (was 421, +29 across 27
    permission + 2 events).
  - **Deferred 80.9.b.b**: higher-level approval-flow
    integration that races the channel reply against the
    local prompt + writes the audit row. Lands in
    `nexo-driver-permission` once it grows a pluggable
    "another approver might claim this" seam. The protocol
    surface shipped here is complete and stable.

- **Phase 80.9.e — Channel operator CLI.** Three new
  `nexo` subcommands for channel debuggability without a
  daemon up:
  - `nexo channel list [--config=<path>] [--json]` walks
    every agent and surfaces enabled-state + approved-servers
    + per-binding `allowed_channel_servers`. JSON output is
    machine-readable; markdown output is structured per
    agent.
  - `nexo channel doctor [--config=<path>] [--binding=<id>]
    [--json]` runs the static half of the 5-step gate
    against every `(agent, binding, server)` triple.
    Capability is *assumed* declared (the doctor can't
    probe a live MCP server); gates 2/3/5 run normally.
    Each row reports `WOULD REGISTER` or a typed
    `SKIP { kind, reason }`. Cross-checks approved entries
    that no binding lists and surfaces them as `NOT BOUND`.
  - `nexo channel test <server> [--binding=<id>]
    [--content=...] [--json]` synthesises a sample
    `notifications/nexo/channel` payload, runs the parser
    + the XML wrap helper, and prints the model-facing
    `<channel>` block plus the derived `session_key`.
  - `load_app_config_for_channels` helper — accepts a
    directory or single-file `--config` override (walks up
    to parent for files), defaults to `--config-dir`.
  - Smoke-tested locally on a YAML without channels — output
    renders cleanly as "no channel-using bindings found"
    and `channel list` surfaces `enabled: false` per agent
    without panicking.

- **Phase 80.9 outbound + boot helpers.** Closes the loop from
  "envelope on `mcp.channel.>`" to "agent has vocabulary to
  reply" with three new pieces:
  - `ApprovedChannel.outbound_tool_name: Option<String>` (default
    `Some("send_message")`) + `resolved_outbound_tool_name()`
    helper; `validate()` rejects empty overrides.
  - `RegisteredChannel.outbound_tool_name` snapshots the resolved
    value at register-time so an in-flight reply still hits the
    tool the operator approved when they flipped the config.
  - `crates/core/src/agent/channel_send_tool.rs` (~200 LOC + 4
    tests verde): `channel_send` LLM tool routes
    `(server, content?, arguments?)` through
    `SessionMcpRuntime.call_tool` against the resolved outbound
    tool. 5 ordered gates (server present, registered for binding,
    arguments-shape, 64 KiB content cap, MCP runtime wired).
    `content` shortcut populates the argument's `text` key when
    the operator hasn't supplied an explicit `arguments` object.
  - `crates/core/src/agent/channel_status_tool.rs` (~170 LOC + 4
    tests verde): `channel_status` LLM tool diagnoses one server
    or every registered server; renders connection state +
    plugin source + resolved outbound name + permission-relay
    flag + registered-at timestamp.
  - `crates/mcp/src/channel_boot.rs` (~200 LOC + 5 tests verde):
    `ChannelBootContext { broker, registry, session_registry,
    dispatcher }` ties the four shipped pieces (registry,
    session-key map, dispatcher, bridge) into a single value
    main.rs constructs once.
    `ChannelBootContext::in_memory(broker)` is the default
    factory; `bridge_config(sink)` and `spawn_bridge(sink,
    cancel)` cover the per-process spawn site.
    `build_inbound_loop_config(...)` + `enumerate_targets(cfg,
    binding_allowlist)` cover the per-(binding, server) spawn.
    The helpers are pure / construction-time so the caller owns
    cancellation tokens + spawn timing — no implicit task
    scheduling.
  - 13 channels schema tests verde (was 10, +3 around
    outbound-tool resolution + override validation).
  - **Workspace counts**: 421 nexo-mcp (was 416, +5 boot
    helper); 771 nexo-core (was 760, +8 across `channel_send`
    and `channel_status`); 13 nexo-config channels.

- **Phase 80.9.d — Agent-side channel bridge.**
  The consumer side of the channel pipeline. Subscribes to
  `mcp.channel.>`, resolves each envelope's
  `ChannelSessionKey` into a stable agent session uuid, and
  delegates injection into the agent runtime to a
  caller-provided sink. The split keeps "transport" and
  "delivery" testable in isolation and lets every deployment
  path reuse the same primitives.
  - `crates/mcp/src/channel_bridge.rs` (~390 LOC + 9 tests
    verde):
    - `SessionRegistry` async trait — `resolve` (first-seen
      creates uuid, repeats refresh timestamp), `gc_idle`
      (with `0` as no-op sentinel), `len`. Persistent
      SQLite-backed impl deferred 80.9.d.b.
    - `InMemorySessionRegistry` —
      `RwLock<BTreeMap<ChannelSessionKey, SessionEntry>>`,
      determinstic iteration for tests + GC sweeps.
    - `ChannelInboundEvent` — typed payload (binding_id,
      server_name, session_id, session_key, content, meta,
      rendered XML, envelope_id, sent_at_ms) so the sink
      doesn't re-parse JSON.
    - `ChannelInboundSink` async trait + `SinkError`
      (`Rejected` / `Other`). Caller decides which intake
      path runs; typical wiring synthesises an inbound on
      `agent.intake.<binding_id>` so the existing pairing /
      dispatch / rate-limit gates apply unchanged.
    - `ChannelBridge` + `ChannelBridgeConfig` (broker,
      registry, sink, subject defaults to
      `mcp.channel.>`, `gc_interval_ms` default 5 min,
      `max_idle_ms` default 1 h). `spawn(cancel)` returns a
      `ChannelBridgeHandle` with two join-handles: the
      consumer that drains the broker subscription + an
      optional GC ticker. Both stop cleanly on cancel.
    - Threading: same `session_key` → same `session_id`
      across messages; distinct keys (Slack threads,
      Telegram chats) split into distinct sessions.
    - Defensive: malformed envelopes warn-logged + dropped
      (not fatal); sink errors warn-log + continue (bridge
      survives); GC `0` skips eviction; subject filter
      narrows subscription for tenancy isolation.
  - 9 tests verde (registry first-seen / distinct keys / GC
    eviction / GC zero noop; bridge resolves+delivers, logs
    sink failures, threads distinct keys, narrows subject
    filter, GC task runs).
  - **Counts**: 416 nexo-mcp lib tests verde
    (was 397, +19 across 80.9.c and 80.9.d).
  - **Deferred 80.9.d.b**: persistent SQLite
    `SessionRegistry` implementation so the
    `session_key → session_uuid` map survives daemon
    restarts. **Deferred main.rs hookup**: instantiating one
    `ChannelInboundLoop` per `(binding, server)` after the
    MCP handshake + spawning a single `ChannelBridge` per
    process.

- **Phase 80.8 — Brief mode + `send_user_message` tool.** New
  user-visible-output channel for autonomous agents. Free-text
  output remains available for the detail view, but the tool
  becomes the primary surface the user actually reads. Pairs
  with Phase 80.15 assistant mode and Phase 80.17 auto-approve
  to make the autonomous-agent UX coherent.
  - `crates/config/src/types/brief.rs` — `BriefConfig {
    enabled, status_required, max_attachments }` with
    `#[serde(default)]` everywhere, hard cap of 64 attachments
    enforced by `validate()`, `is_active_with_assistant_mode`
    helper that short-circuits on Phase 80.15.
  - `AgentConfig.brief: Option<BriefConfig>` field, 49+
    workspace fixture sites swept.
  - `crates/core/src/agent/send_user_message_tool.rs`
    (~330 LOC + 16 tests verde) ships the
    `SendUserMessageTool` (`name = "send_user_message"`).
    Output JSON carries the `__nexo_send_user_message__`
    sentinel + `sent_at` ISO timestamp + resolved attachment
    metadata. The tool def's required-fields list adapts to
    `cfg.status_required` so operators can pilot brief mode
    on models that aren't yet trained on the proactive flow.
    Four ordered validation gates: message non-empty + 8 MiB
    cap, status enum (`normal` or `proactive`), attachment
    count vs `cfg.max_attachments`, attachment path
    canonicalize + reject directories.
  - `BRIEF_SECTION` constant + `brief_system_section(cfg,
    assistant_addendum_appended)` pure helper wired into
    `llm_behavior.rs` immediately after the Phase 80.15
    assistant-mode addendum site. The section is *skipped*
    when the assistant-mode addendum is already appended —
    that addendum hard-codes the same instruction, so
    duplicating it would only inflate the system prompt.
  - 22 new tests verde (6 on the schema + 12 on the tool +
    4 on the section gate). nexo-core lib total: 760 verde.
  - **Deferred follow-ups**: 80.8.b channel-adapter
    hide-free-text filter, 80.8.c `/brief` slash command for
    live toggling, 80.8.d main.rs boot wiring of
    `register_send_user_message_tool` per binding.

- **Phase 80.2-80.6 — Cron jitter cluster (6-knob hot-reload
  config + deterministic per-entry jitter + recurring/one-shot
  modes + `permanent` flag + killswitch + missed-task sweep).**
  Replaces the single static `with_jitter_pct(pct)` knob with
  six cooperating levers operators can flip per incident
  without restarting the daemon.
  - `crates/config/src/types/cron_jitter.rs` —
    `CronJitterConfig { enabled, recurring_frac, recurring_cap_ms,
    one_shot_max_ms, one_shot_floor_ms, one_shot_minute_mod,
    recurring_max_age_ms }`. Every field has `#[serde(default)]`
    so existing YAML rolls forward; `validate()` rejects
    out-of-range values at boot. `from_legacy_pct(pct)` shim
    keeps the original `with_jitter_pct` signature working.
  - `CronRunner` now holds an
    `Arc<ArcSwap<CronJitterConfig>>` so the Phase 18 reload
    coordinator can swap the inner value atomically; the
    running tick observes the new config on the next read.
  - New pure helpers in `crates/core/src/cron_schedule.rs`:
    - `jitter_frac_from_entry_id(id)` — UUID hex prefix to
      `[0.0, 1.0)`, deterministic across retries, `0.0` fallback
      when the id is short or non-hex (no panic).
    - `apply_recurring_jitter(next, following, from, id, cfg)`
      — forward `t1 + min(frac * (t2 - t1), cap_ms)`, clamps to
      `from + 1` to never schedule into the past.
    - `apply_one_shot_lead(target, from, id, cfg, target_minute)`
      — backward lead `target - max(frac * max_ms, floor_ms)`,
      gated by `target_minute % cfg.one_shot_minute_mod == 0`.
      `one_shot_minute_mod = 0` is the documented "never jitter
      one-shots" sentinel.
  - `CronEntry` gains a `permanent: bool` column with
    `#[serde(default)]` (false). Idempotent
    `ALTER TABLE ... ADD COLUMN permanent INTEGER NOT NULL DEFAULT 0`
    runs on every boot, mirroring the existing `recipient` /
    `model_provider` migrations.
  - `CronStore` trait gains two sweep helpers:
    - `sweep_missed_entries(now, skew_ms)` — boot-time
      quarantine. Rewrites `next_fire_at = i64::MAX` for every
      entry whose stored `next_fire_at` is older than
      `now - skew_ms`. Operator sees them in `cron list` and can
      resume manually, instead of seeing a stampede on the next
      tick. `permanent: true` exempt.
    - `sweep_expired_recurring(now, max_age_ms)` — auto-expire
      recurring rows older than `max_age_ms`. `permanent: true`
      exempt; one-shots untouched (their retry policy is the
      boundary). `max_age_ms == 0` is the no-op default.
  - `CronRunner::tick_once` reads `cfg.enabled` at the top of
    every tick. When `false` the loop short-circuits before
    `due_at(...)` so paused entries stay durable in storage and
    resume on the next `true` tick.
  - 17 new unit tests verde (8 on the schema + 8 on the jitter
    helpers / sweep helpers + 1 killswitch round-trip on the
    runner). Total cron-side test suite: 80 verde.
  - **Deferred follow-up 80.6.b**: the boot helper invocation
    of `sweep_missed_entries` from `src/main.rs` waits on a
    pre-existing dirty-state resolution; the helper is callable
    today with the operator-configured `skew_ms`.

- **Phase 81.2 — `NexoPlugin` trait + `PluginInitContext`**
  (lifecycle contract for native Rust plugins). New module
  `crates/core/src/agent/plugin_host.rs` (~470 LOC + 8 tests
  verde) defines the async trait every Rust plugin implements
  to participate in the plug-and-play system. Trait: `manifest()`
  returns the parsed `PluginManifest` (Phase 81.1), `init(ctx)`
  is called once at boot to register tools/advisors/hooks,
  `shutdown()` defaults to Ok and is overridden only when the
  plugin owns persistent state. `PluginInitContext<'a>` bundles
  11 handles plugins need: `Arc<ToolRegistry>` for tool
  registration, `Arc<RwLock<AdvisorRegistry>>` for advisory_hook
  composition (RwLock so multiple plugins can register without
  contention), `Arc<HookRegistry>` for per-message extension
  hooks (Phase 11.6), `AnyBroker` for NATS pub/sub,
  `Arc<LlmRegistry>` for provider-agnostic LLM client builds,
  `Arc<ConfigReloadCoordinator>` for Phase 18 hot-reload
  post-hooks, `Arc<SessionManager>`,
  `Option<Arc<LongTermMemory>>`, `&Path` config_dir + state_root,
  `CancellationToken` for shutdown signal. Helpers
  `plugin_config_dir(id)` and `plugin_state_dir(id)` resolve
  `<root>/plugins/<id>/` paths so every plugin uses the same
  layout. `PluginInitError` enum 5 variants thiserror-typed
  (`MissingNexoCapability`, `UnregisteredTool`, `Config`,
  `ToolRegistration`, `Other`) — every variant carries
  `plugin_id: String` so registry logs identify the source
  without re-querying manifest. `PluginShutdownError` 2 variants
  (`Timeout`, `Other`). `DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT =
  Duration::from_secs(5)` const used by Phase 81.5/81.10
  registry to wrap plugin shutdown calls in
  `tokio::time::timeout`. Compile-time dyn-safety guarantee via
  `static _OBJECT_SAFE_CHECK: OnceLock<Arc<dyn NexoPlugin>>` —
  if the trait gains an associated type or generic method that
  prevents `dyn NexoPlugin`, the build refuses. Re-exported in
  `crates/core/src/agent/mod.rs` as `NexoPlugin`,
  `PluginInitContext`, `PluginInitError`, `PluginShutdownError`,
  `DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT`. **Distinct from existing
  `crate::agent::plugin::Plugin`** (Channel I/O trait for
  browser/wa/tg/email): different file (`plugin_host.rs` vs
  `plugin.rs`), different trait name (`NexoPlugin` vs
  `Plugin`), different concept (boot-time lifecycle vs runtime
  channel I/O). `nexo-core` Cargo.toml gained two deps:
  `nexo-plugin-manifest` (workspace, for `PluginManifest` type)
  and `nexo-driver-permission` (for `AdvisorRegistry`; was
  previously dev-only). 8 inline tests cover trait dyn-safety
  compile-time check, manifest exposure, init outcome dispatch,
  shutdown default vs override, `tokio::time::timeout` wrap
  pattern, error Display actionable messages, path helpers,
  default timeout const value. Provider-agnostic by construction
  — trait + context have no LLM-specific surface; plugins build
  providers via `ctx.llm_registry.build(...)`. IRROMPIBLE refs in
  module doc-comment: claude-code-leak `src/tools/*` (absence —
  leak hardcodes every tool, no plugin trait); `research/src/
  plugins/runtime/types-channel.ts:56-71` (`register/dispose`
  pattern adapted via Rust Drop); `research/src/plugins/
  activation-context.ts:27-44` (`PluginActivationInputs`
  metadata pattern); internal precedent
  `crates/core/src/agent/plugin.rs` (Channel `Plugin` trait,
  distinct concept). Phase 81 sub-phase counter advances 1/13 →
  2/13. Future sub-phases consume this trait: 81.3 namespace
  runtime enforcement, 81.5 `PluginRegistry::discover` (critical
  path), 81.9 `Mode::Run` registry sweep replacing per-plugin
  boot wire. Tests:
  `cargo test -p nexo-core --lib agent::plugin_host` → 8/8,
  `cargo build -p nexo-core` verde.
- **Phase 81.1 — `nexo-plugin-manifest` crate** (foundation for
  plug-and-play plugin system). New crate
  `crates/plugin-manifest/` (~860 LOC + 25 tests verde) defines
  the TOML schema + 4-tier defensive validator that every
  native Rust nexo plugin must ship as `nexo-plugin.toml`.
  Schema covers 14 sub-sections (`[plugin]` core +
  `capabilities`/`tools`/`advisors`/`agents`/`channels`/`skills`/
  `config`/`requires`/`capability_gates`/`ui`/`contracts`/`meta`)
  + 3 enums (`Capability` 10 variants, `GateKind` 3, `GateRisk`
  4). Public API: `PluginManifest::{from_str, from_path,
  validate, id, version}`. Validator collects ALL errors (no
  bail-on-first) into `Vec<ManifestError>` so operators see the
  full diagnostic in one pass. Validation tiers: **syntactic**
  via `toml::from_str` + `#[serde(deny_unknown_fields)]`
  everywhere (rejects unknown fields forward-compat); **field-
  level** via id regex `^[a-z][a-z0-9_]{0,31}$`, semver
  parsing, path security (rejects `..` traversal + absolute
  paths); **cross-field** via tool namespace policy (every
  tool name MUST start with `<plugin.id>_`), `deferred ⊆
  expose`, capability impl confirmation, duplicate gate
  env_var detection; **runtime** via
  `min_nexo_version.matches(current_nexo_version)` so plugins
  built for future daemon versions reject cleanly.
  `ManifestError` enum 13 variants, all thiserror-typed,
  Display messages carry operator-actionable hints
  (e.g. *"Rename to `marketing_<descriptive>`"*). Reference
  manifest `examples/marketing-example.toml` documenta cada
  sección con comments per-block; loaded + validated by
  `example_marketing_manifest_validates` test as drift guard.
  Distinct from the existing `crates/extensions/<n>/plugin.toml`
  (Phase 11.1, subprocess tool extensions) — different filename
  (`nexo-plugin.toml`), different concept (native Rust plugins
  link into the daemon shipping full mini-applications: agents
  + tools + skills + channels + advisors + capability gates).
  Future sub-phases consume this foundation: 81.2 `NexoPlugin`
  trait + lifecycle, 81.5 `PluginRegistry` discovery, 81.9
  `Mode::Run` registry sweep replacing per-plugin boot wire.
  Operator UX for plugin authors: drop `nexo-plugin.toml` in
  the plugin's crate root with desired sections; future
  registry walks the workspace + user dir, parses manifests,
  validates, and wires plugins automatically — zero
  `src/main.rs` edits needed once 81.9 ships. Provider-agnostic
  by construction: schema makes no LLM-provider assumption;
  plugins declare requirements via `[plugin.requires]`. IRROMPIBLE
  refs: claude-code-leak `src/tools/*` (absence — leak hardcodes
  every tool, no plugin manifest concept); `research/src/plugins/
  manifest-types.ts:1-20` (`PluginConfigUiHint` mirrored as
  [`UiHint`]); `manifest.ts:17` (`PLUGIN_MANIFEST_FILENAME`
  precedent — OpenClaw uses `openclaw.plugin.json` JSON5; we
  use TOML idiomatic Rust); `manifest.ts:54-60`
  (`PluginManifestActivation` capability enum mirrored as
  [`Capability`]); `extensions/firecrawl/openclaw.plugin.json`
  (example with `providerAuthEnvVars`/`uiHints`/`contracts`);
  internal `crates/extensions/src/manifest.rs:108-132` (Phase
  11.1 distinct concept). Tests:
  `cargo test -p nexo-plugin-manifest --lib` → 25/25,
  `cargo build -p nexo-plugin-manifest` verde.
- Phase 80.12 (MVP) — generic webhook receiver primitives. New crate
  `crates/webhook-receiver/` (~700 LOC + 33 tests verde). Provider-
  agnostic by construction: zero GitHub-specific (or any other
  provider-specific) Rust code; new providers add a YAML config
  entry, no code change. `WebhookSourceConfig` carries 5 fields:
  `id` (stable identifier for log correlation + capability gate
  naming), `path` (HTTP path the listener exposes), `signature:
  SignatureSpec` (algorithm + header name + optional prefix +
  secret env var), `publish_to` (NATS subject template with
  `${event_kind}` substitution), `event_kind_from: EventKindSource`
  (header lookup or JSON body dotted path), optional
  `body_cap_bytes` (default 1 MB). YAML uses `serde(rename_all =
  "kebab-case")` for the algorithm enum and `tag = "kind"` for
  EventKindSource so operators write natural YAML
  (`algorithm: hmac-sha256`, `kind: header` / `kind: body`).
  Algorithms supported: `HmacSha256`, `HmacSha1`, `RawToken`
  (constant-time string compare for providers that just share a
  secret token rather than a real signature). All verifications
  use `subtle::ConstantTimeEq` to resist timing attacks; hex
  decoding is defensive (garbage hex → `InvalidSignature` without
  panic); per-spec prefix stripping (e.g. `"sha256="` removed
  before hex-decode). Pure-fn primitives exported:
  `verify_signature(spec, secret, header_value, body) ->
  Result<(), RejectReason>` does the constant-time HMAC compare
  or raw-token compare; `extract_event_kind(source, headers,
  body) -> Result<Option<String>, RejectReason>` does
  case-insensitive header lookup or JSON dotted-path body
  navigation via the private `json_get_dotted` helper (recursive
  `Value::get` walk, no unwraps); `render_publish_topic(template,
  event_kind) -> String` substitutes `${event_kind}` and is
  forward-compatible with future template variables.
  `WebhookHandler::handle(headers, body) -> Result<HandledEvent,
  RejectReason>` orchestrates 4 gates in order: (1) body cap
  rejects BEFORE any HMAC compute as DoS defense; (2) signature
  header presence + secret env presence + signature match; (3)
  event-kind extraction (header lookup or body JSON path); (4)
  subject-safety check — rejects event_kind values containing
  `.`, `*`, `>`, or whitespace which would break NATS subject
  parsing. Output is `HandledEvent { source_id, event_kind,
  topic: String, payload: serde_json::Value }`. Body normally
  parses as JSON; non-UTF-8 bodies wrap as `{ "raw_base64":
  "..." }` — operator still sees the content for post-mortem
  without assuming JSON. `RejectReason` is `thiserror`-typed
  with 7 variants: `OversizedBody`, `MissingSignatureHeader`,
  `InvalidSignature`, `SecretMissing`, `MissingEventKind`,
  `InvalidBodyJson`, `InvalidEventKindForSubject`. Caller maps
  to HTTP status (401 for signature errors, 413 for oversize,
  422 for missing event kind, 500 for secret-missing operator
  misconfig). `WebhookHandler::validate(&config)` runs boot-time
  invariants: id non-empty, path non-empty + starts with `/`,
  publish_to non-empty, signature.header non-empty,
  signature.secret_env non-empty, body_cap_bytes > 0 when set,
  event_kind_from header.name or body.path non-empty. Workspace
  `Cargo.toml::members` adds `crates/webhook-receiver`. 33 unit
  tests verde cover: 4 validate (well-formed + 3 rejection
  arms), 6 verify_signature (HMAC-SHA256 match/mismatch,
  HMAC-SHA1 match, RawToken match/mismatch, garbage hex), 5
  extract_event_kind (header case-insensitive, header missing
  → None, body top-level, body nested, body invalid JSON
  errors, body missing path → None), 2 render_publish_topic, 3
  is_event_kind_subject_safe (rejects dot / wildcards / whitespace
  / empty, accepts alphanumeric+dashes), 6 handle integration
  (oversized body, missing sig header, secret unset, invalid
  sig with secret set, happy path with NATS topic + JSON
  payload, event kind containing dot rejected by subject
  safety), 1 non-JSON body wrapping (raw_base64 fallback), 2
  YAML round-trip (full config with header extraction + body-
  path extraction shape). HTTP listener integration deferred
  to 80.12.b — the operator wires the route via the existing
  `:8080` health server (`read_http_path` / `write_http_response`
  helpers in `src/main.rs` already handle the raw TcpListener
  pattern) or extends with axum/hyper, depending on their
  preference. The crate stays HTTP-framework-agnostic so neither
  choice is forced. Provider-agnostic by construction: zero
  LLM-provider touchpoints, decision table data-driven via
  YAML, transversal across Anthropic / MiniMax / OpenAI /
  Gemini / DeepSeek / xAI / Mistral. **Wiring point** (operator
  opts in when ready): construct `let handler =
  WebhookHandler::new(config)` per source at boot; in the
  HTTP listener route handler, collect headers + body bytes
  and call `handler.handle(&headers, body)`; map
  `Result<HandledEvent, RejectReason>` to HTTP response (200
  for Ok, appropriate status for each error variant); on Ok,
  call `broker.publish(handled.topic, Event::new(handled.topic,
  handled.source_id, handled.payload)).await` so any
  downstream subscriber consumes the event. **Deferred
  follow-ups**: 80.12.b — HTTP listener integration routing
  `/webhooks/<source_id>` via the existing `:8080` health
  server or a dedicated dispatch port (reuse `read_http_path`
  / `write_http_response` helpers — no axum/hyper dep
  required); 80.12.c — tunnel registration for the public URL
  (pairs with `crates/tunnel/`); 80.12.d — INVENTORY entries
  for per-source secrets (`WEBHOOK_<SOURCE_ID>_SECRET`) so
  `nexo setup doctor capabilities` lists them; 80.12.e — audit
  log per request (Phase 72-style) so operators can replay;
  80.12.f — multi-source config validation at boot (reject
  duplicate paths or duplicate ids); 80.12.g — replay
  protection (idempotency tokens / nonce window per source);
  80.12.h — main.rs hookup (route + listener registration +
  per-source handler map). Three-pillar audit: **robusto** —
  33 tests cover every path; constant-time HMAC compare via
  `subtle::ConstantTimeEq`; body cap before HMAC compute as
  DoS defense; defensive hex decode + JSON parse never panic;
  structured `RejectReason` enum makes caller error mapping
  exhaustive; subject-safety check prevents NATS pollution;
  YAML validation at boot fails fast on operator typos;
  **óptimo** — pure-fn primitives are zero-alloc on the hot
  path; single HMAC state allocation per request; case-
  insensitive header lookup via ASCII lower-case comparison
  without per-call allocation; `Arc<WebhookSourceConfig>`
  shareable across handler clones; **transversal** — zero
  LLM-provider touchpoints, decision table fully data-driven
  via YAML, transversal Anthropic / MiniMax / OpenAI / Gemini
  / DeepSeek / xAI / Mistral.
- Phase 80.21 (MVP) — public docs + admin-ui tech-debt sweep for the
  full Phase 80 KAIROS-style cluster (assistant mode, auto-approve
  dial, AWAY_SUMMARY, multi-agent coordination, BG sessions). 5 new
  mdBook pages under `docs/src/`: `agents/assistant-mode.md` (~250
  LOC) covering the per-binding flag + addendum + boot-immutable
  semantics + cross-feature lifecycle hooks + per-sub-phase status
  table; `agents/auto-approve.md` (~280 LOC) covering the curated
  decision table + always-asks list + layered-gates ASCII diagram +
  YAML config + defense-in-depth checklist; `agents/away-summary.md`
  (~180 LOC) covering config table + output shape with truncation +
  wiring snippet using `try_compose_away_digest` + atomic-update
  pattern + defensive edges; `agents/multi-agent-coordination.md`
  (~250 LOC) covering the `agent.inbox.<goal_id>` subject contract +
  `InboxMessage` shape + `list_peers` / `send_to_peer` tool shapes +
  6 validation gates + per-goal fan-out + receive side router +
  buffer-on-demand semantics + render shape + wiring snippet;
  `cli/agent-bg.md` (~280 LOC) covering `SessionKind` enum table +
  `agent run [--bg]` + `agent ps` + `agent discover` + `agent
  attach` + 3-tier DB path resolution (`--db` > `NEXO_STATE_ROOT`
  > XDG default) + kind-aware reattach semantics. `docs/src/SUMMARY.md`
  reorganised with a new top-level **Assistant mode** section
  grouping the four concept pages plus an entry under **CLI** for
  `agent-bg.md`. `mdbook build docs` smoke verde — every cross-ref
  resolves, no broken links. Each page maintains the project's
  no-leak-attribution posture: zero mentions of upstream codenames
  or paths in committed text. Status tables per page list every
  sub-phase as ✅ MVP or ⬜ deferred so operators see exactly what
  ships vs what's blocked on follow-up wiring (main.rs hookups,
  caller-side metadata population, daemon-side BG-goal pickup).
  Six new tech-debt registry entries in `admin-ui/PHASES.md`
  defining the operator UI surface for each cluster feature: (1)
  assistant_mode toggle + addendum textarea + initial_team multi-
  select + boot-immutable "restart required" banner on Phase A3
  agent-config tab + Phase A4 active-badge per goal; (2) auto_approve
  per-binding toggle with workspace-path display + curated-tools
  preview + Phase A9 audit log + setup-doctor banner for
  `assistant_mode + !auto_approve` misconfig; (3) BG sessions
  Phase A4 dashboard tab with SessionKind chips + spawn-BG modal +
  discover-detached pane + per-row drill-in with live-stream
  placeholder; (4) AWAY_SUMMARY config block on Phase A3 + Phase
  A9 last-digest viewer + per-channel rendering preview; (5)
  multi-agent inbox pane on Phase A8 delegation visualiser with
  live buffer count + per-message preview + drain button + tool
  registry status + API reference for the subject contract; (6)
  AutoDream cluster pane on Phase A7 memory inspector with status
  badge + `dream_runs` audit table tail + force-run button gated
  by `NEXO_DREAM_NOW_ENABLED` capability + kill button. Three-pillar
  audit: **robusto** — cross-refs between every page so operators
  navigate by use-case not by sub-phase number; per-page status
  tables surface deferred follow-ups inline so operators don't
  have to dig PHASES.md to know what's wired vs not; defensive
  language ("operator hookup pending", "blocked on dirty-state",
  "until the daemon-side pickup lands") wherever the slim MVP
  stops short of end-to-end; **óptimo** — mdbook reuses the
  existing infrastructure (no new build steps, no new
  preprocessors); admin-ui tech-debt entries are one-liner-per-
  feature scope-bound, no over-specification of UI shape; **transversal**
  — zero LLM-provider mentions in docs (every example provider-
  agnostic); admin-ui entries describe operator knobs not LLM
  prompts so the UI surface stays provider-agnostic too. Cluster
  KAIROS-style docs now complete; Phase 80 progress 18/22 sub-phases
  with full operator-visible documentation for every shipped
  feature. Pending sub-phases (cron jitter 80.2-80.6 + brief
  mode 80.8 + MCP channels 80.9 + generic webhook receiver
  80.12) will land their own doc pages when shipped.
- Phase 80.11.b (MVP) — receive side for the agent inbox: router +
  per-goal FIFO buffer + render helper. Closes the Send→Receive
  loop opened by Phase 80.11 (publisher half). New module
  `crates/core/src/agent/inbox_router.rs` (~280 LOC + 17 tests
  verde). `InboxRouter<B: BrokerHandle + ?Sized + 'static>`
  mirrors the Phase 79.6 `TeamMessageRouter` pattern: single
  broker subscriber on `agent.inbox.>` wildcard pattern (one
  subscription per process, not per-goal — efficient under
  fan-out) + `dashmap::DashMap<GoalId, Arc<InboxBuffer>>` for
  in-memory routing. Spawn API:
  `pub fn spawn(self: &Arc<Self>, cancel: CancellationToken) ->
  JoinHandle<()>` runs the subscribe loop with
  `tokio::select! { _ = cancel.cancelled() => break, next =
  sub.next() => match next { Some(ev) => self.dispatch_inbound(ev),
  None => break } }`. `dispatch_inbound(ev)` parses the subject
  suffix as a UUID via the private `parse_goal_from_subject`
  helper (defensive: rejects unknown prefix or non-UUID suffix
  with a debug log instead of panicking), decodes the payload
  as `InboxMessage` (malformed → drop with debug log), looks up
  or creates the buffer via `dashmap::Entry::or_insert_with`,
  pushes the message. The "or creates" leg implements
  buffer-on-demand semantics: a peer can fire a message at a
  goal that hasn't `register()`'d yet, the message queues in a
  fresh buffer, and the goal sees it when it eventually
  registers — race-safe under fast-spawn-then-immediate-send.
  `InboxBuffer { queue: Mutex<VecDeque<InboxMessage>> }` with
  `pub const MAX_QUEUE: usize = 64`: `push(msg) -> bool` returns
  `true` when an eviction happened (FIFO `pop_front` when the
  queue is at cap, plus a `tracing::warn!` line so long-idle
  goals surface in logs); `drain() -> Vec<InboxMessage>`
  atomically empties the queue and returns its contents in
  chronological order (oldest first); `len()` / `is_empty()`
  for read tools. Mutex held only for the microsecond push /
  drain windows. Idempotent `register(goal_id) -> Arc<InboxBuffer>`
  returns the existing buffer when the goal already has one
  (goal resume safe — re-registration on restart preserves any
  buffered messages). `forget(goal_id)` removes the entry on
  goal terminal state. `buffer_count()` for ops audit. Pure-fn
  `pub fn render_peer_messages_block(messages: &[InboxMessage])
  -> Option<String>` returns `None` when the slice is empty so
  callers can `if let Some(block) = render(...)` inline; when
  non-empty, returns markdown:
  ```
  # PEER MESSAGES

  <peer-message from="researcher" sent_at="2026-04-30T12:00:00+00:00"[ correlation_id="<uuid>"]>
  body
  </peer-message>
  ```
  `correlation_id` attribute is rendered ONLY when `Some` —
  attribute is omitted when `None` to keep the prompt minimal.
  Cancellation is clean: dropping the supplied `CancellationToken`
  causes the subscribe loop to break and the spawned task to
  exit. Subscriber failures log a `tracing::warn!` and return
  early (mirrors `team_message_router::spawn` pattern — inbox
  offline preferable to a panic loop). 17 unit tests verde:
  `buffer_push_drain_round_trip` (basic FIFO),
  `buffer_drain_empty_returns_empty_vec`,
  `buffer_evicts_oldest_at_cap` (push 64+1 → eviction; oldest
  gone, newest present), `parse_goal_from_subject_valid`,
  `parse_goal_from_subject_rejects_unknown_prefix`
  (`team.broadcast.foo` → None), `parse_goal_from_subject_rejects_non_uuid_suffix`
  (`agent.inbox.not-a-uuid` → None), `render_empty_returns_none`,
  `render_single_message_includes_from_and_body` (asserts no
  `correlation_id` attribute when None),
  `render_with_correlation_id_includes_attribute`,
  `render_preserves_chronological_order` (3-message slice →
  body positions in output increasing),
  `router_register_idempotent_returns_same_buffer` (push via
  Arc clone, drain via another Arc clone, same buffer
  instance), `router_dispatch_inbound_pushes_to_buffer`
  (synthetic event → direct `dispatch_inbound` call),
  `router_buffer_on_demand_for_unregistered_goal` (send before
  register → register later → drain sees buffered message),
  `router_drops_malformed_subject` (no panic on garbage
  subject), `router_drops_malformed_payload` (garbage JSON
  body), `router_forget_drops_buffer`, and
  `router_spawn_subscribes_and_routes_end_to_end` (full
  end-to-end via `AnyBroker::local()` pubsub: spawn router →
  publish to `agent.inbox.<goal_id>` → sleep 100ms for the
  loop → drain buffer → assert message present → cancel).
  Re-export through `crates/core/src/agent/mod.rs` (`pub mod
  inbox_router;`). `cargo build --workspace` + `cargo test
  -p nexo-core --lib agent::inbox_router` all green. **Wiring
  point** for the operator (deferred 80.11.b.b/c sub-phases):
  (1) at boot, `let router = InboxRouter::new(broker.clone());
  let _handle = router.spawn(cancel.clone());` — single
  per-process spawn covers every goal; (2) at goal startup,
  `let buf = router.register(goal_id);` and stash on the
  goal's runtime context; (3) in the per-turn loop adjacent
  to Phase 80.15's assistant addendum push site,
  `let drained = buf.drain(); if let Some(block) =
  render_peer_messages_block(&drained) { channel_meta_parts.push(block); }`;
  (4) at goal terminal, `router.forget(goal_id);` releases the
  buffer. **Deferred follow-ups**: 80.11.b.b — hook the drain +
  render into `llm_behavior.rs` per-turn loop adjacent to
  Phase 80.15 assistant addendum push site (1-line snippet,
  blocked on dirty-state pattern); 80.11.b.c — main.rs router
  spawn + per-goal `register` / `forget` on goal lifecycle
  hooks. Three-pillar audit: **robusto** — 17 tests cover
  every branch + race scenarios (buffer-on-demand,
  re-registration, eviction, malformed input both at subject
  and payload layers, end-to-end pubsub); MAX_QUEUE cap with
  FIFO eviction prevents memory bloat for long-idle goals;
  cancellation token shutdown clean; subscriber-failure
  log-and-exit avoids panic loop; **óptimo** — single broker
  subscriber per process, dashmap O(1) lookup, per-buffer
  Mutex held only for microsecond windows, drain is a single
  allocation, queue starts at capacity 8 and grows by VecDeque
  doubling; **transversal** — pure NATS subject + JSON payload
  + in-memory VecDeque, zero LLM-provider touchpoints,
  transversal across Anthropic / MiniMax / OpenAI / Gemini /
  DeepSeek / xAI / Mistral.
- Phase 80.11 (MVP) — agent inbox subject contract +
  `list_peers` / `send_to_peer` LLM tools (publisher-only slim
  MVP). Multi-agent in-process coordination via per-goal NATS
  subject `agent.inbox.<goal_id>`. New module
  `crates/core/src/agent/inbox.rs` ships
  `inbox_subject(GoalId) -> String` helper rendering
  `agent.inbox.<uuid>`, `InboxMessage { from_agent_id,
  from_goal_id, to_agent_id, body, sent_at, correlation_id:
  Option<Uuid> }` with `Serialize + Deserialize` so the wire
  format is JSON over NATS uniformly across local broker +
  remote cluster, plus constants `INBOX_SUBJECT_PREFIX =
  "agent.inbox"` / `MIN_BODY_CHARS = 1` / `MAX_BODY_BYTES =
  64 * 1024`. `correlation_id` is skipped from the wire when
  None via `#[serde(skip_serializing_if = "Option::is_none")]`
  so request/response patterns can reuse the channel. New
  `ListPeersTool` (`crates/core/src/agent/list_peers_tool.rs`,
  ~80 LOC + 1 test) returns `{ peers: [{ agent_id, description,
  reachable }] }` JSON, excludes self, computes `reachable` per
  entry by glob-matching against
  `EffectiveBindingPolicy::allowed_delegates` (empty list ⇒ all
  reachable, trailing-`*` prefix match, exact otherwise — same
  semantics as Phase 16 + the existing
  `peer_directory::render_for` markdown path). `PeerDirectory`
  gains a `pub fn peers(&self) -> &[PeerSummary]` slice
  accessor (the underlying field was private; markdown rendering
  via `render_for` keeps the existing system-prompt path
  intact, the new accessor exposes the raw list to tools that
  return JSON). When `ctx.peers` is `None`, returns `{ peers:
  [], note: "this agent has no PeerDirectory configured" }`.
  New `SendToPeerTool { lookup: PeerGoalLookup }`
  (`crates/core/src/agent/send_to_peer_tool.rs`, ~280 LOC + 11
  tests) where `pub type PeerGoalLookup = Arc<dyn Fn(&str) ->
  Vec<GoalId> + Send + Sync + 'static>` is a caller-injected
  closure — operator wires it to `nexo_agent_registry::AgentRegistry::list`
  filtered by `agent_id` + Running status, keeping the tool free
  of an agent-registry dependency. Tool def declares
  `to: string`, `message: string` (1-65536 chars), optional
  `correlation_id: string` (UUID), `additionalProperties:
  false`, `required: ["to", "message"]`. Handler walks 6
  validation gates: (1) `to` present + non-empty after trim,
  (2) `to != ctx.agent_id` (self-sends rejected), (3) `message`
  present + non-empty, (4) body ≤ MAX_BODY_BYTES (oversize
  rejected with explicit limit in error message), (5) `to`
  must exist in `PeerDirectory` (fast-path "unknown agent_id"
  unreachable when not — fail-fast before broker round-trip),
  (6) `lookup(to)` must return at least one live goal id
  (empty → "no live goals" unreachable). When all 6 pass,
  iterates live goals, builds an `InboxMessage` per goal with
  `from_goal_id = ctx.session_id.map(GoalId).unwrap_or_else(GoalId::new)`
  (best-effort; provenance preserved via `from_agent_id`),
  publishes via `ctx.broker.publish(inbox_subject(goal),
  Event::new(...))`, accumulates `delivered_to: Vec<String>` and
  `unreachable_reasons: Vec<String>` so per-goal fan-out is
  fault-tolerant — one bad goal doesn't block others.
  Returns `{ delivered_to: [...], unreachable_reasons: [...] }`.
  11 send_to_peer tests cover every gate + happy path:
  `tool_def_shape`, `empty_to_errors`, `missing_to_errors`,
  `missing_message_errors`, `empty_message_errors`,
  `self_send_rejected`, `unknown_agent_id_returns_unreachable`,
  `no_live_goals_returns_unreachable`,
  `live_peer_publishes_and_returns_delivered`,
  `oversize_message_rejected` (body > 64 KB),
  `correlation_id_round_trips` (subscribes to the inbox subject,
  fires send with explicit `correlation_id`, asserts the wire
  payload deserialises to the same UUID — proves the
  Some-correlation-id path serialises and the receiver can
  recover it intact). Test fixture mirrors the
  `extension_tool::tests::test_ctx` pattern (full
  `AgentConfig` field set), uses `AnyBroker::local()` +
  `SessionManager::new(60s, 20)` so broker round-trip works
  in-memory without external NATS. 3 inbox tests
  (`subject_format_uses_prefix_dot_uuid`,
  `message_serde_round_trip`, `correlation_id_omitted_when_none`)
  + 1 list_peers test (`tool_def_shape`) round out the cluster
  → **15 new tests verde**. Re-exports plumbed through
  `crates/core/src/agent/mod.rs`: `inbox`, `list_peers_tool`,
  `send_to_peer_tool`. `cargo build --workspace` + `cargo test
  -p nexo-core --lib agent::inbox agent::list_peers_tool
  agent::send_to_peer_tool` all green. **Deferred follow-ups**:
  80.11.b — receive side: subscriber on `agent.inbox.<goal_id>`,
  per-goal FIFO buffer, injection as `<peer-message
  from="agent_id">...</peer-message>` system block at next turn
  start (requires runtime hook in the per-turn loop, similar to
  Phase 77.16 AskUserQuestion injection pattern; without it,
  sent messages publish to the broker but no subscriber consumes
  them — operator can wire a NATS consumer manually);
  80.11.c — broadcast `to: "*"` with cap (linear in team size,
  marked expensive); 80.11.d — cross-machine inbox via NATS
  cluster (works automatically with NATS, documents the
  operator's broker config requirement); 80.11.e — bridge
  protocol responses (`shutdown_request` /
  `plan_approval_request` JSON shapes — niche, defer);
  80.11.f — main.rs tool registration wire (1-line snippet
  wrapping `AgentRegistry::list_by_kind` as the lookup closure,
  blocked on the dirty-state pattern). Three-pillar audit:
  **robusto** — 15 tests cover every gate + happy path +
  correlation_id wire round-trip; defensive arg parsing
  (`as_str().trim()` + is_empty); broker publish failure
  handling per-goal via `unreachable_reasons` so partial
  failures don't cancel the whole call; PeerDirectory existence
  fast-path avoids broker round-trip on typos; race-safe — peer
  goal terminating between `list_peers` and `send_to_peer` falls
  through unreachable not panic; **óptimo** — `PeerDirectory`
  is cached at boot, lookup closure resolves in-memory via
  dashmap, broker publish is fire-and-forget, for-loop over
  live goals is O(N) trivially; **transversal** — zero
  LLM-provider touchpoints, pure JSON tool surface, NATS
  subject contract is provider-agnostic and works uniformly
  under Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI
  / Mistral.
- **M1.b.c — daemon-embed MCP HTTP server** (FOLLOWUPS A6.M1).
  `Mode::Run` (daemon) now optionally starts an MCP HTTP server
  in-process alongside the agent runtime, exposing the primary
  agent's tools — mirror of `nexo mcp-server` standalone behavior
  but inside the daemon process so operators don't need a second
  process. New `crates/config/src/types/mcp_server.rs::
  McpServerDaemonEmbedConfig { enabled: bool }` + `McpServerConfig
  .daemon_embed` field with `#[serde(default, deny_unknown_fields)]`
  — back-compat preserved (default false → no MCP server in
  daemon). `src/main.rs` boot wire just before
  `reload_coord.start(...)`: captures primary agent id+config
  pre-loop (since the loop consumes `cfg.agents.agents`), looks up
  the primary's `Arc<ToolRegistry>` from the existing
  `tools_per_agent` map, builds an `AgentContext` +
  `ToolRegistryBridge::new(...).with_list_changed_capability(true)`,
  validates `mcp_server.http.enabled` + bails with clear error on
  inconsistent config, calls `start_http_transport` to bring up
  the HTTP server, then registers a `ConfigReloadCoordinator`
  post-hook that re-reads `mcp_server.expose_tools` from disk,
  atomically swaps the bridge allowlist via
  `swap_allowlist(new)`, and emits
  `notify_tools_list_changed()` so connected Claude Desktop /
  Cursor clients refresh tool list automatically on every Phase
  18 reload — **no SIGHUP required, no daemon restart**. The
  `mcp_embed_handle` is drained on shutdown with a 5s timeout so
  SSE consumers see a clean disconnect.
  
  Sub-cleanup: `reload_expose_tools` helper (M1.b) refactored
  from `async fn` to `fn` since its body was synchronous
  (`AppConfig::load_for_mcp_server` is sync). The SIGHUP caller
  in `run_mcp_server` drops the `.await`. 3 existing helper
  tests adapted from `#[tokio::test]` to `#[test]`. New helper
  `compute_allowlist_from_mcp_server_cfg` derives the
  `ToolRegistryBridge` allowlist from an in-memory
  `McpServerConfig` (used by the daemon-embed boot wire where the
  config is already loaded; complements `reload_expose_tools`
  which re-reads the YAML on reload).
  
  3 new inline tests:
  `compute_allowlist_returns_set_from_expose_tools`,
  `compute_allowlist_returns_none_for_empty`,
  `compute_allowlist_dedupes_via_hashset`.
  
  **Operator UX** (single config block now drives both
  standalone and daemon-embed paths):
  ```yaml
  mcp_server:
    daemon_embed:
      enabled: true
    http:
      enabled: true
      bind: 127.0.0.1:8765
      auth:
        kind: static_token
        token_env: NEXO_MCP_TOKEN
    expose_tools: [Read, Edit, marketing_lead_route]
  ```
  Boot `nexo run`, MCP server live alongside agents on
  `127.0.0.1:8765`. Edit `mcp_server.yaml`, Phase 18 file watcher
  detects the change → reload-coord fires → post-hook swaps
  allowlist + emits notification → connected clients refresh.
  Zero downtime, zero SIGHUP, zero daemon restart.
  
  **Conflict path**: running `nexo` daemon with embed AND
  `nexo mcp-server` standalone with the same port fails on the
  second `bind` with `EADDRINUSE`; operator picks one path. The
  `mcp_server.*` config block is shared between both so there is
  no duplicate config to maintain.
  
  Open follow-ups: **M1.b.c.b** (per-agent endpoint
  `/mcp/agent_x` for multi-tenant routing across the daemon's N
  agents), **M1.b.c.c** (multi-agent union endpoint with
  tool-name collision detection), **M1.b.c.d** (hot-swap primary
  agent identity mid-run — today the bridge is held for the
  daemon's life; changing `agents[0]` requires a restart). Slices
  M1.c (stdio notification pump) and M1.b.b (cross-platform file
  watcher for `nexo mcp-server` standalone Windows path) remain
  open — the daemon-embed path on Linux/macOS now handles the
  cross-platform case automatically because Phase 18's file
  watcher is already cross-platform via the `notify` crate.
  
  IRROMPIBLE refs: `src/main.rs::run_mcp_server` (`:9173-9180`)
  is the architectural mirror — same primary-agent capture +
  bridge construction shape. claude-code-leak no relevant prior
  art (CLI single-process, no daemon mode); `research/` no
  relevant prior art (channel-side, no MCP server embedding).
  
  Provider-agnostic: protocol-MCP layer, no LLM-provider
  assumption — works under Anthropic / MiniMax / OpenAI / Gemini
  / DeepSeek / xAI / Mistral. The advisor pipeline (advisory_hook,
  shipped earlier today) composes seamlessly with the daemon-embed
  bridge — plugins can register their `ToolAdvisor` impl and the
  daemon's MCP server surfaces the advisory output to connected
  clients.
  
  Tests: `cargo test --bin nexo compute_allowlist` → 3/3,
  `cargo test --bin nexo reload_expose_tools` → 3/3,
  `cargo test -p nexo-config --lib` → 169/169,
  `cargo build --bin nexo` verde.
- Phase 80.14 (MVP) — AWAY_SUMMARY re-connection digest. Per-binding
  YAML opt-in. When the user sends a message after a configurable
  threshold of silence (default 4h), the runtime composes a short
  markdown digest summarising goals + aborts + failures recorded
  in the Phase 72 turn-log during the silence window and the
  operator-side handler delivers it before processing the user's
  message. Slim MVP is **template-based** — no LLM call — so the
  feature ships with zero per-fire token cost; the LLM-summarised
  variant lands as 80.14.b. New
  `crates/config/src/types/away_summary.rs` (~120 LOC + 6 unit
  tests) ships `AwaySummaryConfig { enabled: bool,
  threshold_hours: u64, max_events: usize }` with `#[serde(default)]`
  on every field, `Default` impl that returns disabled + 4h
  threshold + 50 max events, `validate()` that rejects
  `threshold_hours > 30 days` (likely operator confusion) and
  `max_events == 0` (would render an empty digest), and
  `threshold() -> Duration` convenience accessor.
  `AgentConfig.away_summary: Option<AwaySummaryConfig>` field
  with `#[serde(default)]` for backward-compat; bindings without
  the block keep current behaviour. Workspace fixture sweep
  applied `perl -i -pe 's/^(\s*)assistant_mode: None,$/$1assistant_mode:
  None,\n$1away_summary: None,/'` to 49+ struct literals across
  the same crates touched by Phase 80.10/80.15/80.17 sweeps.
  `nexo_agent_registry::TurnLogStore::tail_since(since: DateTime<Utc>,
  limit: usize) -> Result<Vec<TurnRecord>, ...>` new trait method
  with a default impl that returns `Vec::new()` (safety fallback
  for impls that haven't customised); `SqliteTurnLogStore`
  overrides with a real `WHERE recorded_at >= ?1 ORDER BY
  recorded_at DESC LIMIT ?2` query capped by `TAIL_HARD_CAP=1000`.
  New module `crates/dispatch-tools/src/away_summary.rs` (~280
  LOC + 11 tests verde) exposes `pub async fn
  try_compose_away_digest(cfg: &AwaySummaryConfig, last_seen:
  Option<DateTime<Utc>>, now: DateTime<Utc>, log: &dyn
  TurnLogStore) -> Result<Option<String>, AwaySummaryError>` —
  composition fn that walks 4 gates cheapest-first: (1)
  `cfg.enabled` (opt-in), (2) `last_seen.is_some()` (None
  bootstraps without firing — caller updates last_seen WITHOUT
  burning the threshold), (3) `(now - last_seen).to_std() >=
  cfg.threshold()` (negative elapsed from clock skew → no fire),
  (4) `log.tail_since(last_seen, max_events)` non-empty (empty
  digest is not worth sending). When all four pass, calls
  `build_digest(events, elapsed, max_events)` and returns
  `Some(markdown)`. Pure-fn `pub fn build_digest(events:
  &[TurnRecord], elapsed: Duration, max_events: usize) -> String`
  renders heading `**While you were away** (last <h>h<m>m):` plus
  bullet list of counters: `<N> goal turn(s) recorded` /
  `<N> completed` (matches `outcome.contains("completed") ||
  outcome == "done"`) / `<N> aborted/cancelled` (matches
  "aborted" or "cancelled") / `<N> failed` / `<N> in progress /
  other` (saturating subtraction), with truncation suffix
  `(showing the most recent <max>; older events may exist)` when
  `events.len() == max_events`. `AwaySummaryError` typed wrapper
  via `thiserror` over `AgentRegistryStoreError`. Re-exports from
  `nexo-dispatch-tools::lib.rs`: `try_compose_away_digest`,
  `build_digest`, `AwaySummaryError`. 11 unit tests in
  `away_summary::tests`: `disabled_returns_none` (cfg.enabled
  false → None even with events present), `last_seen_none_returns_none`
  (bootstrap path), `elapsed_below_threshold_returns_none` (2h
  with 4h threshold → None), `negative_elapsed_returns_none`
  (clock skew when `last_seen` is in the future relative to `now`
  → None), `empty_log_returns_none` (gates pass but log empty →
  None), `populated_log_returns_digest` (3 events: 2 done + 1
  failed → Some markdown contains expected counters),
  `digest_renders_completed_aborted_failed_counts` (mixed 6-event
  set covers all counter arms including "running" → "in progress
  / other"), `digest_caps_at_max_events` (50 events with cap=50
  → truncation suffix appears), `digest_below_cap_no_truncation_suffix`
  (10 events with cap=50 → no suffix), `digest_renders_minutes_correctly`
  (2h30m elapsed → string contains "2h30m"),
  `populated_log_truncates_to_max_events` (5 events with
  max_events=3 → digest renders 3 + truncation suffix). Mock
  `MockLog` impl `TurnLogStore` returns scripted records
  deterministically without SQLite. `cargo build --workspace`
  + `cargo test -p nexo-config --lib types::away_summary` (6
  verde) + `cargo test -p nexo-dispatch-tools --lib away_summary`
  (11 verde) all green. Provider-agnostic by construction —
  pure markdown template + SQLite filesystem; zero LLM-provider
  touchpoints; transversal across Anthropic / MiniMax / OpenAI /
  Gemini / DeepSeek / xAI / Mistral. Wiring point: operator's
  inbound handler invokes `try_compose_away_digest(...)` before
  processing the user's message; if `Some(digest)` returns,
  delivers via `notify_origin` then processes the inbound; in
  both cases atomically updates `last_seen_at = now` afterward
  (caller manages storage — this slim MVP doesn't couple to
  `nexo-pairing` or any specific table). **Deferred follow-ups**:
  80.14.b — LLM-summarised digest forks a subagent over the
  events list (richer 1-3 sentence prose vs today's bullet
  template); 80.14.c — `last_seen_at` tracking in
  `nexo-pairing::PairingStore` with a SQLite migration so
  operators don't roll their own; 80.14.d — per-channel-adapter
  rendering (whatsapp / telegram render markdown differently);
  80.14.e — time-of-day awareness ("don't ping at 3am unless
  awake_hours covers"); 80.14.f — custom prompt template per
  agent (relevant once 80.14.b ships); 80.14.g — main.rs inbound
  interceptor wire (1-line invocation site, blocked on dirty-state
  pattern). Three-pillar audit: **robusto** — 11 tests + 6
  config tests cover every gate path + defensive edges
  (negative elapsed / empty log / disabled / bootstrap / cap
  truncation); fail-safe trait default impl returns empty for
  uncustomised stores; UTC throughout to avoid TZ confusion;
  **óptimo** — pure-fn template render with zero LLM call;
  SQLite query bounded by `WHERE recorded_at >= ? LIMIT ?`
  (indexed) and capped by `TAIL_HARD_CAP=1000`; mock-based tests
  avoid SQLite spin-up; **transversal** — zero LLM-provider
  touchpoints, pure markdown text, delivery via existing
  `notify_origin` is provider-agnostic.
- **advisory_hook — generic tool advisory extension point**
  (FOLLOWUPS A6). Generalizes the bash-only
  `gather_bash_warnings` pipeline (Phase 77.8-10 + C4.a-b) into
  an extensible registry that any plugin (marketing / payment /
  CRM / etc.) can hook into without touching
  `nexo-driver-permission`. New module
  `crates/driver-permission/src/advisor.rs`:
  - `pub trait ToolAdvisor { fn id(&self) -> &str; fn advise(&self,
    tool_name: &str, input: &Value) -> Option<String>; }` —
    `Send + Sync + 'static` — sync trait so it stays dyn-safe
    without `async-trait`. Implementations should be cheap
    (heavy work behind an internal cache or async follow-up).
  - `pub struct AdvisorRegistry` (Vec-backed, ordered, Default)
    with `new()` (empty), `with_default()` (pre-registers
    `BashSecurityAdvisor`), `register(Arc<dyn ToolAdvisor>)`,
    and `gather(tool_name, input) -> Option<String>`. The
    `gather` method runs every advisor with
    `std::panic::catch_unwind(AssertUnwindSafe(...))` isolation
    — a panicking advisor logs `tracing::warn!` and is skipped;
    other advisors run unaffected. Multi-line advisor output is
    split on `\n` and each non-empty line gets its own
    `[<id>]` bracket prefix.
  - `pub struct BashSecurityAdvisor` wraps the existing
    `crate::mcp::gather_bash_warnings` (now `pub(crate)`) and
    strips the legacy `WARNING — bash security:\n- ` prefix so
    the registry can re-wrap with the unified header. Multi-tier
    bash output is preserved — each tier line gets its own
    `[bash]` prefix in the unified block.
  
  `PermissionMcpServer` (`crates/driver-permission/src/mcp.rs`)
  gains an `advisors: Arc<AdvisorRegistry>` field initialized to
  `AdvisorRegistry::with_default()` in `new()` so the back-compat
  default is "bash advisor fires" — operators existing pre-this
  slice see no behavior loss at the call-shape level. New builder
  `pub fn with_advisors(self, Arc<AdvisorRegistry>) -> Self` lets
  plugin-aware boot wire pass a registry with extra advisors
  registered. Wire site at `call_tool` swaps the previous
  `gather_bash_warnings(&tool_name, &original_input)` direct call
  for `self.advisors.gather(&tool_name, &original_input)`.
  
  **Output prefix change**: was
  `WARNING — bash security:\n- <tier line>\n- <tier line>`,
  now `WARNING — tool advisories:\n- [bash] <tier line>\n- [bash]
  <tier line>` (multiple advisors interleave by registration
  order). Operator dashboards or log parsers that match the
  exact old string need updating — the unified format is more
  consumable for LLM context but is a textual breaking change.
  All other behaviors (advisory-only, no block, decider
  authoritative) are unchanged.
  
  6 inline tests in `advisor::tests`:
  `advisor_registry_empty_returns_none`,
  `advisor_registry_single_includes_id_prefix`,
  `advisor_registry_multiple_joins_lines`,
  `advisor_registry_skips_silent_advisors`,
  `advisor_registry_isolates_panicking_advisor`,
  `bash_security_advisor_strips_legacy_prefix`.
  
  Plugin author surface (informational example —
  `nexo-plugin-marketing` ships its own concrete advisor when
  constructed):
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
  
  let mut registry = AdvisorRegistry::with_default();
  registry.register(Arc::new(MarketingAdvisor));
  let server = PermissionMcpServer::new(decider).with_advisors(Arc::new(registry));
  ```
  
  Provider-agnostic: advisors operate on `(tool_name, input)`,
  no LLM-provider assumption — works under Anthropic / MiniMax
  / OpenAI / Gemini / DeepSeek / xAI / Mistral. All advisories
  remain advisory-only; the upstream LLM decider is the
  authoritative allow/deny gate. Plugins that want hard
  blocks integrate with `nexo-core::plan_mode::MUTATING_TOOLS`
  (existing surface).
  
  IRROMPIBLE refs: claude-code-leak
  `src/tools/BashTool/bashSecurity.ts` (single-tier-class
  pattern this generalizes — leak hardcodes bash; the registry
  composes the bash advisor with arbitrary plugin advisors).
  `research/` no relevant prior art (channel-side scope, no
  permission advisory layer concept).
  
  Open follow-ups: `advisory_hook.b` (async `ToolAdvisor`
  variant for DB/network lookups), `advisory_hook.c` (per-binding
  advisor allowlist/disable granularity), `advisory_hook.d`
  (Prometheus metrics `nexo_advisor_runs_total`).
  
  Tests: `cargo test -p nexo-driver-permission --lib`
  → 170/170 (164 pre-existing + 6 new).
- Phase 80.17.b (MVP) — `AutoApproveDecider<D>` decorator that hooks
  the curated auto-approve dial (Phase 80.17) into the existing
  `PermissionDecider` chain. Decorator wraps any inner decider,
  reads `auto_approve: bool` + `workspace_path: String` from the
  request's `metadata: serde_json::Map` (defensive parsing —
  missing fields, wrong-type values, non-canonicalisable paths
  all collapse to `false` → delegate to inner). When
  `is_curated_auto_approve(tool_name, input, on, ws)` returns
  `true`, short-circuits to `PermissionOutcome::AllowOnce
  { updated_input: None }` with rationale
  `"auto_approve: curated subset (<tool_name>)"`. When `false`,
  delegates to the inner decider with the original request
  unchanged. Public constants exported for caller-side metadata
  population: `META_AUTO_APPROVE = "auto_approve"`,
  `META_WORKSPACE_PATH = "workspace_path"`. `AutoApproveDecider::new(
  inner: Arc<D>)` accepts any `Arc<D: PermissionDecider + ?Sized>`
  so existing decorators (rate limiter, audit log, etc.) compose
  freely. Six decorator tests in `auto_approve::decorator_tests`:
  `delegates_when_metadata_missing` (no `auto_approve` field →
  inner DenyAll fires Deny), `delegates_when_flag_false`
  (`auto_approve: false` → delegates),
  `short_circuits_for_curated_tool` (`auto_approve: true` +
  FileRead → AllowOnce, inner DenyAll never invoked),
  `delegates_for_destructive_bash` (`auto_approve: true` +
  `Bash rm -rf` → helper rejects → inner AllowAll fires AllowOnce
  with its own rationale, proving the path went through inner),
  `delegates_for_unknown_tool` (default-ask for new tool names),
  `handles_string_in_bool_field_defensively` (`"true"` string →
  `as_bool()` returns None → flag treated as false → delegate).
  Re-exports from `nexo-driver-permission::lib.rs`:
  `AutoApproveDecider`, `META_AUTO_APPROVE`, `META_WORKSPACE_PATH`,
  `is_curated_auto_approve`. Module total: 33 unit tests verde
  (27 from Phase 80.17 inventory + 6 new decorator tests).
  `cargo build --workspace` + `cargo test -p nexo-driver-permission
  --lib auto_approve` both verde post-change. Doc-comment on the
  decorator includes the boot-time wiring snippet
  (`let decider = AutoApproveDecider::new(inner)`) plus an example
  of caller-side `metadata` population from the resolved
  `EffectiveBindingPolicy`. Zero changes to `PermissionRequest`
  shape, zero changes to existing decider implementations,
  zero changes to `mcp.rs` / `socket.rs` / `permission_mcp` bin
  — operator opts in by wrapping their existing decider at boot.
  **Deferred follow-ups**: 80.17.b.b — main.rs wire wrapping the
  active decider with `AutoApproveDecider::new(...)` (1-line
  snippet, blocked on the same dirty-state pattern as
  Phase 80.1.b.b.b / 80.1.c / 80.10 / 80.15 / 80.16); 80.17.b.c —
  caller-side metadata population: the wire that constructs
  `PermissionRequest` (in `crates/driver-claude/` or the adapter
  layer) must insert `metadata.auto_approve` and
  `metadata.workspace_path` from the resolved
  `EffectiveBindingPolicy` before invoking the decider. Without
  the population step, the flag always reads `false` from
  metadata and the decorator becomes a transparent pass-through
  — wired up but inert until 80.17.b.c. Three-pillar audit:
  **robusto** — 6 decorator tests + 27 inventory tests = 33
  total cover every match arm + defensive parsing + delegation
  semantics; rationale string carries tool_name for audit trail;
  inner-decider behaviour preserved when flag off; **óptimo** —
  zero allocations on the hot path when flag off (just delegate);
  single helper invocation; metadata reads are trivial JSON
  access via existing serde APIs; **transversal** — decorator
  is generic over any `PermissionDecider` impl, zero
  LLM-provider touchpoints, transversal Anthropic / MiniMax /
  OpenAI / Gemini / DeepSeek / xAI / Mistral.
- Phase 80.17 (MVP) — `auto_approve` mode (curated auto-approve dial
  for the proactive-agent workflow). Operator opt-in via per-binding
  YAML flag. New module `crates/driver-permission/src/auto_approve.rs`
  (~280 LOC + 27 tests) exposes `is_curated_auto_approve(tool_name,
  args, auto_approve_on, workspace_path) -> bool` decision table:
  read-only / info-gathering tools always auto when the dial is on
  (FileRead, Glob, Grep, LSP, list_agents, agent_status, WebSearch,
  list_peers, task_get, etc.); Bash conditional on
  `is_read_only ∧ !destructive ∧ !sed_in_place` (Phase 77.8/77.9
  vetoes ALWAYS apply); FileEdit/Write conditional on canonical
  path under `workspace_path` with symlink-escape defense + parent-
  canonicalize fallback for new files; notifications + memory +
  multi-agent coordination always auto (notify_origin/push/channel,
  dream_now, delegate, team_create, task_create); ConfigTool / REPL
  / remote_trigger / schedule_cron NEVER auto; mcp_/ext_ prefix
  default-ask; default arm `_ => false`. `AgentConfig.auto_approve:
  bool` and `InboundBinding.auto_approve: Option<bool>` with
  `#[serde(default)]`. `EffectiveBindingPolicy` gains
  `auto_approve: bool` (resolved via override > agent default) and
  `workspace_path: Option<PathBuf>` (derived from `agent.workspace`).
  Workspace fixture sweep: 49 `AgentConfig` literals + 14
  `InboundBinding` literals via 2 perl multi-line replaces. 27
  unit tests verde covering every match arm + defensive edges
  (disabled flag, missing args, bash with destructive in pipe,
  symlink escape, new-file canonicalize). `cargo build --workspace`
  + `cargo test --bin nexo` verde. **Deferred follow-ups**:
  80.17.b hook the helper into the approval gate at
  `crates/driver-permission/src/mcp.rs` decision site (today the
  fn ships standalone, gate hookup pending); 80.17.c `nexo setup
  doctor` warn for `assistant_mode + !auto_approve` misconfig;
  80.17.d audit log on AutoAllow path (Phase 72 turn-log); 80.17.e
  CLI `--no-auto-approve` runtime override; 80.17.f customisable
  allowlist via YAML. Three-pillar audit: **robusto** — 27 tests,
  default-deny for new tools, Phase 77.8/77.9 vetoes preserved,
  symlink-escape defense, per-binding override; **óptimo** — pure
  fn, single match, zero hot-path allocs, reuses existing
  classifiers; **transversal** — zero LLM-provider touchpoints,
  works under Anthropic / MiniMax / OpenAI / Gemini / DeepSeek /
  xAI / Mistral.
- **M1.b — SIGHUP reload trigger for `nexo mcp-server`
  `expose_tools`** (FOLLOWUPS A6.M1.b). M1.a shipped the
  capability + ArcSwap allowlist surface but no caller hot-swapped
  it; adding/removing tools from `mcp_server.expose_tools` required
  a daemon restart for connected Claude Desktop / Cursor clients
  to see the change. Fix wires a SIGHUP-driven reload trigger
  inside the standalone `nexo mcp-server` subcommand. New
  `nexo-mcp` public surface: `pub struct HttpNotifyHandle`
  (`#[derive(Clone)]`) returned by
  `HttpServerHandle::notifier(&self)` — a lightweight clone-able
  notifier detached from the `JoinHandle` so it can be moved into
  long-lived background tasks safely.
  `HttpNotifyHandle::notify_tools_list_changed()` mirrors the
  existing handle method. New `src/main.rs::reload_expose_tools(config_dir)
  -> Result<Option<HashSet<String>>>` helper: re-reads
  `mcp_server.expose_tools` via `AppConfig::load_for_mcp_server`;
  empty list → `Ok(None)` (no filter, expose all non-proxy
  tools), non-empty → `Ok(Some(set))`, parse / IO error → `Err`
  (caller absorbs and keeps the previous last-known-good
  allowlist). `run_mcp_server` gained a `#[cfg(unix)]` SIGHUP
  handler tokio task that loops on
  `tokio::signal::unix::SignalKind::hangup()` selected against
  `shutdown.cancelled()` for clean exit. On every SIGHUP: log
  receive → re-read YAML → atomic swap-then-notify
  (`bridge.swap_allowlist(new)` first, then
  `notifier.notify_tools_list_changed()` second — reverse order
  races) → log success with sessions reached + new tool count.
  The bridge is `Clone` (M1.a) and shares the inner
  `Arc<ArcSwap>` between stdio + HTTP clones, so a single swap
  is observable across both transports atomically. Non-Unix
  build path logs warn-once and skips the handler — Windows
  operators restart for `expose_tools` changes (defer
  cross-platform file watcher to slice M1.b.b). Burst SIGHUPs
  yield multiple swaps + multiple notifications; clients
  debounce within the existing 200 ms session window per leak
  `useManageMCPConnections.ts:721-723`. Operator UX:
  `kill -HUP $(pidof nexo)` after editing `mcp_server.yaml` —
  connected clients refresh tool list without reconnect. 3
  inline tests in `src/main.rs::tests`:
  `reload_expose_tools_returns_set_from_yaml`,
  `reload_expose_tools_returns_none_for_empty_list`,
  `reload_expose_tools_propagates_yaml_parse_errors`.
  Doc-comment cites IRROMPIBLE refs to claude-code-leak
  `useManageMCPConnections.ts:618-665` (consumer-side handler
  registration) and `:721-723` (debounce). `research/` carries
  no relevant prior art (channel-side scope, no MCP server
  hot-reload concept). Slice **M1.b.b** (file watcher +
  `ConfigReloadCoordinator` integration when daemon `Mode::Run`
  exposes the bridge in-process) and slice **M1.c** (stdio
  notification pump for stdio-mode clients) remain open in
  FOLLOWUPS A6.M1. Provider-agnostic: protocol-MCP layer, no
  LLM-provider assumption — works under Anthropic / MiniMax /
  OpenAI / Gemini / DeepSeek / xAI / Mistral. Tests:
  `cargo test --bin nexo reload_expose_tools` → 3/3,
  `cargo build --bin nexo` verde, `cargo build -p nexo-mcp` verde.
- Phase 80.16 (MVP) — `nexo agent attach <goal_id>` + `nexo agent
  discover` operator CLI for the BG-sessions / KAIROS workflow.
  DB-only viewer slim MVP; live NATS event streaming + user input
  piping deferred as named follow-ups (80.16.b / 80.16.c /
  80.16.d). Adds `Mode::AgentAttach { goal_id, db, json }` +
  `Mode::AgentDiscover { include_interactive, db, json }` enum
  variants + 2 parser arms (the attach arm matches `[cmd, sub,
  goal_id]` slice with UUID in the trailing positional; discover
  matches bare `[cmd, sub]` and reads the optional flag from the
  positional list) + 2 dispatch arms + 2 async run fns
  (`run_agent_attach`, `run_agent_discover`, ~150 LOC together).
  **`run_agent_attach`** validates the UUID upfront via
  `Uuid::parse_str` (clean exit-1 error "is not a valid UUID"),
  resolves the DB path through the existing
  `resolve_agent_db_path` helper (3-tier `--db` > `NEXO_STATE_ROOT`
  env > XDG default — same as 80.1.d / 80.10), bails when the DB
  file is absent ("agent_handles DB not found at ..."), opens the
  store, fetches the handle via `store.get(GoalId(uuid))`, errors
  "no agent handle found for `<uuid>`" with anyhow context when
  the row doesn't exist. Markdown render covers all relevant
  fields: full goal_id / kind (via `as_db_str`) / `Debug` of
  status / phase_id / started_at / optional finished_at /
  optional last_progress_text in a code block / optional
  last_diff_stat in a fenced block / `turn_index/max_turns` /
  last_event_at. Final hint is status-aware: when status ==
  `Running`, prints "Live event stream requires daemon connection
  — re-run with NATS available (Phase 80.16.b follow-up)" so the
  operator knows the next step; when terminal (`is_terminal()`),
  prints "Goal is in terminal state {status:?}; no further
  updates expected" so post-mortem inspection has the right
  framing. `--json` path serialises the entire `AgentHandle` via
  `serde_json::to_string_pretty` (works because Phase 80.10 added
  `Serialize + Deserialize` to `SessionKind` and the field has
  `#[serde(default)]`). **`run_agent_discover`** accepts an
  `--include-interactive` flag that broadens the kinds-filter
  from default `[Bg, Daemon, DaemonWorker]` to all four variants
  including `Interactive` — the default answers the operator's
  "what is running detached?" question, and the broadened mode
  is the diagnostic alternative. Iterates kinds via
  `store.list_by_kind`, applies `retain(|h| h.status == Running)`,
  sorts `started_at` descending. Empty result emits a friendly
  message with conditional hint ("(no detached / daemon goals
  running; pass --include-interactive to broaden)" when default;
  no hint when broadened). Renders markdown table with cols:
  short-uuid (8 chars) / kind / phase_id / started_at /
  last_event_at — operator gets activity freshness at a glance.
  Missing-DB path mirrors `agent ps` UX: friendly stdout message
  + exit 0, JSON variant returns `[]`. 8 new inline tests in
  `src/main.rs::tests`:
  `run_agent_attach_rejects_invalid_uuid` (bad shape → anyhow
  context "valid UUID"),
  `run_agent_attach_missing_db_errors` (`--db /missing` → "not
  found"),
  `run_agent_attach_handle_not_found_errors` (valid UUID but row
  absent → "no agent handle found"),
  `run_agent_attach_running_renders_snapshot` (seeds Running Bg
  via `run_agent_run`, then exercises both markdown and `--json`
  render paths),
  `run_agent_discover_filters_to_bg_daemon` (seeds 1 Interactive
  + 1 Bg, asserts the underlying store query semantics + runs
  the fn for code-path coverage),
  `run_agent_discover_include_interactive_returns_all` (flag set,
  both markdown and JSON paths run),
  `run_agent_discover_empty_db_friendly_message` (DB missing,
  both render paths exit 0),
  `run_agent_discover_no_matching_goals_renders_friendly` (only
  Interactive seeded, default discover hits the no-match
  friendly branch). All 8 verde alongside the existing 13 from
  Phase 80.10 (= 21 total cli tests in `src/main.rs::tests`).
  CLI smoke confirmed manually: `NEXO_STATE_ROOT=... nexo agent
  discover` → markdown table when 1+ Bg row present;
  `nexo agent discover --include-interactive` → broader table;
  `nexo agent attach <real-uuid>` → markdown render + Running
  hint; `nexo agent attach not-a-uuid` → exit 1 "is not a valid
  UUID"; `nexo agent attach <fake-uuid>` → exit 1 "no agent
  handle found". `cargo build --bin nexo` + `cargo build
  --workspace` both verde post-change. Imports trimmed of
  unused `AgentRegistryStore` from `run_agent_discover` (only
  the inherent `list_by_kind` is called, not the trait method).
  Provider-agnostic by construction — pure SQLite + CLI; cero
  LLM-provider touchpoints; works under any `Arc<dyn LlmClient>`
  impl. **Deferred follow-ups**:
  - **80.16.b** — Live event streaming via NATS subscribe
    (`agent.registry.snapshot.<goal_id>` + `agent.driver.>`
    filtered by goal_id payload). Requires `nexo-broker` connect
    from CLI side; events stream to stdout; Ctrl-C detaches
    without killing the goal. Phase 67's existing per-goal
    snapshot subject is the natural feed.
  - **80.16.c** — User input piping via `agent.inbox.<goal_id>`
    subject (depends on Phase 80.11 — the inbox subject contract
    + `ListPeers` / `SendToPeer` LLM tools).
  - **80.16.d** — Interactive REPL UI / TUI for attach. Plain
    stdout printing covers the MVP today; richer terminal
    rendering comes when there's demand.
  Three-pillar audit: **robusto** — 8 tests covering UUID
  parse / DB absent / handle absent / Running render / terminal
  render / discover empty / discover no-match / both --json
  paths; defensive flag composition; sort newest-first invariant
  on discover output; **óptimo** — reuses 80.10 store helpers
  (`list_by_kind` + `get`) and shared CLI utilities
  (`resolve_agent_db_path` + `short_uuid`); pure RO pool; zero
  new infrastructure; **transversal** — pure SQLite + CLI; no
  LLM-specific phrasing; transversal across Anthropic /
  MiniMax / OpenAI / Gemini / DeepSeek / xAI / Mistral.
- Phase 80.10 (MVP) — `SessionKind` provenance enum + `nexo agent run`
  / `nexo agent ps` operator CLI. New `pub enum SessionKind` in
  `crates/agent-registry/src/types.rs` with 4 variants —
  `Interactive` (default; user-driven REPL turn or chat-channel
  inbound), `Bg` (operator-detached goal via `nexo agent run --bg`),
  `Daemon` (persistent supervised goal — assistant_mode binding's
  always-on agent loop), `DaemonWorker` (sub-agent spawned BY a
  Daemon). Helpers: `as_db_str` / `from_db_str` (typed decode error
  for unknown values so hand-edited DBs surface a clean message),
  `survives_restart` returns `true` for Bg / Daemon / DaemonWorker
  (these keep `Running` across daemon restart) and `false` for
  Interactive (flips to `LostOnRestart`). `AgentHandle` gains
  `pub kind: SessionKind` field with `#[serde(default)]` so rows
  persisted before 80.10 deserialise as `Interactive` automatically.
  Schema migration v5 ships via the existing
  `add_column_if_missing` helper at `migrate()` —
  `kind TEXT NOT NULL DEFAULT 'interactive'` is idempotent (the
  helper swallows "duplicate column" errors so re-opening the DB
  is safe). New index `idx_agent_registry_kind` for
  `list_by_kind` queries. UPSERT extended with bind 11
  (`kind = excluded.kind`); `row_to_handle` reads the column as
  source-of-truth (column wins over the JSON blob copy — same
  pattern Phase 79.1 plan_mode uses). New helpers on
  `SqliteAgentRegistryStore`: `list_by_kind(SessionKind)` for the
  CLI ps filter, and `reattach_running_kind_aware()` which flips
  Running → LostOnRestart **only** for `kind = 'interactive'` —
  Bg / Daemon / DaemonWorker rows keep Running because the
  operator expects them to survive across daemon restarts.
  Workspace fixture sweep applied
  `perl -i -pe 's/^(\s*)plan_mode: None,$/$1plan_mode: None,\n$1kind:
  nexo_agent_registry::SessionKind::Interactive,/'` to 14+ struct
  literals across `crates/{agent-registry,core,dispatch-tools}`
  (registry / dispatch_handlers / dispatch_followup / program_phase
  / shutdown_drain / admin / agent_control / event_forwarder
  + 4 test files); manually fixed `plan_mode_persists_across_restart.rs`
  test helper. CLI surface in `src/main.rs`:
  `Mode::AgentRun { prompt: String, bg: bool, db: Option<PathBuf>,
  json: bool }` + `Mode::AgentPs { kind: Option<String>, all: bool,
  db: Option<PathBuf>, json: bool }` + 2 parser arms (the `agent run`
  arm uses `[cmd, sub, ..]` slice pattern + filters `--flag` tokens
  out so operators can pass spaces without quoting:
  `nexo agent run --bg ship the release`) + 2 dispatch arms +
  helper `resolve_agent_db_path` mirroring Phase 80.1.d's 3-tier
  resolution (`--db` explicit > `NEXO_STATE_ROOT` env >
  `dirs::data_local_dir() / "nexo/state/agent_handles.db"`).
  **`run_agent_run`** validates prompt non-empty, opens the store
  RW (creates parent dir + DB on first call), inserts a new
  `AgentHandle { goal_id: Uuid::new_v4(), phase_id: "cli-bg" |
  "cli-run", status: Running, snapshot: default(), plan_mode: None,
  kind: if bg { Bg } else { Interactive } }`, prints the goal_id +
  detach hint pointing at Phase 80.16 attach. **`run_agent_ps`**
  opens the store RO (returns friendly "(no agent runs recorded
  yet — db not found at ...)" message + exit 0 when DB absent —
  same UX pattern as Phase 80.1.d dream tail), dispatches to
  `list_by_kind(parsed_kind)` when `--kind=...` is supplied or
  `list()` otherwise, applies `Running` filter unless `--all`,
  renders markdown table or JSON. 13 new tests verde:
  nexo-agent-registry 8 (`session_kind_default_is_interactive` /
  `session_kind_db_round_trip_all_variants` /
  `session_kind_from_db_str_rejects_unknown` /
  `session_kind_survives_restart_only_for_bg_daemon` /
  `agent_handle_serde_default_kind` field-strip round-trip /
  `store_insert_with_kind_round_trips` /
  `list_by_kind_filters_correctly` seeds 3 kinds + asserts filter
  returns 1 / `reattach_running_kind_aware_keeps_bg` seeds Running
  Bg + Running Interactive then asserts only Interactive flips to
  LostOnRestart) + nexo-rs bin 5
  (`resolve_agent_db_path_override_wins` /
  `resolve_agent_db_path_uses_env_when_no_override` /
  `run_agent_run_rejects_empty_prompt` /
  `run_agent_run_bg_inserts_handle_with_kind_bg` /
  `run_agent_run_no_bg_inserts_handle_with_kind_interactive` /
  `run_agent_ps_empty_db_friendly_message` /
  `run_agent_ps_filters_by_kind` /
  `run_agent_ps_rejects_invalid_kind`). CLI smoke confirmed
  manually: bare `nexo agent ps` against missing DB →
  "(no agent runs recorded yet)" exit 0; `nexo agent run --bg
  "test goal here"` → goal_id printed + queued status; `nexo agent
  ps` after → 1 row Running/bg; `nexo agent ps --kind=interactive`
  → "(no rows match)". Provider-agnostic by construction — pure
  SQLite + CLI; zero LLM-provider touchpoints; works under any
  `Arc<dyn LlmClient>` impl. **Deferred follow-ups** (each split
  out as a named sub-phase for clarity): 80.10.b — `nexo agent
  attach <goal_id>` TTY re-attach (= Phase 80.16); 80.10.c —
  daemon supervisor process for `Daemon` / `DaemonWorker` kinds
  (separate process lifecycle distinct from the interactive
  daemon); 80.10.d — `nexo agent kill <goal_id>` graceful abort
  signal; 80.10.e — `nexo agent logs <goal_id>` re-stream goal
  output without attaching; 80.10.f — Phase 77.17
  schema-migration system integration (versioned `user_version`
  bump for the new `kind` column); 80.10.g — daemon-side pickup
  of queued goals (today the CLI inserts the row but no daemon
  worker consumes it automatically; rows sit `Running` until
  manually transitioned via attach + future supervisor or
  explicit `agent dream kill`-style admin commands). Three-pillar
  audit: **robusto** — 13+ tests, migration idempotent, default
  `Interactive` keeps fixtures + Phase 71 backward-compat,
  reattach kind-aware preserves expected semantics, ps gracefully
  handles missing DB; **óptimo** — single new column, no separate
  table, reuses existing list / upsert paths, ps RO pool, spawn
  zero overhead vs current; **transversal** — provider-agnostic
  SQLite + CLI, no LLM-specific phrasing, transversal across
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
  Mistral.
- **M4.a.b — `extract_memories` schema field + `LlmClientAdapter` +
  per-agent boot wire** (FOLLOWUPS A6.M4). The trait + post-turn
  hook shipped in M4.a, but no operator-facing way to enable it
  existed: `AgentConfig` had no field, no adapter wrapped
  `Arc<dyn LlmClient>` into the narrow `ExtractMemoriesLlm`
  surface, and `src/main.rs` did not construct the extractor.
  Fix introduces all three.
  - **Schema**: `crates/config/src/types/agents.rs` gains
    `extract_memories: Option<ExtractMemoriesYamlConfig>` with
    `#[serde(default)]`. The YAML struct is a 1:1 mirror of
    `nexo_driver_types::ExtractMemoriesConfig` (4 fields:
    `enabled`, `turns_throttle`, `max_turns`,
    `max_consecutive_failures`) — wire-shape duplication is
    deliberate to avoid creating a `nexo-config ->
    nexo-driver-types` edge that would cycle through the existing
    `nexo-driver-types -> nexo-config` dep. Same precedent as
    `SecretGuardYamlConfig` shipped in slice C5. Conversion
    happens at boot in `src/main.rs`.
  - **Adapter**: `crates/driver-loop/src/extract_memories.rs`
    gains `pub struct LlmClientAdapter { llm: Arc<dyn LlmClient>,
    model: String }` with `impl ExtractMemoriesLlm`. The adapter
    packages `(system_prompt, user_messages, max_tokens)` into
    `ChatRequest::new(model, [ChatMessage::user(...)])` with
    `system_prompt` + `max_tokens` set, calls
    `self.llm.chat(req).await`, pattern-matches
    `ResponseContent::Text(s) => Ok(s)` and
    `ResponseContent::ToolCalls(_) => Err(...)`. Provider-agnostic
    — no Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
    Mistral specifics; switching the underlying `LlmClient`
    swaps the provider transparently.
  - **Helper**: `src/main.rs::resolve_extract_memory_dir`
    resolves the per-agent memory destination —
    `<workspace>/memory/` when `agent_cfg.workspace` is non-empty,
    else `<state_root>/<agent_id>/memory/` so multi-agent
    deployments stay isolated.
  - **Boot wire**: agent-loop in `src/main.rs` gains a block just
    after `let llm = llm_registry.build(...)` that converts
    `ExtractMemoriesYamlConfig` to the canonical
    `ExtractMemoriesConfig`, constructs `LlmClientAdapter` +
    `Arc<ExtractMemories>` when enabled, and stores
    `Option<Arc<dyn MemoryExtractor>>` in scope. After the
    `let mut behavior = LlmAgentBehavior::new(...)` line, a
    follow-up block calls `resolve_extract_memory_dir` +
    `std::fs::create_dir_all` (warn-and-continue on failure) and
    invokes `behavior.with_memory_extractor(...)`. Total wire
    additions: ~50 LOC + the helper.
  - **Sweep**: 50-fixture sweep added `extract_memories: None,`
    after every `assistant_mode: None,` in every existing
    `AgentConfig { ... }` literal — same mechanical perl pattern
    used for the Phase 80.15 `assistant_mode` sweep.
  - **Tests**: 2 new in `nexo-driver-loop`
    (`llm_client_adapter_chat_round_trips` verifies the
    `ChatRequest` shape and `Text` extraction;
    `llm_client_adapter_errors_on_tool_call_response` verifies
    the `ToolCalls` error path), 3 new in `nexo-config`
    (`agent_config_yaml_without_extract_memories_parses`,
    `agent_config_yaml_with_extract_memories_parses`,
    `extract_memories_default_disables`).
  - **Marketing plugin path now ready**: opt-in via
    `extract_memories: { enabled: true }` in `agents.yaml`. The
    agent processes inbound emails → reply → post-turn extract
    fires → memory persists in `<workspace>/memory/<auto>.md`.
    Re-engagement queries find the memory via the existing
    `who_am_i` / `what_do_i_know` skills. Lead memory survives
    daemon restart via the on-disk markdown files.
  - **IRROMPIBLE refs**: claude-code-leak
    `services/extractMemories/extractMemories.ts` (the leak's
    extractor calls the model directly inside `runExtraction`;
    splitting via `LlmClientAdapter` keeps the trait surface
    narrow). `research/` no relevant prior art (channel-side
    scope, no extract-memories concept).
  - **Open follow-ups**: M4.b (autoCompact in regular
    AgentRuntime), M4.c (per-session turn counter to replace
    the `turn_index = 0` sentinel), per-binding override (defer
    until binding-level extract policy is requested).
  - Tests: `cargo test -p nexo-config --lib` → 163/163,
    `cargo test -p nexo-driver-loop --lib llm_client_adapter`
    → 2/2, `cargo test -p nexo-core --lib` → 687/687 (sweep
    clean), `cargo build --bin nexo` verde.
- Phase 80.15 (MVP) — per-binding `assistant_mode` toggle: behavioural
  flag that flips proactive-agent posture for an agent's binding via
  YAML opt-in. New crate `nexo-assistant` (`crates/assistant/`,
  ~150 LOC + 6 unit tests) exposes `AssistantConfig` (re-exported
  from `nexo-config`) + `ResolvedAssistant::resolve(Option<&cfg>)` +
  `DEFAULT_ADDENDUM` const. The default addendum is plain English
  "you are running in assistant mode; default posture is proactive;
  use cron + channels + teammates; surface only what the user needs;
  stay quiet otherwise" — provider-agnostic, no LLM-specific
  phrasing. `crates/config/src/types/assistant.rs` (~140 LOC + 7
  unit tests) ships the YAML schema with three fields:
  `enabled: bool` (default false), `system_prompt_addendum:
  Option<String>` (None → use bundled default; empty string is
  rejected by `validate()`), `initial_team: Vec<String>` (validated
  for shape — alphanumeric + `-` + `_` only; actual spawn lands in
  80.15.b follow-up). `AgentConfig.assistant_mode: Option<AssistantConfig>`
  field added with `#[serde(default)]` so existing YAMLs without the
  block keep parsing. Workspace fixture sweep applied
  `perl -i -pe 's/^(\s*)auto_dream: None,$/$1auto_dream: None,\n$1assistant_mode: None,/'`
  to 49 struct literals across `crates/{core,fork,dream}` +
  `src/main.rs` + `crates/core/tests/` (single file `agents.rs`
  itself remained — that's the field declaration, not a struct
  literal). `AgentContext` gains
  `assistant: nexo_assistant::ResolvedAssistant` field; the
  `::new` constructor body initialises it to `disabled()` so all
  callers (`AgentContext::new` is invoked at 4 sites in
  `src/main.rs`) keep working without per-call changes. Toggling
  the flag at runtime requires a daemon restart (the boolean is
  boot-immutable to avoid mid-turn races); the addendum text
  itself is hot-reloadable through the existing Phase 18 path.
  `Arc<String>` for the addendum + `Arc<Vec<String>>` for the
  initial-team list mean cloning the resolved view into per-turn
  contexts is cheap. **System-prompt injection** wired in
  `crates/core/src/agent/llm_behavior.rs` adjacent to the existing
  proactive + coordinator hints (`channel_meta_parts.push(...)`
  pattern): when `ctx.assistant.should_append_addendum()` returns
  true (boot-immutable enabled flag ∧ non-empty addendum), the
  addendum text gets pushed into `channel_meta_parts` with stable
  cross-turn ordering so the LLM provider's prompt cache stays
  warm. Phase 16 binding policy already accepts arbitrary
  per-binding YAML so no schema-migration was needed.
  `nexo-core/Cargo.toml` gained `nexo-assistant` as a dep.
  Workspace `Cargo.toml::members` includes the new crate.
  Tests verde: nexo-assistant 6 (disabled-when-none /
  disabled-when-enabled-false / uses-default-when-no-override /
  honors-override / initial-team-passes-through / default-is-disabled),
  nexo-config types::assistant 7 (default / reject empty addendum /
  reject whitespace-only / reject bad team name / accept good
  names / yaml round-trip disabled / yaml round-trip full),
  workspace `cargo build --workspace` + `cargo build --workspace
  --tests` both verde. **Deferred follow-ups** (split out as named
  sub-phases for clarity):
  - **80.15.b** — `initial_team` auto-spawn at boot (needs Phase
    8 agent-to-agent + 80.10 BG sessions).
  - **80.15.c** — auto-flip `cron.enabled: true` default for
    assistant bindings (needs 80.6 killswitch).
  - **80.15.d** — auto-flip `brief: true` default (needs 80.8
    SendUserMessage tool).
  - **80.15.e** — activation-path telemetry / provenance field.
  - **80.15.f** — `nexo setup doctor` per-binding `assistant_mode`
    reporter row (polish).
  - **80.15.g** — `src/main.rs` boot wiring populating
    `ctx.assistant = ResolvedAssistant::resolve(cfg.assistant_mode.as_ref())`
    on every per-binding `AgentContext` build site (1-line
    snippet in 4 places; deferred until pre-existing dirty state
    resolves per the 80.1.b.b.b / 80.1.c / 80.1.d / 80.1.e /
    80.1.g pattern). Until 80.15.g lands, the boolean stays
    `false` for every binding at runtime — the system-prompt
    addendum stays invisible regardless of YAML. Operator can
    test by mutating `ctx.assistant` in their main.rs hookup.

  Three-pillar audit: **robusto** — default-disabled, `validate()`
  rejects malformed input, boot-immutable flag avoids mid-turn
  race, `Arc<String>` shared addendum (no per-turn alloc), 13+
  tests cover all gates including yaml round-trip; **óptimo** —
  addendum resolved once at boot, single-byte bool in
  `AgentContext`, no per-turn cost when disabled, Arc-shared
  references; **transversal** — provider-agnostic default text,
  bool readable by any consumer (driver-loop, cron, brief, dream
  context, remote-control auto-tier in 80.17), addendum is plain
  English nudge with no Anthropic / OpenAI / Gemini / MiniMax /
  DeepSeek / xAI / Mistral specific phrasing.
- **M4.a — `MemoryExtractor` trait + `LlmAgentBehavior` post-turn
  wire** (FOLLOWUPS A6.M4). Phase 77.5 shipped `ExtractMemories`
  (`crates/driver-loop/src/extract_memories.rs`, ~600 LOC + 21
  tests) with full gate logic — but the post-turn wire lived only
  in driver-loop's orchestrator at `:702-726`. Agents going
  through the regular `LlmAgentBehavior` path (event-driven
  inbound, pollers, heartbeat, marketing-style plugins) never
  extracted memories; lead/conversation memory was lost across
  reloads. Fix introduces `nexo_driver_types::MemoryExtractor`
  (`crates/driver-types/src/memory_extractor.rs`) — a 2-method
  trait (`tick`, `extract`) declared upstream of both `nexo-core`
  and `nexo-driver-loop` so they hold `Arc<dyn MemoryExtractor>`
  without depending on each other (mirrors `AutoDreamHook`
  cycle-break from Phase 80.1.b). `nexo-driver-loop` ships
  `impl MemoryExtractor for ExtractMemories` re-using the inherent
  methods. `LlmAgentBehavior` gains `memory_extractor:
  Option<Arc<dyn MemoryExtractor>>` + `memory_dir:
  Option<PathBuf>` fields plus `with_memory_extractor(extractor,
  dir)` builder. Post-turn hook (just before
  `Ok(RunTurnOutcome::Reply(reply_text))`) calls `extractor.tick()`
  unconditionally (cadence stays sane even when gates skip) and
  `Arc::clone(extractor).extract(GoalId(session_id), 0, text,
  dir)` only when both `memory_dir` is `Some` AND `reply_text` is
  `Some` — defensive: no writes outside an explicit dir, no
  extraction without an assistant turn. `turn_index = 0` is an
  MVP sentinel (the consumer uses `turn_index` only for telemetry,
  not control flow). Provider-agnostic by construction —
  `Arc<dyn MemoryExtractor>` keeps any concrete impl pluggable;
  the `ExtractMemoriesLlm` upstream is itself a narrow wrapper
  around `Arc<dyn LlmClient>`, so behavior is identical across
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
  Mistral. 3 inline tests in `llm_behavior::tests`:
  `with_memory_extractor_populates_both_fields` (builder
  semantics), `default_behavior_has_no_memory_extractor` (default
  sanity), `memory_extractor_records_tick_and_extract_calls`
  (`Arc::clone + extract(...)` dyn dispatch + side effects). Trait
  doc-comment cites IRROMPIBLE refs to claude-code-leak
  `services/extractMemories/extractMemories.ts:121-148`
  (`hasMemoryWritesSince`) and `QueryEngine.ts` (leak's
  single-turn-engine extract trigger our two engines now share via
  the trait); `research/` no relevant prior art. Slice **M4.a.b**
  (boot wire — `ExtractMemoriesConfig` in `AgentConfig` schema +
  `ExtractMemoriesLlm` adapter + per-agent `memory_dir`
  resolution), **M4.b** (autoCompact in regular AgentRuntime), and
  **M4.c** (per-session turn counter) remain open in FOLLOWUPS
  A6.M4. Marketing plugin scope: once M4.a.b lands, the marketing
  agent (event-driven, regular AgentRuntime) automatically gets
  memory persistence for leads — e.g. "juan@x.com mostró interés
  en plan Pro" survives across turns / sessions / daemon
  restarts. Tests: `cargo test -p nexo-driver-types` verde,
  `cargo test -p nexo-driver-loop --lib` verde (21 ExtractMemories
  tests preserved), `cargo test -p nexo-core --lib
  agent::llm_behavior::tests` → 9/9 (6 existing + 3 new).
- Phase 80.1.f (MVP) — docs sweep cubriendo el cluster 80.1.x
  autoDream. Extendido `docs/src/soul/dreaming.md` (single point
  of truth para consolidation, no nueva página) con 7 nuevas
  secciones ~370 LOC: (1) **Two-tier consolidation: light + deep**
  con tabla comparativa de 7 dimensiones (crate / cadence / cost /
  writes / failure mode / coordination / reference) — operadores
  ven de un vistazo la diferencia entre el scoring sweep (Phase
  10.6 era) y el deep fork-pass (Phase 80.1). (2) **Deep pass via
  fork** con sub-secciones para los 7 gates ordenados por costo
  (kairos / remote / auto_memory / auto_dream / time / scan-
  throttle / session) + ConsolidationLock semantics (mtime IS
  lastConsolidatedAt + holder_stale 1h + canonicalize symlink
  defense + try_acquire/rollback semantics) + 4-phase consolidation
  prompt (Orient → Gather → Consolidate → Prune) con apuntador a
  `crates/dream/src/consolidation_prompt.rs` para la plantilla
  completa + AutoMemFilter restrictions (FileRead/Glob/Grep/REPL
  unrestricted, Bash via `is_read_only`, FileEdit/Write scoped a
  memory_dir) + post-fork escape audit + MAX_TURNS=30 server-side
  cap. (3) **Coordination: skip pattern** explicando que cuando
  ambos pases están enabled, el light pass chequea el probe al
  inicio de `run_sweep` y defiere con `deferred_for_fork: true`
  cuando lock held por live PID; trade-off documentado (un turno
  de latencia para promotions diferidas, recoverables porque
  memorias hot scorean igual de high). (4) **Audit trail** con
  schema completa de la tabla `dream_runs` SQLite (Phase 80.18) —
  12 columnas (id / goal_id / status / phase / sessions_reviewing
  / prior_mtime_ms / files_touched JSON / turns JSON / started_at
  / ended_at / fork_label / fork_run_id) + defenses (MAX_TURNS=30
  cap, TAIL_HARD_CAP=1000, idempotent insert) + git commits con
  subject `auto_dream: N file(s) consolidated` y body con
  `audit_run_id: <uuid>` para cross-link al SQLite row +
  ejemplo `git log --grep auto_dream | nexo agent dream status`.
  (5) **Operator CLI** con 3 sub-comandos `tail|status|kill` y
  4-5 ejemplos cada uno (`--json` para scripting con jq, `--goal`
  para filtrar por goal_id, `--n` para tamaño de página, `--force`
  para abortar Running, `--memory-dir` para lock rollback, `--db`
  para override de path) + sección de DB path resolution 3-tier
  (`--db` > `NEXO_STATE_ROOT` env > XDG default
  `~/.local/share/nexo/state/dream_runs.db`) — el tier YAML está
  intencionalmente ausente porque `agents.state_root` no existe
  como config field hoy. (6) **LLM tool `dream_now`** con JSON
  tool shape completo + JSON envelope output documentando las 6
  outcomes (`completed` / `skipped` / `lock_blocked` / `errored` /
  `timed_out` / `escape_audit`) + capability gate two-layer
  documentado: host-level `NEXO_DREAM_NOW_ENABLED=true` env var
  (Phase 80.1.c.b — sin esta var, registration short-circuits
  con `tracing::info!`) ∧ Phase 16 binding policy `allowed_tools`
  array — ambos deben permitir; ejemplo de output esperado en
  `nexo setup doctor capabilities`. (7) **Configuration** con
  yaml block ejemplo bajo `agents.<id>.auto_dream` mostrando todos
  los knobs (`enabled` / `min_hours` / `min_sessions` /
  `scan_interval` / `holder_stale` / `fork_timeout` / `memory_dir`)
  + boot logging output esperado (`auto_dream runner registered
  git_checkpoint_wired=true`) + asimetría documentada (auto_dream
  off no afecta light pass; dreaming off no afecta deep pass —
  bindings independientes). Sección final **See also** con
  cross-links a Phase 10.9 git-backed memory + Phase 18 hot-reload
  + Phase 77.7 secret guard + Phase 80.18 audit row + Phase 80.20
  AutoMemFilter — todos apuntan a paths de crates locales, sin
  referencias externas. `mdbook build docs` smoke verde — sin
  broken links, página final ~560 LOC (era 186, +370). admin-ui
  panel + páginas separadas tipo `concepts/kairos-mode.md` /
  `operations/cron-jitter.md` quedan reservadas para Phase 80.21
  (broader docs sweep + admin-ui sync, no cluster 80.1.x).
  Provider-agnostic por construcción: cero LLM-provider mencionado
  en ejemplos; todo el flow funciona bajo Anthropic / MiniMax /
  OpenAI / Gemini / DeepSeek / xAI / Mistral. Cluster 80.1.x core
  ahora cerrado: 80.1 / 80.1.b / 80.1.b.b / 80.1.b.b.b / 80.1.c /
  80.1.c.b / 80.1.d / 80.1.e / 80.1.f / 80.1.g — todos ✅ MVP;
  remain follow-ups 80.1.d.b (live NATS abort, needs 80.11) y
  80.1.d.c (`agent dream now` operator force, needs daemon
  plumbing).
- **C4.c — `LlmError::QuotaExceeded` provider-agnostic + 4-provider
  plumb + last-quota cache + `setup doctor` surface** (FOLLOWUPS
  A4.c). Phase 77.11 shipped `rate_limit_info` (762 LOC, 12 tests)
  with `RateLimitInfo` + `format_rate_limit_message` returning
  `RateLimitMessage { text, severity, plan_hint }` — but the
  structured output collapsed to `tracing::warn!` at
  `anthropic.rs:391-405` and `retry.rs:118-126`, never reaching
  `setup doctor` / `notify_origin` / admin-ui (audit M-priority).
  Plus, hard 429s with `RateLimitInfo.status == Rejected` were
  retried 5× before failing — wasteful when the quota is hard. Fix
  introduces `LlmError::QuotaExceeded { retry_after_ms, severity,
  message, plan_hint, provider, window }` distinct from the
  existing `LlmError::RateLimit` (transient burst, retry-able).
  Public helper `pub fn classify_429_error(retry_after_ms,
  info: Option<RateLimitInfo>) -> LlmError` is the single source
  of truth for the 429 → variant decision: when
  `info.status == Some(Rejected)` AND
  `format_rate_limit_message(&info)` produces a message →
  `QuotaExceeded` AND a `record_quota_event` side-effect lands in
  the process-wide `static LAST_QUOTA: OnceLock<DashMap<LlmProvider,
  QuotaEvent>>` so the most recent rejection per provider survives
  for `setup doctor` to render. Otherwise (no info, AllowedWarning,
  Allowed) → `RateLimit` (retry transient bursts). `with_retry`
  short-circuits `QuotaExceeded` (no retry, no backoff). Wired in
  4 provider classify_response sites: `anthropic.rs:381`,
  `openai_compat.rs:81` (covers OpenAI / xAI / DeepSeek / Mistral
  via shared `x-ratelimit-*`), `gemini.rs:95`, and `minimax.rs:228`
  chat path + `:280` finish path (MiniMax speaks OpenAI-compat).
  `LlmProvider` gained `Hash` derive so it can key the cache
  `DashMap`. `setup doctor` renders a new "LLM quota" section
  iterating `last_quota_events_all()`: `[!]` icon for `Error`
  severity, `[.]` for `Warning`, age in minutes since `at`,
  message + optional `plan_hint` indented. Empty cache renders
  "no recent quota events". `nexo-setup` gained `nexo-llm` as
  a direct dep. 9 inline tests covering promotion + cache +
  extractor flow + `with_retry` no-retry guard. Test-only
  `pub fn clear_last_quota()` isolates state. 100%
  provider-agnostic across Anthropic / OpenAI / Gemini / MiniMax /
  Generic (xAI / DeepSeek / Mistral). Doc-comment cites
  IRROMPIBLE refs to claude-code-leak
  `services/api/errors.ts:465-548` (3-tier 429 classification)
  and `services/rateLimitMessages.ts:45-104`
  (`getRateLimitMessage`). `research/` no relevant prior art.
  C4.c.b (notify_origin wire), C4.c.c (admin-ui A8 panel +
  Prometheus metric), C4.c.d (Anthropic entitlement-reject hint)
  remain open in FOLLOWUPS A4.c. Tests:
  `cargo test -p nexo-llm --lib` → 167/167 (158 existing + 9 new).
- Phase 80.1.e (MVP) — coordination skip entre scoring sweep y
  autoDream fork-pass via consolidation-lock probe. **PIVOTED** del
  plan original "buffer pattern `_pending_promotions.md`" al
  **SKIP pattern** alineado con leak
  `claude-code-leak/src/services/extractMemories/extractMemories.ts:121-148`
  `hasMemoryWritesSince`. El buffer original era complejidad
  inventada que el leak NO tiene — cuando un memory-writer está
  activo, el otro defiere entirely. Mutually exclusive per turn.
  Nuevo trait `nexo_driver_types::ConsolidationLockProbe`
  (`crates/driver-types/src/consolidation_lock_probe.rs`, ~30 LOC
  + 1 trait-object-safety test) sentado upstream de `nexo-dream` y
  `nexo-core` (mismo cycle-break que Phase 80.1.b `AutoDreamHook`
  y Phase 80.1.g `MemoryCheckpointer` patterns). Método
  `is_live_holder(&self) -> bool` SYNC — un real impl es solo un
  `stat()` + parse + `kill(0)`, no surprise async I/O. Doc-comment
  del trait documenta fail-open semantics: cualquier I/O / parse
  error → retornar `false` sin panic. Impl en
  `crates/dream/src/consolidation_lock.rs` para `ConsolidationLock`
  reusing existing `is_pid_running` (`:217`): lee el lock-file con
  `std::fs::read_to_string`, parsea PID body — `Ok(0)` (rollback
  marker) → false, `Ok(pid > 0)` → `is_pid_running(pid)`,
  `Err(_)` → false. 5 probe tests verde:
  `probe_returns_false_when_lock_absent` (sin lock file en
  memory_dir), `probe_returns_false_for_pid_zero` (cuando rollback
  ya rewrote a `"0"`), `probe_returns_true_for_live_pid` (usa
  `std::process::id()` para evitar surprises de PID 1 en sandbox),
  `probe_returns_false_for_dead_pid` (PID 999999 fuera del pid_max
  típico de Linux), `probe_returns_false_for_garbage_body` (body
  `"not-a-pid"`). En `nexo-core::agent::dreaming`, `DreamReport`
  gana field `deferred_for_fork: bool` y `DreamEngine` gana field
  `consolidation_probe: Option<Arc<dyn ConsolidationLockProbe>>` +
  builder `with_consolidation_probe(probe)`. `run_sweep` chequea
  el probe AL INICIO (después del log "dream sweep started", antes
  de cualquier query a SQLite o filesystem), y si
  `probe.is_live_holder() == true` retorna early con
  `DreamReport { deferred_for_fork: true, candidates_considered: 0,
  promoted: vec![], skipped_already_promoted: 0, started_at,
  finished_at, agent_id }` y log info `"dream sweep deferred —
  autoDream fork holds consolidation lock"`. Sin probe (`None`
  campo) → behaviour idéntico a pre-80.1.e — backward compatible.
  Trade-off documentado en doc-comment del builder: promociones
  del scoring sweep durante la ventana del fork se difieren al
  siguiente turno; memorias hot siguen scoring high next turn,
  costo es a lo sumo un turno de latencia, mucho menor que la
  complejidad del buffer (drain ordering, secret-guard scoping
  sobre buffer drain, race en archivo de buffer mismo, edge cases
  de partial drain). 3 nuevos tests en
  `nexo-core::agent::dreaming::tests`:
  `run_sweep_proceeds_when_no_probe_configured` (probe `None` —
  promotion normal con `deferred_for_fork: false`),
  `run_sweep_proceeds_when_probe_says_dead` (probe `Some` con
  `MockProbe::new(false)` — promotion normal),
  `run_sweep_skips_when_probe_says_live` (probe `Some` con
  `MockProbe::new(true)` — `deferred_for_fork: true`, NO
  candidates considered, NO `MEMORY.md` written, SQLite ledger
  sin promotion entry, verifica que el sweep no tocó nada). Mock
  probe usa `AtomicBool` toggleable para tests deterministas
  inmutables. Defense-in-depth preservada: AutoMemFilter (Phase
  80.20) + ConsolidationLock acquire/rollback + secret guard
  Phase 77.7 + MAX_COMMIT_FILE_BYTES + AHORA TAMBIÉN coordination
  skip que evita race directo en `MEMORY.md` writes entre los
  dos passes. main.rs hookup queda documentado en doc-comment del
  builder con snippet 1-line: cuando el agent tiene
  `dreaming.enabled && auto_dream.is_some()`, construir
  `Arc::new(ConsolidationLock::new(memory_dir, holder_stale)?) as
  Arc<dyn ConsolidationLockProbe>` y pasar a
  `DreamEngine::with_consolidation_probe(probe)`. Diferido hasta
  resolución de dirty state pre-existente del usuario (mismo
  pattern que 80.1.b.b.b / 80.1.c / 80.1.d / 80.1.g main.rs
  hookups). Provider-agnostic por construcción: pure filesystem
  + POSIX PID semantics; cero touchpoints LLM-provider; transversal
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI / Mistral.
  Tests totales verde: nexo-driver-types 24 (23 + 1 nuevo),
  nexo-dream 72 (67 + 5 probe), nexo-core dreaming 8 (5 + 3 nuevos
  con MockProbe). Workspace build verde. Out of scope (deferred):
  80.1.e.b (revivir buffer pattern si aparece evidencia de que el
  SKIP pierde promotions importantes — por ahora hipotético; SKIP
  + re-evaluation next turn cubre los casos esperados), 80.1.e.c
  (sweep-during-fork via parallel write a archivo distinto como
  `MEMORY-pending.md`). `research/` (OpenClaw) carries no relevant
  prior art — single-process Node app sin two-tier consolidation;
  **absence noted** per IRROMPIBLE rule.
- Phase 80.1.g (MVP) — wire git auto-commit a AutoDream fork-pass.
  Closes the Phase 10.9 forensics gap on the deep-pass consolidation:
  before this slice, the scoring-sweep dreaming (`crates/core/src/agent/
  dreaming.rs`) auto-committed via `MemoryGitRepo::commit_all` at
  `src/main.rs:3640-3665` but the fork-style autoDream
  (`crates/dream/`) reescribía archivos en `memory_dir` directamente
  sin pasar por git — perdiendo `git blame` / `git revert` / secret
  guard de Phase 77.7 sobre los writes del fork. Nuevo trait
  `nexo_driver_types::MemoryCheckpointer` (`crates/driver-types/src/
  memory_checkpoint.rs`, ~25 LOC + 1 trait-object-safety test)
  upstream de `nexo-dream` y `nexo-core` (mismo cycle-break que el
  `AutoDreamHook` de Phase 80.1.b). Adapter `MemoryGitCheckpointer
  { repo: Arc<MemoryGitRepo> }` en `crates/core/src/agent/
  workspace_git.rs` (~25 LOC + 2 inline tests
  `checkpointer_async_calls_commit_all` y
  `checkpointer_returns_ok_on_clean_worktree`) envuelve el
  `commit_all` blocking en `tokio::task::spawn_blocking` porque
  `git2::Repository` es sync-only. Newtype obligatorio por Rust
  orphan rule — `impl ForeignTrait for Arc<Local>` no compila.
  `AutoDreamRunner` gana field `git_checkpointer: Option<Arc<dyn
  MemoryCheckpointer>>` + builder `with_git_checkpointer(ckpt)` +
  observability accessor `has_git_checkpointer()`. El `run` invoca
  el checkpointer **DESPUÉS** de `audit.update_status(Completed) +
  finalize` (audit row primero — fuente de verdad — git commit
  segundo, bonus forensics) y **SOLO** cuando
  `progress.touched.is_empty() == false` (decisión D-2: empty
  touches no generan commits vacíos; el audit row en
  `dream_runs.db` ya captura la pasada). Helper
  `build_checkpoint_body(run_id, files)` rinde format
  `audit_run_id: <uuid>\n\n- path1\n- path2\n` para que `git log
  --grep auto_dream` cross-linkee al audit row vía el run_id.
  Subject template: `auto_dream: <N> file(s) consolidated`.
  Failure del checkpointer → `tracing::warn!(target:
  "auto_dream.checkpoint", run_id, error, "memory checkpoint
  failed; audit row preserved")` SIN downgrade del outcome — el
  fork ya escribió el memory_dir y el audit row está correcto, un
  commit fallido es solo forensics perdida; misma semántica que
  el scoring sweep en `:3656-3663`. `BootDeps` gana field
  `git_checkpointer: Option<Arc<dyn MemoryCheckpointer>>`,
  `nexo_dream::boot::build_runner` lo cablea con
  `with_git_checkpointer(ckpt)` y emite
  `git_checkpoint_wired = bool` en el log de boot para
  observabilidad operacional. main.rs hookup para construir
  `MemoryGitCheckpointer::new(Arc::clone(&agent_git)) as Arc<dyn
  MemoryCheckpointer>` queda documentado en doc-comment del
  builder — diferido hasta que el usuario resuelva su dirty
  state pre-existente con la hookup general de
  `nexo_dream::boot::build_runner`. 4 nuevos tests en
  `nexo-dream::auto_dream::tests`:
  `build_checkpoint_body_renders_run_id_and_paths` (run_id en
  primera línea + bullet por path),
  `build_checkpoint_body_renders_empty_file_list` (run_id sin
  bullets),
  `with_git_checkpointer_setter_round_trips`
  (`has_git_checkpointer` antes false, después true),
  `checkpoint_skipped_when_files_touched_empty` (verifica el
  guard `if !empty` con `RecordingCheckpointer` mock que cuenta
  llamadas — assert `count == 0` con MockFork::ok que produce
  empty progress.touched; valida la decisión D-2),
  `checkpoint_failure_does_not_downgrade_completed_outcome`
  (mock `RecordingCheckpointer::failing` retorna `Err` — verifica
  que el outcome NO termina en `Errored`). `RecordingCheckpointer`
  mock impl con `AtomicUsize` counter + `StdMutex<Vec<(subject,
  body)>>` log + flag `failing()` para tests defensivos.
  Defense-in-depth preservada: AutoMemFilter (Phase 80.20 sandbox
  físico) ∧ ConsolidationLock ∧ secret guard de Phase 77.7
  (transparent vía `MemoryGitRepo::with_guard` que rechaza
  commits con secretos detectados) ∧ MAX_COMMIT_FILE_BYTES (1 MB
  cap, archivos grandes loggeados pero no fatales) ∧
  `Mutex<Repository>` serialización con otros callers
  (session-close commit, scoring-sweep commit). Provider-agnostic
  por construcción: el trait permite cualquier checkpointer
  (git, S3 backup, dual-write audit log); cero touchpoints
  LLM-provider; pure infra layer transversal a Anthropic /
  MiniMax / OpenAI / Gemini / DeepSeek / xAI / Mistral. Tests
  totales verde: nexo-driver-types 23 (22 + 1 nuevo), nexo-core
  workspace_git tests +2 (2 nuevos), nexo-dream auto_dream 16
  (12 + 4 nuevos), boot 7 (todos con `git_checkpointer: None`
  en `mk_deps` fixture), 67 nexo-dream tests totales. Mirror
  reference: NO hay precedente en `claude-code-leak/` —
  `memdir/paths.ts:14` usa `findCanonicalGitRoot` solo para
  localizar el memory dir (path discovery, no commit);
  `memoryTypes.ts:187` documenta explícitamente la postura del
  leak: "Git history, recent changes, or who-changed-what —
  `git log` / `git blame` are authoritative" — el leak NO
  duplica info git en memoria. Phase 10.9 git-backed memory
  (existing nexo) + 80.1.g (este sub-fase) son innovación
  nexo-específica que extiende esa parity al fork-pass deep
  consolidation. `research/` (OpenClaw) carries no relevant
  prior art — single-process Node app expects user to manage
  git themselves. Out of scope (deferred): 80.1.g.b commit on
  Killed con subject `auto_dream KILLED: <N> file(s) partial`
  (revisar cuando haya demanda — operador puede usar
  `forge_memory_checkpoint` manual mientras), 80.1.d.d auto
  `git revert HEAD` opcional en `nexo agent dream kill --revert`
  (no urgente — `ConsolidationLock::rollback` ya cubre el "no
  re-fire" path).
- **C4.b — sandbox 5th tier in `gather_bash_warnings`** (FOLLOWUPS
  A4.b advisory MVP). The Phase 77.10 `should_use_sandbox` module
  (`crates/driver-permission/src/should_use_sandbox.rs`, 401 LOC +
  20 tests) had zero production callers outside `#[cfg(test)]`
  since shipping — the audit ("computed and discarded"). Fix wires
  the heuristic as a 5th advisory tier in `gather_bash_warnings`
  (`crates/driver-permission/src/mcp.rs`) coupled to risk: fires
  only when (1) at least one prior tier (destructive, sed-shallow,
  sed-deep, path-extractor) already flagged the command AND (2)
  `SandboxProbe` detected `bwrap` or `firejail` on PATH. The
  coupling is intentional — leak's
  `should_use_sandbox(_, Auto, Some_backend, false, [])` returns
  `true` for ANY command (not command-aware), so firing alone
  would emit advisory on every Bash call on a sandbox-equipped
  host. Coupling to existing warnings keeps the
  signal-to-noise ratio high: a no-warning command on a
  sandbox-equipped host stays silent. Probe is process-wide via
  `static SANDBOX_PROBE: std::sync::OnceLock<SandboxProbe>` —
  runs `which bwrap` + `which firejail` once on first call and
  caches the detected backend, prefers bwrap when both present.
  Refactor split: public `gather_bash_warnings(tool_name, input)`
  resolves the static probe and delegates to internal
  `gather_bash_warnings_with_backend(tool_name, input,
  sandbox_backend: SandboxBackend)` which accepts the backend
  explicitly so tests inject `SandboxBackend::Bubblewrap` /
  `Firejail` / `None` deterministically without hitting `which`
  on the test host (same testability pattern as M2's
  `compute_args_metrics`). MVP hard-codes `SandboxMode::Auto`,
  empty `excluded_commands`, `dangerously_disable_sandbox: false`
  — YAML config schema (`runtime.bash_safety.sandbox.{mode,
  excluded_commands, dangerously_disable}` + per-binding
  override + Phase 18 hot-reload re-validation + admin-ui Phase
  A8 surface) defers to slice C4.b.b along with the leak's
  fixed-point `stripAllLeadingEnvVars` + `stripSafeWrappers`
  normalization (only relevant once excluded_commands exists).
  Warning shape: `"sandbox backend available ({bwrap|firejail});
  consider wrapping risky commands above before execution"`. All
  tiers stay advisory — final allow/deny remains with the
  upstream LLM decider. 3 inline tests in `mcp::tests`:
  `gather_bash_warnings_appends_sandbox_advisory_when_risky_and_backend_available`
  (`rm -rf /tmp/x` + injected `Bubblewrap` → fires "sandbox
  backend available (bwrap)"),
  `gather_bash_warnings_skips_sandbox_when_no_backend`
  (same risky command + injected `None` → tier 5 silent, other
  tiers still fire),
  `gather_bash_warnings_skips_sandbox_when_no_other_warnings`
  (`echo hi` + injected `Firejail` → result `None` because
  `!warnings.is_empty()` gate denies tier 5). Doc-comment now
  documents 5 tiers + risk-coupling rationale + scope note +
  IRROMPIBLE refs to claude-code-leak
  `shouldUseSandbox.ts:130-153` (pure decision shape backing
  the helper — leak's wrapper actually wraps the command in
  `bwrap`/`firejail` before exec; we stay advisory because our
  decider is the upstream LLM, not the bash exec path) and
  `:55-58` (`excludedCommands` is "not a security boundary"
  disclaimer). `research/` carries no relevant prior art —
  OpenClaw is channel-side and the only `sandbox` references
  are Docker test fixtures (`docker-setup.e2e.test.ts:52`).
  Provider-agnostic: probe + decision operate on command string
  + PATH; LLM provider does not enter the decision. Transversal
  Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
  Mistral. Slice C4.b.b (YAML config) and the L1 follow-up
  (real `bwrap`/`firejail` wrapping at exec time) remain open.
  Tests: `cargo test -p nexo-driver-permission --lib
  gather_bash_warnings` → 7/7 (4 from C4.a + 3 new).
- Phase 80.1.d (MVP) — `nexo agent dream {tail|status|kill}` operator
  CLI for the autoDream audit log + manual abort. Adds
  `Mode::AgentDream(AgentDreamSubcommand)` (next to `Mode::McpServer`
  precedent at `src/main.rs:74`) and `enum AgentDreamSubcommand
  { Tail | Status | Kill }`. Four parser arms (bare `agent dream`
  defaults to tail, plus `agent dream tail|status|kill` verbs)
  use the existing hand-rolled positional matcher with the
  `parse_kv_flag` helper at `src/main.rs:5667` for `--goal`,
  `--n`, `--db`, `--memory-dir` kv pairs (also accepts `--json`
  via the global `has_json_flag` and `--force` as a positional
  bool). Three async run fns ship adjacent to
  `run_mcp_tail_audit:9963`. **`run_agent_dream_tail`** opens
  `dream_runs.db` via `SqliteDreamRunStore::open` and dispatches
  to `tail(n)` (no goal filter) or `tail_for_goal(GoalId(uuid),
  n)` when `--goal=<uuid>` is set, then renders either a markdown
  table (TTY default) or `serde_json::to_string_pretty(&rows)`
  (`--json`). Empty / missing-DB case returns `Ok(())` with a
  friendly "(no dream runs recorded yet — db not found at ...)"
  message rather than erroring — operators inspecting before the
  daemon ever ran with auto_dream-enabled bindings should not see
  a stack trace. **`run_agent_dream_status`** validates the uuid
  upfront (`uuid::Uuid::parse_str` with anyhow context),
  `store.get(uuid)` → renders full row (id, goal_id, status,
  phase, sessions, fork_label, started_at, optional ended_at,
  optional prior_mtime_ms, files_touched list, last 5 turns
  summary). **`run_agent_dream_kill`** parses uuid, fetches the
  row, returns early-noop when status is already terminal
  (`Completed` / `Failed` / `Killed` / `LostOnRestart`),
  warn-and-`std::process::exit(2)` when row is `Running` and
  `--force` is absent (preventing accidental aborts), otherwise
  calls `update_status(Killed)` + `finalize(now())` + (when
  `--memory-dir <path>` is supplied AND `prior_mtime_ms.is_some()`)
  `ConsolidationLock::new(memory_dir, 1h_holder_stale).rollback(prior)`
  to rewind the consolidation-lock mtime so the next non-force
  turn sees the lock as if no consolidation had fired. Helper
  `resolve_dream_db_path(override)` implements 3-tier resolution:
  (1) `--db <path>` explicit override, (2) `NEXO_STATE_ROOT` env
  → `<state_root>/dream_runs.db` via
  `nexo_dream::default_dream_db_path`, (3) XDG default
  `dirs::data_local_dir() / "nexo/state/dream_runs.db"`. The YAML
  tier is intentionally absent — `agents.state_root` is not a
  config field today (state_root flows into `BootDeps` directly
  per Phase 80.1.b.b.b documentation), so the CLI uses the
  env-or-default fallback to stay aligned with the daemon's
  discovery path once main.rs hookup ships. `DreamRunRow` (in
  `crates/agent-registry/src/dream_run.rs:135`) gained
  `Serialize + Deserialize` derives so the `--json` output path
  serialises directly without an intermediate type. Workspace
  `Cargo.toml` gained `nexo-dream = { path = "crates/dream" }`,
  `nexo-driver-types = { path = "crates/driver-types" }`,
  `dirs = "5"` in `[dependencies]` and `tempfile = "3"` in a new
  `[dev-dependencies]` section — these resolve the
  rust-analyzer-flagged drift left over from Phase 80.1.c that
  the M8.a CHANGELOG entry called out as a binary-build blocker
  ("dream surface dirty state"); both blockers are now resolved.
  11 inline tests in `src/main.rs::tests`:
  `resolve_dream_db_path_override_wins` (override beats env beats
  XDG), `resolve_dream_db_path_uses_env_when_no_override`
  (env → expected path), `short_uuid_takes_first_eight_chars`
  (compact-id helper), `run_agent_dream_tail_empty_db_exits_zero`
  (missing DB → friendly message + Ok), `run_agent_dream_tail_with_rows_renders`
  (seed 1 row → markdown render), `run_agent_dream_tail_json_output`
  (seed 1 Running row → `--json` path), `run_agent_dream_status_not_found_errors`
  (bogus uuid lookup → `not found` error), `run_agent_dream_status_returns_row`
  (real uuid lookup → render), `run_agent_dream_status_invalid_uuid_errors`
  (`"not-a-uuid"` → `not a valid UUID` error), `run_agent_dream_kill_already_terminal_is_noop`
  (Completed row → noop, no `--force` needed),
  `run_agent_dream_kill_running_with_force_flips_status`
  (Running row + `--force` → status flips to `Killed` and
  `ended_at` populated, verified post-kill via second
  `store.get`). Static `DREAM_ENV_LOCK: Mutex<()>` serialises
  env-var manipulation across the parallel-running `#[tokio::test]`
  suite. CLI smoke: `mkdir -p /tmp/nexo-test/state &&
  NEXO_STATE_ROOT=/tmp/nexo-test/state ./target/debug/nexo
  agent dream tail` → "(no dream runs recorded yet — db not
  found at /tmp/nexo-test/state/dream_runs.db)" exit 0;
  `agent dream tail --json` → `[]` exit 0; `agent dream status
  7a3b2f00-deaf-cafe-beef-001122334455` → exit 1 "Error:
  dream_runs DB not found at /tmp/nexo-test/state/dream_runs.db".
  Provider-agnostic by construction — pure SQLite + filesystem
  primitives, zero LLM-provider touchpoints; works under any
  `Arc<dyn LlmClient>` impl across Anthropic / MiniMax / OpenAI /
  Gemini / DeepSeek / xAI / Mistral. Mirror leak
  `claude-code-leak/src/components/tasks/BackgroundTasksDialog.tsx:281,315-317`
  `DreamTask.kill(taskId, setAppState)` semantics — leak does
  this through the Ink BackgroundTasksDialog keyboard ('x' key);
  we ship as CLI subcommand because nexo has no Ink-equivalent
  yet (Phase 80.16 attach/discover would parallel). Remaining
  follow-ups: 80.1.d.b (live NATS abort signal — `agent.dream.abort.<run_id>`
  subject contract needs Phase 80.11 inbox primitive), 80.1.d.c
  (`agent dream now <agent_id> [--reason "..."]` operator force
  trigger — needs daemon-runtime tool dispatch plumbing to
  invoke `dream_now` out-of-band), parser unit tests deferred
  (covered by manual smoke + 11 run-fn integration tests; the
  hand-rolled positional parser is hard to unit-test without
  env-arg manipulation).
- **M8.a — built-in deferred tools sweep** (FOLLOWUPS A6.M8). Phase
  79.2 shipped the deferred-schema infrastructure
  (`ToolMeta::deferred()` + `to_tool_defs_non_deferred()` +
  `deferred_tools_summary()`) but only `mcp_catalog.rs:253-257`
  consumed it (auto-deferring `mcp__*` tools at registration). The
  six leak-defaulted built-ins (`TodoWrite`, `NotebookEdit`,
  `RemoteTrigger`, `Lsp`, `TeamCreate/Delete/SendMessage`, `Repl`)
  registered without a meta, so the LLM request body still carried
  their full JSONSchemas every turn — the `ToolSearch` token-budget
  win was partial. Fix introduces
  `crates/core/src/agent/built_in_deferred.rs` with
  `BUILT_IN_DEFERRED_TOOLS: &[(&'static str, &'static str)]` —
  canonical 12-entry `(name, search_hint)` slice covering the 6
  audit-listed tools plus `TeamList` / `TeamStatus` (per leak
  `TaskListTool.ts:52` list/status precedent) and `ListMcpResources`
  / `ReadMcpResource` (per leak `ListMcpResourcesTool.ts:50` /
  `ReadMcpResourceTool.ts:59`, mirroring the `mcp_catalog.rs:253`
  symmetry for unprefixed router tools). `pub fn
  mark_built_in_deferred(registry: &ToolRegistry)` iterates the
  slice and calls `registry.set_meta(name,
  ToolMeta::deferred_with_hint(hint))`. Idempotent in two senses:
  (1) tools not registered in this boot (gated off via
  `agent.team.enabled = false`, `agent.lsp.enabled = false`,
  `agent.repl.runtimes = []`, etc.) are silently skipped because
  `set_meta` only writes the side-channel meta map and doesn't
  require a handler; (2) calling N times has the same effect as
  calling once — last write wins, all writes carry identical
  content. Single sweep call wired in `src/main.rs:3293-3303` after
  ALL tool registrations (including MCP via
  `register_session_tools_with_overrides`) and BEFORE the second-
  pass binding validation, so the registry is fully assembled when
  the meta lands. The leak's `name == TOOL_SEARCH_TOOL_NAME` carve-
  out is implicitly preserved — `ToolSearch` itself is never in
  `BUILT_IN_DEFERRED_TOOLS`, and `mcp_catalog.rs` never marks it
  either. Module doc-comment ports the cap+emit coupling rule plus
  9 IRROMPIBLE refs to claude-code-leak: `Tool.ts:438-449`
  (`shouldDefer` / `alwaysLoad` semantics),
  `tools/ToolSearchTool/prompt.ts:62-108` (`isDeferredTool`
  decision tree), `services/api/claude.ts:1136-1253` (token-budget
  rationale + `<available-deferred-tools>` synthetic block format),
  and 7 per-tool `shouldDefer: true` sites (TodoWriteTool:51,
  NotebookEditTool:94, RemoteTriggerTool:50, LSPTool:136,
  TeamCreateTool:78, TeamDeleteTool:36, TaskListTool:52,
  SendMessageTool:533, ListMcpResourcesTool:50,
  ReadMcpResourceTool:59); `research/` carries no relevant prior
  art (channel-side, no `ToolSearch` concept). 3 inline tests in
  `tool_registry::tests`:
  `mark_built_in_deferred_excludes_listed_tools` (registers 3
  in-list + 1 not-in-list, asserts `to_tool_defs_non_deferred()`
  returns only the not-in-list, asserts the 3 appear in
  `deferred_tools()`),
  `mark_built_in_deferred_skips_absent_tools` (empty registry +
  sweep doesn't panic),
  `mark_built_in_deferred_propagates_search_hints` (verifies
  `meta("TodoWrite").search_hint == Some("todo, tasks,
  in-progress checklist")` after sweep). Provider-agnostic:
  deferral filtering happens at the `ToolRegistry` layer that every
  provider shim — Anthropic, MiniMax, OpenAI, Gemini, DeepSeek,
  xAI, Mistral — consumes uniformly via
  `to_tool_defs_non_deferred()`. Switching providers does NOT
  change which tools are deferred. Slices remain open: M8.b
  (plan-mode tools), M8.c (5 cron tools), M8.d (`WebSearch` /
  `WebFetch`), and the Phase 79.2 follow-up wire that teaches the
  4 LLM provider shims to actually consume
  `to_tool_defs_non_deferred()` instead of `to_tool_defs()` in the
  request body — M8.a ships the registry-side marking; the
  per-turn token win lands when shims consume it.
  Tests: `cargo test -p nexo-core --lib agent::tool_registry::tests`
  → 19/19 (16 existing + 3 new). Note: binary build
  (`cargo build --bin nexo`) is blocked by pre-existing dirty state
  from Phase 80.1.d (`nexo_dream` crate not in `Cargo.toml`,
  `DreamRunRow` lacks `Serialize`, `GoalId::as_uuid` removed) — the
  M8 changes themselves are isolated to `crates/core/` (new module
  + 1 re-export + 3 tests) plus a single-line wire in `src/main.rs`,
  none of which touch the dream surface.
- Phase 80.1.c.b (MVP) — `dream_now` capability gate INVENTORY
  entry. `crates/setup/src/capabilities.rs::INVENTORY` (line 280
  region) appends `CapabilityToggle { extension: "dream", env_var:
  "NEXO_DREAM_NOW_ENABLED", kind: ToggleKind::Boolean, risk:
  Risk::Medium, effect: "Allow the LLM to force a memory-
  consolidation pass via the `dream_now` tool. Bypasses time /
  session / kairos / remote gates but honors
  `<memory_dir>/.consolidate-lock` (one fork at a time). Each
  call spawns a forked subagent up to 30 turns with FileEdit /
  FileWrite scoped to <memory_dir> and Bash limited to read-only
  commands. Cost: thousands of tokens per fire.", hint: "export
  NEXO_DREAM_NOW_ENABLED=true" }` so `nexo setup doctor
  capabilities` lists the host-level dream_now gate beside the
  other dangerous toggles. `crates/dream/src/tools.rs::register_dream_now_tool`
  now short-circuits when the env is unset / falsy: a private
  `is_dream_now_env_enabled()` mirrors `nexo-setup::capabilities::
  evaluate_one` Boolean coercion (truthy = `true` / `1` / `yes`,
  case-insensitive, trimmed; anything else = false) and the public
  `register_dream_now_tool` early-returns with `tracing::info!(
  target: "nexo_dream::tools", env_var, "dream_now: host-level
  capability gate closed; tool not registered")`. Comment block
  documents drift invariant — the 7-line coercion is duplicated
  in `nexo-dream` instead of pulling `nexo-setup` (with its plugin
  / auth / google / whatsapp transitive deps) into the dream
  crate; both copies share the identical truthy set so the host
  doctor + the registration guard stay coherent. Two-layer gate
  composes cleanly: (1) `NEXO_DREAM_NOW_ENABLED` host env (this
  entry, default deny) ∧ (2) Phase 16 per-binding `allowed_tools`
  (verified existing `Vec<String>` schema in `crates/config/src/
  types/agents.rs:138` admits `dream_now` without schema change).
  Pulled `anyhow` from `[dev-dependencies]` to `[dependencies]`
  in `crates/dream/Cargo.toml` fixing pre-existing drift —
  `tools.rs` lib code used `anyhow::Result` as the
  `ToolHandler::call` return shape but only `cargo test
  -p nexo-dream` worked because dev-deps inflate the available
  crate set; `cargo build --workspace` exposed the missing
  declaration. Tests verde: nexo-setup 7 capability tests
  including `inventory_has_expected_entries` extended with 3 new
  asserts (env var presence, extension `"dream"`, risk `Medium`,
  kind `Boolean`); nexo-dream 12 tools tests adding 4 new
  (`register_dream_now_skips_when_env_disabled` for unset env,
  `register_dream_now_skips_when_env_garbage` for non-truthy
  string `"maybe"`, `register_dream_now_registers_for_truthy_variants`
  iterating 6 truthy variants `["true", "TRUE", "True", "1",
  "yes", "YES"]` per existing `boolean_true_variants_are_enabled`
  parity, `register_dream_now_skips_for_falsy_variants` iterating
  6 falsy + edge variants `["false", "FALSE", "0", "no", "",
  "garbage"]`). Tests use a `static ENV_LOCK: Mutex<()>` +
  `EnvGuard<'a>` RAII helper (sets / unsets `NEXO_DREAM_NOW_ENABLED`
  with cleanup at drop) so concurrent `cargo test` runs don't race
  on process-wide env state. Provider-agnostic: env-var gate runs
  BEFORE LLM dispatch so the registration short-circuits regardless
  of which provider drives it (Anthropic / MiniMax / OpenAI /
  Gemini / DeepSeek / xAI / Mistral). Mirror leak
  `claude-code-leak/src/services/autoDream/autoDream.ts:95-107`
  composed-flag `isGateOpen()` pattern (we collapse the multi-flag
  composition to a single env var because the per-binding
  allow/deny already lives in Phase 16).
- **M1.a — `tools/listChanged` capability + hot-swap allowlist**
  (FOLLOWUPS A6.M1). `ToolRegistryBridge` (`crates/core/src/agent/
  mcp_server_bridge/bridge.rs:85-200`) hard-coded
  `"tools": { "listChanged": false }` since Phase 12.6 even though
  Phase 76.7 shipped `HttpServerHandle::notify_tools_list_changed()`
  — clients connected over HTTP/SSE never registered the
  notification handler (per leak `useManageMCPConnections.ts:618-665`
  the consumer side only listens when the server advertises
  `capabilities.tools.listChanged: true`), so any future
  hot-reload of `mcp_server.expose_tools` would have been a no-op
  on connected clients. Fix migrates the bridge in two parts:
  1) `allowlist: Option<HashSet<String>>` →
  `allowlist: Arc<ArcSwap<Option<Arc<HashSet<String>>>>>` so an
  external caller can atomically replace the filter via
  `swap_allowlist(new)` without reconstructing the bridge —
  `is_allowed()` reads via `arc_swap::Guard`, in-flight calls
  finish against the previous snapshot, all `Clone`d bridges
  share the same `Arc<ArcSwap>` so a single swap is visible
  across stdio + HTTP transports atomically;
  2) `list_changed_capability: bool` field (default `false`) +
  `with_list_changed_capability(on)` builder, read by
  `capabilities()` instead of the hard-coded `false`.
  `start_http_transport` (`src/main.rs:10100-10183`) clones the
  bridge with `with_list_changed_capability(true)` before passing
  it to `start_http_server`, because the HTTP transport CAN push
  `notifications/tools/list_changed` (Phase 76.7 SSE broadcast).
  Stdio path keeps the default `false` because there is no
  server→client push channel today (no bidir transport mid-session;
  defer to slice M1.c). 5 inline tests in `bridge::tests`:
  `capability_defaults_to_false` (sanity),
  `with_list_changed_capability_flips_capability` (builder
  semantics + resources/prompts stay false — M1 only touches
  tools), `swap_allowlist_visible_immediately` (Some({A}) →
  Some({B}) → None all observable on next list_tools call),
  `swap_allowlist_propagates_through_clone` (`Arc<ArcSwap>`
  shared-state invariant — swap on original, clone observes new
  set), `proxy_tools_filtered_regardless_of_swap` (the hard-coded
  `ext_*`/`mcp_*` proxy filter survives any swap because
  open-relay defense lives ABOVE the allowlist gate). Doc-comment
  on the struct documents the cap+emit coupling rule (advertise
  true ⇒ caller MUST emit, advertise false ⇒ no point emitting)
  with IRROMPIBLE refs to claude-code-leak `useManageMCPConnections.ts:618-665`
  (consumer-side handler registration) and `:628-633`
  (invalidate-then-fetch refresh pattern). Provider-agnostic:
  MCP capability negotiation is protocol-level and transversal
  to Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
  Mistral. Slice M1.b (trigger that calls `swap_allowlist` +
  `notify_tools_list_changed()` on config change) and slice M1.c
  (stdio server→client notification pump so stdio path can also
  cap=true) remain open in FOLLOWUPS A6.M1. Tests:
  `cargo test -p nexo-core --lib agent::mcp_server_bridge::bridge::tests`
  → 17/17 (12 existing + 5 new).
- Phase 80.1.c (MVP) — `dream_now` LLM tool
  (`crates/dream/src/tools.rs`, ~250 LOC + 9 unit tests). Forces a
  memory-consolidation pass on demand from inside an LLM turn —
  bypasses the kairos / remote / time / session gates while still
  honoring the PID-mtime `.consolidate-lock` (only one fork at a
  time). `DreamNowTool { runner: Arc<AutoDreamRunner>,
  transcript_dir: PathBuf }` implements `ToolHandler::call(ctx,
  args)`: extracts optional `args.reason: string` (defensive —
  empty / missing / non-string all collapse to `"no reason given"`
  so a malformed call from any provider stays well-typed), reads
  `ctx.session_id` and **errors out when missing** because forced
  runs need a goal anchor for the `DreamRunStore` audit row, then
  builds `DreamContext { goal_id, session_id, transcript_dir,
  kairos_active: false, remote_mode: false }` and calls
  `runner.run_forced(&ctx).await`. `outcome_to_json` maps all six
  `RunOutcome` variants to a structured JSON envelope:
  `{ status: "completed" | "skipped" | "lock_blocked" | "errored"
  | "timed_out" | "escape_audit", reason: string, audit_run_id?:
  uuid, files_touched?: [string], holder_pid?: u32, error_message?:
  string }` — same surface across Anthropic / MiniMax / OpenAI /
  Gemini / DeepSeek / xAI / Mistral so the contract is provider-
  agnostic per memory rule `feedback_provider_agnostic.md`.
  `tool_def()` returns `ToolDef { name: "dream_now", description:
  "Force a memory consolidation pass now (bypasses time/session
  gates; lock gate honored).", parameters: { type: "object",
  properties: { reason: { type: "string", description: "Optional
  human-readable reason recorded in the audit row." }},
  additionalProperties: false } }`. `register_dream_now_tool(
  registry, runner, transcript_dir)` boot helper registers via
  `register_arc` for operator-side wiring. Module doc comment
  includes the 3-line main.rs hookup snippet (`let runner =
  build_runner(BootDeps { ... }).await?; if let Some(runner) =
  runner { register_dream_now_tool(&tool_registry, runner,
  transcript_dir); }`) for application after the user resolves
  their pre-existing main.rs dirty state. Tests verde: 9 inline
  (`tool_def_shape`, `call_with_reason_returns_completed`,
  `call_without_reason_uses_default`, `call_with_empty_reason_uses_default`,
  `call_with_non_string_reason_uses_default`, `call_without_session_id_errors`,
  `outcome_to_json_skipped_renders_gate`, `register_dream_now_tool_adds_to_registry`,
  `outcome_to_json_lock_blocked_renders_holder_pid`).
  nexo-dream cumulative: 64 tests verde (55 + 9). Capability-gate
  INVENTORY entry under `crates/setup/src/capabilities.rs` deferred
  as 80.1.c.b — gate id is `dream_now`, default deny outside
  `assistant_mode: true` bindings, alignment with Phase 16 binding-
  policy schema needed before write. Mirror leak: forced
  consolidation pattern from `claude-code-leak/src/services/autoDream/
  autoDream.ts:102-179` (`runAutoDream` callable directly when the
  manual trigger fires) + Phase 77.20 Sleep tool shape (single
  optional string arg + structured JSON response).
- **C4.a — sed_validator + path_extractor wire into
  `gather_bash_warnings`** (FOLLOWUPS A4.a). Two orphaned safety
  modules under `crates/driver-permission/` (`sed_validator.rs`
  696 LOC + 21 tests, `path_extractor.rs` 564 LOC + 12 tests)
  shipped during Phase 77.9 but only `bash_destructive::
  check_sed_in_place` reached the production permission decider
  through `mcp.rs::gather_bash_warnings` — the richer
  allowlist/denylist + command-aware path extraction were dead
  code. Fix extends `gather_bash_warnings` (`crates/driver-
  permission/src/mcp.rs:190-260`) to compose 4 advisory tiers in
  order: 1. destructive-command (existing), 2. sed-in-place
  shallow (existing), 3. **sed deep validator** — gated on first
  parsed token == `sed` (because `sed_command_is_allowed` returns
  `false` for any non-sed input), calls
  `sed_command_is_allowed(cmd, allow_file_writes=false)`, fires
  warning "sed expression outside the safe allowlist
  (line-printing or simple substitution); review for `e` (exec)
  or `w` (file-write) flags" when result is `false`, 4. **path
  extractor** — when first token classifies as a `PathCommand`
  via `classify_command()`, runs `parse_command_args` →
  `filter_out_flags` → `extract_paths(cmd, &filtered)` to surface
  paths the command touches, lists up to `MAX_LISTED=10` entries
  with `(N more)` suffix when over cap, prefixes with the command's
  `action_verb()` (e.g. "concatenate files from"). All tiers stay
  advisory — final allow/deny remains with the upstream LLM
  decider, preserved 100% provider-agnostic across Anthropic /
  MiniMax / OpenAI / Gemini / DeepSeek / xAI / Mistral (operates
  on the bash command string only, no LLM provider assumption).
  Buffer changed `Vec<&str>` → `Vec<String>` because new warnings
  are owned format strings; existing warnings clone via
  `to_string()` (only allocates on the rare warning-present path).
  Scope: only the first clause inspected — pipes / `&&` chains
  past the first command stay covered by the existing destructive
  check downstream. 4 inline tests in `mcp::tests`:
  `gather_bash_warnings_skips_non_bash` (FileEdit returns None),
  `gather_bash_warnings_returns_none_for_simple_sed`
  (`sed -n '1,5p' f.txt` is line-printing, deep validator must
  not fire), `gather_bash_warnings_flags_complex_sed`
  (`sed 's/foo/bar/e' file.txt` with the `e` exec flag triggers
  the deep warning), `gather_bash_warnings_lists_paths_for_classified_commands`
  (`cat /etc/passwd /etc/shadow` lists both paths via the path
  wire). Doc-comment on `gather_bash_warnings` documents the
  4-tier composition + scope + provider-agnostic guarantee +
  IRROMPIBLE refs to claude-code-leak `bashSecurity.ts` (composes
  the tiers in upstream UI prompt), `sedValidation.ts:247-301`
  (exact source pattern for `sed_command_is_allowed`),
  `pathValidation.ts:27-509` (command-aware path extraction).
  C4.b (sandbox heuristic wire) and C4.c (rate-limit
  `LlmError::QuotaExceeded` variant) remain open in FOLLOWUPS A4.
- Phase 80.1.b.b.b (MVP) — `nexo_dream::boot::build_runner` helper
  + `BootDeps` struct + `default_memory_dir` /
  `default_dream_db_path` path helpers (`crates/dream/src/boot.rs`,
  ~270 LOC + 7 unit tests). Operator calls `build_runner(deps)` once
  at startup; helper validates config, mkdirs memory_dir + state_root
  parent, opens `SqliteDreamRunStore` (shared
  `<state_root>/dream_runs.db`), constructs `ConsolidationLock`,
  builds `AutoDreamRunner` via `with_default_fork`. Returns
  `Ok(None)` when `config.enabled = false` (orchestrator stays
  clean — no per-turn cost). Mirrors leak `autoDream.ts:111-122`
  `initAutoDream()` startup pattern + Phase 77.5
  `ExtractMemories` boot construction shape. Provider-agnostic
  `BootDeps` carries `Arc<dyn LlmClient>` + `Arc<dyn ToolDispatcher>`
  trait objects — works under any provider impl per memory rule
  `feedback_provider_agnostic.md`. Module doc comment includes the
  3-line main.rs hookup snippet (`if let Some(ad_cfg) = ... let
  runner = build_runner(deps).await?; orchestrator_builder.auto_dream(runner)`)
  for operator-side application — main.rs change deferred until
  user resolves their pre-existing dirty state
  (`CronToolCallsConfig` + `Arc` import); the helper is fully
  testable + isolable in `nexo-dream` regardless of binary build
  state. nexo-dream cumulative: 55 unit tests verde (48 + 7).
- Phase 80.1.b.b (partial) — `AgentConfig::auto_dream:
  Option<AutoDreamConfig>` field with `#[serde(default)]` for
  backward-compat. 47 struct-literal test fixtures across 17
  directories swept via `perl -i -p0e` multi-line replace
  (anchor `repl: Default::default(),\n}`). 3 new YAML round-trip
  tests in `nexo-config` covering: missing block (None), present
  with `enabled: true`, present with `enabled: false`. Affected
  crates verde: nexo-config 153, nexo-fork 66, nexo-dream 48,
  nexo-driver-loop 104, nexo-driver-types 22, nexo-agent-registry
  38, nexo-core 671. main.rs boot wiring (~10K LOC binary; needs
  per-binding `parent_ctx_template` + `tool_dispatcher` plumbing
  audit) + Phase 18 hot-reload propagation hook deferred as
  80.1.b.b.b follow-up. The 80.1.b orchestrator integration is
  functional standalone — operators can wire AutoDreamRunner
  programmatically right now.
- Phase 80.1.b (MVP) — `nexo-driver-loop` orchestrator gains
  `auto_dream: Option<Arc<dyn AutoDreamHook>>` field + `.auto_dream(...)`
  builder + post-turn invocation site adjacent to Phase 77.5
  `extract_memories`. Mirrors leak `autoDream.ts:316-324` `executeAutoDream`
  invoked from `stopHooks`. New types in `nexo-driver-types::auto_dream`
  (`AutoDreamHook` trait + `AutoDreamOutcomeKind` enum +
  `DreamContextLite` struct) — placed upstream of nexo-driver-loop and
  nexo-dream to break the would-be `nexo-core ↔ nexo-dream` cycle.
  `nexo-dream` provides `impl AutoDreamHook for AutoDreamRunner` with
  `RunOutcome → AutoDreamOutcomeKind` lossy mapping (full row stays in
  80.18 `dream_runs`). `DreamContext` refactored: drops `parent_ctx`
  + `last_chat_request`, replaced by operator-supplied
  `parent_ctx_template` + `fork_system_prompt` + `fork_tools` +
  `fork_model` at `AutoDreamRunner::new` (mirror Phase 77.5 shape;
  no parent prompt-cache share). `AutoDreamConfig` moved to
  `nexo-config::types::dream` (cycle-free); `nexo-dream::config`
  re-exports + adds `validate()` helper. New `DriverEvent::AutoDreamOutcome
  { goal_id, outcome_kind }` event variant. Tests verde across
  nexo-config (150+), nexo-driver-types (1 new outcome_kind serde
  round-trip), nexo-dream (48), nexo-driver-loop (104+). main.rs boot
  wiring + `AgentConfig::auto_dream` field deferred to 80.1.b.b
  follow-up (adding the field breaks 30+ struct-literal fixtures
  across the workspace; needs coordinated sweep).
- Phase 80.1 (MVP) — `crates/dream/` foundation crate for autoDream
  fork-style memory consolidation. Verbatim port of leak
  `claude-code-leak/src/services/autoDream/{autoDream.ts:1-324,
  consolidationLock.ts:1-140, consolidationPrompt.ts:1-65}`. Mirrors
  the leak's per-turn-hook design (not cron-based — design audit
  caught and corrected). 8 modules + 49 unit tests verde:
  `error`/`config`/`consolidation_lock` (PID+mtime lock with
  symlink defense via canonicalize-at-construction, idempotent
  rollback, `HOLDER_STALE=1h`, `is_pid_running` via `nix::sys::signal::kill`)/
  `consolidation_prompt` (4-phase Orient→Gather→Consolidate→Prune
  template, `ENTRYPOINT_NAME=MEMORY.md`, `MAX_ENTRYPOINT_LINES=200`)/
  `dream_progress_watcher` (verbatim port of `makeDreamProgressWatcher`
  with bidirectional turn+files collection through 80.18 store +
  defense-in-depth escape detection)/`auto_dream` (`AutoDreamRunner`
  control flow with 7 gates per leak, force bypass, lock acquire/rollback,
  fork via `nexo_fork::DefaultForkSubagent` + `AutoMemFilter` (80.20),
  `tracing::info!` events with leak field names sans `tengu_` prefix).
  `RunOutcome` enum (Completed/Skipped/LockBlocked/Errored/TimedOut/
  EscapeAudit) is a nexo extension over leak's `Promise<void>` for
  CLI/LLM-tool feedback. Provider-agnostic via `Arc<dyn LlmClient>`
  per memory rule `feedback_provider_agnostic.md`. Driver-loop
  post-turn hook integration + `dream_now` LLM tool + `nexo agent
  dream` CLI + buffer pattern in `dreaming.rs` (D-1 coexistence with
  Phase 10.6 scoring) deferred as 80.1.b/c/d/e follow-ups.
- Phase 80.18 — `crates/agent-registry::dream_run` audit-log store
  for forked memory-consolidation runs. Verbatim port of leak
  `claude-code-leak/src/tasks/DreamTask/DreamTask.ts:1-158`. Mirrors
  Phase 72 turn-log pattern: `DreamRunStore` trait + `SqliteDreamRunStore`
  impl, schema migration v4 idempotent + 3 indexes, MAX_TURNS=30
  server-side cap, TAIL_HARD_CAP=1000 defends `tail(usize::MAX)`,
  JSON columns for `files_touched` + `turns` avoid join tables.
  `Option<i64>` for `prior_mtime_ms` distinguishes `Some(0)` (no prior
  consolidation file marker for autoDream) from `None` (non-lock-holding
  forks like AWAY_SUMMARY 80.14). `fork_label: String` flexible —
  supports autoDream + AWAY_SUMMARY + future Phase 51 eval forks.
  Provider-agnostic: `DreamTurn { text, tool_use_count }` plain Rust,
  no `LlmClient` coupling. 26 unit tests including idempotent insert
  on (goal_id, started_at), trim cap proof (insert 35 → final 30),
  reattach `Running → LostOnRestart` flip, drop_for_goal isolation,
  prior_mtime zero-vs-none round-trip. Phase 71 reattach integration
  deferred to 80.18.b follow-up.
- Phase 80.20 — `crates/fork::AutoMemFilter` tool whitelist for
  forked memory-consolidation work. Verbatim port of leak
  `claude-code-leak/src/services/extractMemories/extractMemories.ts:165-222`
  (`createAutoMemCanUseTool`). Allows `REPL` (cache-key parity per
  leak `:171-180`), `FileRead`/`Glob`/`Grep` (read-only), `Bash` only
  when `nexo_driver_permission::is_read_only(command)` returns true,
  `FileEdit`/`FileWrite` only when `file_path` (post-canonicalize)
  starts with the filter's `memory_dir`. Path canonicalize at
  construction + per-call defeats symlink swaps and `..` traversal.
  New helper `nexo_driver_permission::bash_destructive::is_read_only`
  composes Phase 77.8/77.9 classifiers + a positive whitelist of ~45
  read-only utilities; intentionally drops `tee`/`awk`/`perl`/`python`/
  `node`/`ruby` from the whitelist because they can shell out via
  `system(...)`. Provider-agnostic — operates on tool name + JSON
  args; expects flat top-level args (provider clients unwrap nested
  envelopes before dispatch). 24 new unit tests in `auto_mem_filter`
  + 19 new in `bash_destructive::is_read_only`. Decisions D-1+R3 in
  `proyecto/design-kairos-port.md` (conservative whitelist, fail-fast
  on missing dir, defense-in-depth via post-fork audit in 80.1).
  Consumed by Phase 80.1 autoDream + 80.14 AWAY_SUMMARY + future
  Phase 51 eval harness.
- Phase 80.19 — `crates/fork/` fork-with-cache-share subagent
  infrastructure (KAIROS port). Standalone in-process turn loop
  using `nexo_llm::LlmClient` directly (NOT Phase 67's heavyweight
  `DriverOrchestrator`, which spawns `claude` subprocesses).
  `CacheSafeParams::from_parent_request(&ChatRequest)` snapshots
  parent prompt + tools + model + message prefix; preserves any
  incomplete `tool_use` blocks bit-for-bit (leak invariant
  `forkedAgent.ts:522-525`). `DelegateMode::{Sync,ForkAndForget}`,
  `ForkHandle::take_completion()` + `Drop`-cancels-abort prevents
  leaked tokio tasks on abandoned ForkAndForget handles. `OnMessage`
  trait with `NoopCollector` / `LoggingCollector` / panic-safe
  `ChainCollector`. `ToolFilter` trait + `AllowAllFilter` default
  (Phase 80.20 ships `AutoMemFilter`). `tracing` span
  `fork.subagent` with run_id + cache_key_hash + mode; inline
  `WARN fork.cache_break_detected` when first-turn cache hit ratio
  drops below 0.5 (Phase 77.4 heuristic). 42 unit tests pass on
  `cargo test -p nexo-fork`. Decisions D-8 in
  `proyecto/design-kairos-port.md`. Consumed by Phase 80.1
  autoDream + 80.14 AWAY_SUMMARY + future Phase 51 eval harness.
  Refactor of `delegation_tool.rs` to consume the new infra is
  follow-up 80.19.b (out of scope for 80.19 itself).
- Phase 27.1 — `cargo-dist` baseline. `dist-workspace.toml` declares
  the cross-target matrix (`x86_64-unknown-linux-gnu` host fallback +
  `x86_64`/`aarch64-unknown-linux-musl` + `x86_64`/`aarch64-apple-darwin`
  + `x86_64-pc-windows-msvc`). `make dist-check` runs the local smoke
  gate (`scripts/release-check.sh`) over whatever `dist build`
  produced, validating tarball contents + sha256 + host-native
  `--version`. `nexo version` (or `nexo --version --verbose`) prints
  build provenance — git-sha, target triple, build channel, build
  timestamp — captured at compile time by `build.rs` and consumed
  via `env!("NEXO_BUILD_*")`. Dev-only programs (`browser-test`,
  `integration-browser-check`, `llm_smoke`) moved to `examples/` so
  cargo-dist excludes them from release tarballs. `release-plz`
  remains the source of truth for version bumps + crates.io publish
  + per-crate `CHANGELOG.md`. Operator notes:
  [`packaging/README.md`](packaging/README.md), contributor docs:
  [Releases](docs/src/contributing/release.md).
- `agent admin` subcommand: runs a web admin UI behind a Cloudflare
  quick tunnel. Auto-installs `cloudflared` per OS/arch on first run,
  starts a loopback HTTP server, mints a fresh 24-char random
  password per launch, and prints a new `https://…trycloudflare.com`
  URL every time. HTTP Basic Auth (`admin` / `<password>`) gates
  every request. Serves the React + Vite + Tailwind bundle from
  `admin-ui/` embedded at Rust compile time via `rust-embed`. See
  [CLI reference — admin](https://lordmacu.github.io/nexo-rs/cli/reference.html#admin).
- `admin-ui/` scaffold (React 18, Vite 5, TS 5, Tailwind 3). First
  page is a minimal "hello" layout; the full admin surface (agent
  directory, DLQ, live reload, config editor) lands in follow-ups.
  `scripts/bootstrap.sh` runs `npm install && npm run build`
  automatically when `npm` is on PATH.
- Native / no-Docker install path: `docs/src/getting-started/install-native.md` +
  idempotent `scripts/bootstrap.sh` (Linux, macOS, Termux).
- Termux (Android) support:
  - Dedicated install guide `docs/src/getting-started/install-termux.md`
    with root-vs-non-root breakdown.
  - `bootstrap.sh` detects `$TERMUX_VERSION` / `$PREFIX` and branches:
    `pkg install rust`, `$PREFIX/bin`, defaults NATS to `skip` with
    a hint toward `broker.type: local`.
- `BrowserConfig.args: Vec<String>` forwards extra CLI flags to the
  spawned Chrome/Chromium (enables `--no-sandbox` etc. for Termux).
- Repository chrome: `SECURITY.md`, `CODE_OF_CONDUCT.md`,
  `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.{md,yml}`,
  `.github/PULL_REQUEST_TEMPLATE.md`.
- Documentation site (mdBook) published at
  <https://lordmacu.github.io/nexo-rs/> with every subsystem
  documented, Mermaid diagrams, 9 ADRs under `docs/src/adr/`, and
  5 end-to-end recipes under `docs/src/recipes/`.
- Pre-commit docs-sync gate in `.githooks/pre-commit` rejects
  production-file changes without accompanying `docs/` edits unless
  the commit message includes `[no-docs]`.
- CI: `.github/workflows/docs.yml` builds mdBook + rustdoc and
  deploys to GitHub Pages; broken local-link scan.

### Changed

- Dual-licensed `MIT OR Apache-2.0` with an enforceable `NOTICE`
  attribution block (ADR 0009).
- `README.md` rewritten with badges and deep links into the
  published documentation.
- **C5** — Operators can now configure the secret-scanner via
  `memory.secret_guard` in `config/memory.yaml`. The 4 knobs
  (`enabled`, `on_secret: block|redact|warn`, `rules: "all" |
  [rule_id...]`, `exclude_rules: [rule_id...]`) replace the two
  hardcoded `SecretGuardConfig::default()` call sites in
  `src/main.rs` (daemon + mcp-server boot). Schema lived in
  `crates/memory/src/secret_config.rs` since Phase 77.7; C5
  finishes the wire. **Pivot from initial spec**: a direct
  `nexo-config -> nexo-memory` dep would form a cycle
  (`nexo-llm -> nexo-config -> nexo-memory -> nexo-llm`). Fix
  uses a wire-shape struct (`SecretGuardYamlConfig`) in
  `crates/config/src/types/memory.rs` that mirrors the canonical
  schema 1:1; the conversion lives in
  `src/main.rs::build_secret_guard_config_from_yaml` (binary
  holds both deps). Doc-comment flags the dual-write contract.
  Default applies when YAML omits the key — back-compat 100%.
  Invalid `on_secret` or malformed `rules` fail boot loud — never
  silent. Provider-agnostic — `exclude_rules` operates on rule
  IDs (kebab-case), not providers; scanner covers Anthropic /
  MiniMax / OpenAI / Gemini / DeepSeek / xAI / Mistral with the
  same regex set. Pattern adopted from OpenClaw's enum-mode YAML
  knob (`research/src/config/zod-schema.ts`); claude-code-leak
  `src/services/teamMemorySync/secretScanner.ts:48` ships
  hardcoded with no operator override, validating the value of
  adding one. Schema duplication tracked as A5.b deferred
  follow-up (migration to a shared types crate).
- **M5.b** — Cron config-reload post-hook reactivates the
  `replace_bindings` API shipped in M5 step 1. New free function
  `build_cron_bindings_from_snapshots` (with `compute_binding_key`
  and `compute_inbound_origin` helpers) is the single source of
  truth used by both boot path and the post-hook. Aggregated
  `tools_per_agent: Arc<HashMap<agent_id, Arc<ToolRegistry>>>` and
  `agent_snapshot_handles: Arc<HashMap<agent_id, Arc<ArcSwap<RuntimeSnapshot>>>>`
  populated during the boot agent loop carry the per-agent state
  the post-hook needs to read fresh effective policy after each
  reload. `Arc<tokio::sync::OnceCell<Arc<RuntimeCronToolExecutor>>>`
  late-bind pattern (mirror of the Phase 79.10.b reload_cell at
  `src/main.rs:1923-1925`) handles the boot-time race where a
  reload could fire before the cron executor is built (no-op with
  `tracing::debug!`). Closes the C2 follow-up (FOLLOWUPS H-3.b):
  per-binding policy changes (`team.max_*`, `lsp.languages`,
  `repl.allowed_runtimes`, `config_tool.allowed_paths`, etc.) now
  apply to cron firings on the very next call after reload,
  without daemon restart. The `dead_code` warning on
  `replace_bindings` from M5 step 1 is resolved. Provider-agnostic
  — `effective.model` carries whatever provider the new snapshot
  resolved (Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI
  / Mistral); the rebuild function never branches on provider.
  Pattern validated against claude-code-leak
  `src/utils/cronScheduler.ts:441-448` (chokidar reload) + OpenClaw
  `research/src/cron/service/timer.ts:709` (forceReload per-tick);
  ArcSwap gives lock-free swap structurally rather than
  imperatively. Limitation: agent add/remove still requires daemon
  restart (Phase 19 scope — `tools_per_agent` and
  `agent_snapshot_handles` are populated during boot and never
  extended). Full integration test with a real
  `ConfigReloadCoordinator::reload()` deferred as M5.c.
- **M5 (partial — infra)** — `RuntimeCronToolExecutor.by_binding`
  migrates from `Arc<HashMap>` (immutable post-construction) to
  `Arc<arc_swap::ArcSwap<HashMap<...>>>`, enabling lock-free atomic
  hot-swap of the per-binding context map via the new
  `replace_bindings(new_map)` API. `resolve_binding` now returns
  owned `Option<CronToolBindingContext>` (cheap clone — fields are
  `Arc<_>` underneath); ArcSwap does not expose stable references
  across swaps. In-flight cron firings retain their pre-swap
  snapshot until completion. Two smoke tests cover the swap
  mechanics. The actual config-reload post-hook wire that exercises
  `replace_bindings` is deferred as **M5.b** in `FOLLOWUPS.md`
  (~30-45 min: extract `build_cron_bindings_from_snapshots` free
  fn + `CronRebuildDeps` + aggregate `tools_per_agent` /
  `agent_snapshot_handles` maps + register post-hook via
  `OnceCell` late-bind pattern, mirroring
  `src/main.rs:3499-3508` Phase 79.10.b). Pattern validated
  against claude-code-leak `src/utils/cronScheduler.ts:441-448`
  (chokidar reload + inFlight Set) and OpenClaw
  `research/src/cron/service/timer.ts:709,697` (forceReload
  per-tick + long-job pitfall); we use ArcSwap which gives
  lock-free swap structurally rather than imperatively.
  Provider-agnostic — the executor + binding map are wire-level
  cross-provider (Anthropic / MiniMax / OpenAI / Gemini /
  DeepSeek / xAI / Mistral).
- **M9** — Regression guard against silent renames in
  `mcp_server.expose_tools`. New
  `crates/core/tests/expose_tools_typo_regression_test.rs`
  maintains a hardcoded `KNOWN_CANONICAL_NAMES_SNAPSHOT` (33
  entries baseline) bidirectionally synced with `EXPOSABLE_TOOLS`.
  Three tests catch silent renames or removals (operator YAML
  with old names would degrade to a `tracing::warn!` at
  `src/main.rs:9261-9269` and silently drop), force snapshot
  updates when the catalog grows, and sanity-check for
  merge-conflict duplicates. Failure messages enumerate explicit
  fix paths (restore catalog / drop snapshot / add deprecated
  alias as M9.b follow-up). Provider-agnostic — `EXPOSABLE_TOOLS`
  is the wire-spec MCP catalog, identical regardless of which
  LLM client (Claude Desktop / Cursor / Continue / Cody / Aider)
  or backing provider (Anthropic / MiniMax / OpenAI / Gemini /
  DeepSeek / xAI / Mistral) drives the call. Pattern adopted
  from OpenClaw `research/src/channels/ids.test.ts:48-50`
  snapshot assertion; claude-code-leak `src/tools.ts:193-251`
  has no equivalent guard, validating the value of adding one.
- **M2** — MCP audit log `tools/call` rows now record the real
  `args_hash` (sha256 truncated to 16 lowercase hex chars / 64 bits)
  and `args_size_bytes` (JSON-serialized byte length) instead of the
  placeholder `None`/`0`. Honors the existing
  `audit_log.redact_args` (default `true`),
  `audit_log.per_tool_redact_args` (per-tool override wins over
  global), and `audit_log.args_hash_max_bytes` (default 1 MiB, hard
  ceiling 16 MiB) knobs — none of those YAML keys change. New
  internal module `crates/mcp/src/server/audit_log/hash.rs` exposes
  the helpers as `pub(crate)`. `SELECT args_hash, COUNT(*) FROM
  mcp_call_audit GROUP BY args_hash` correlation queries now return
  real data; the SQLite schema is unchanged. Provider-agnostic —
  operates on the MCP wire envelope, identical regardless of which
  LLM client (Claude Desktop / Cursor / Continue / Cody / Aider) or
  backing provider (Anthropic / MiniMax / OpenAI / Gemini /
  DeepSeek / xAI / Mistral) drives the call. Truncation length
  matches the prior-art pattern in claude-code-leak
  (`src/services/mcp/utils.ts:157-168` `hashMcpConfig` + 4 other
  sites all `slice(0, 16)`).
- **C3** — `crates/setup/src/capabilities.rs::INVENTORY` extended
  from 9 → 12 entries closing the audit drift. New entries:
  `CHAT_AUTH_SKIP_PERM_CHECK` (auth-wide file-perm-gauntlet bypass,
  High), `NEXO_CLAUDE_CLI_VERSION` (Anthropic OAuth Bearer CLI
  version stamp override, Low), and `config-self-edit` Cargo feature
  (gates the self-config-editing ConfigTool, Critical). New
  `ToggleKind::CargoFeature(&'static str)` variant supports
  compile-time gates alongside runtime env-var toggles. Module
  doc-comment expanded with provider-agnostic clause: every LLM
  provider (Anthropic, MiniMax, OpenAI, Gemini, DeepSeek, xAI,
  Mistral, future) gets its own entries when it introduces a
  dangerous toggle (insecure-tls, skip-ratelimit, allow-write); the
  `extension` field already accepted any identifier. New regex-based
  drift-prevention test (`inventory_covers_known_dangerous_envs`)
  walks `crates/**/*.rs` and fails when an `env::var("X")` literal
  is neither in INVENTORY nor in `NON_DANGEROUS_ENV_ALLOWLIST` —
  surfaced 13 previously-unclassified env reads (all benign,
  classified into the allowlist with category comments). Implementation
  100% Rust (`cfg!`, const slice, `walkdir + regex` dev-deps); the TS
  references (claude-code-leak `envUtils.ts` + `commands/doctor/`,
  OpenClaw `auth-profiles/doctor.ts`) guided pattern, not code.
- **C2** — Hot-reload now picks up per-binding policy overrides for
  `lsp.languages` / `lsp.idle_teardown_secs`, `team.max_*` /
  `team.worktree_per_member`, `repl.allowed_runtimes`, and the
  C1-added inheritance for the four resolved fields. Tool handlers
  (`LspTool`, `ReplTool`, `TeamCreateTool`/`TeamDeleteTool`/
  `TeamSendMessageTool`/`TeamListTool`/`TeamStatusTool`) read
  policy from `ctx.effective_policy().<x>` per call instead of
  capturing it at `Tool::new`. Reload semantics: a snapshot swap
  via `ConfigReloadCoordinator` is observed on the very next
  intake event without restart. **Boolean enable flips** (e.g.
  `lsp.enabled: false → true`) still require restart — see
  `docs/src/ops/hot-reload.md::What's reloaded` for the full
  matrix. Subsystem actor lifecycle (LspManager child processes,
  ReplRegistry subprocess pool, TeamMessageRouter broker subs)
  is unchanged across reload, matching the prior-art pattern
  from claude-code-leak's MCP `useManageMCPConnections` invalidate-
  and-refetch. Implementation is 100% Rust idiomatic
  (`Arc<EffectiveBindingPolicy>` lookups, `ArcSwap<RuntimeSnapshot>`
  swap, `From` trait adapters); the TS references guided the
  pattern, not the code. Two follow-ups tracked in `FOLLOWUPS.md`:
  H-3.b (M5 — `cron_tool_bindings` registry captured at boot) and
  H-3.c (M11 — full ConfigTool config-pull at handler entry).
- **C1** — `EffectiveBindingPolicy` now resolves four additional
  per-binding overrides (`lsp`, `team`, `config_tool`, `repl`) using
  the same replace-whole strategy as `proactive` / `remote_triggers`.
  **Behavioural change**: configs that already declared
  `inbound_bindings[].repl: { ... }` will start applying it — the
  override field had been declared in Phase 79.12 but the resolver
  was missing, silently inheriting the agent-level value. Three new
  optional fields (`lsp`, `team`, `config_tool`) added to
  `InboundBinding`; defaults inherit, so pre-existing YAML is
  unaffected. `binding_validate::has_any_override` extended to
  count the seven previously-uncounted overrides
  (`plan_mode` / `role` / `proactive` / `repl` / `lsp` / `team` /
  `config_tool`); this fixes the misleading `binding without
  overrides` warning. The actual consumption of the new resolved
  fields by tool-registration paths in `src/main.rs` remains
  pinned to boot — runtime hot-reload of these policies is
  tracked under C2.

### Deprecated

_Nothing yet._

### Removed

_Nothing yet._

### Fixed

- Setup wizard no longer hardcodes a shared `whatsapp.session_dir`
  — the writer derives a per-agent path when the YAML field is
  empty, avoiding cross-agent session collisions.
- Extension tools are gated on `Requires::missing()`: if declared
  `bins` / `env` aren't available, the extension is skipped with a
  warn log instead of registering tools that fail every call.

### Security

- `SECURITY.md` formalizes the private disclosure channel
  (<informacion@cristiangarcia.co>) and sets expected response SLAs.

---

## [0.1.0] — 2026-04-24 (initial public release)

First public cut of the codebase. All 16 internal development
phases complete (120/120 sub-phases in `PHASES.md`). No backward-
compatibility commitments yet — treat the public surface as unstable
until `v1.0.0`.

<!-- Link definitions:
[Unreleased]: https://github.com/lordmacu/nexo-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lordmacu/nexo-rs/releases/tag/v0.1.0
-->
