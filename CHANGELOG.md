# Changelog

All notable changes to this project are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project adheres to [Semantic Versioning](https://semver.org)
**once `v1.0.0` is tagged**. Until then breaking changes may land on
`main` between any two commits; see the commit history for detail.

## [Unreleased]

### Added

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
