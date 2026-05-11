# Follow-ups

This file tracks the **active technical backlog** in English.

### Cody mapping deep-dive 2026-05-11 — Phase A deudas — shipped

Audit-driven 5-fix wave on Cody's surrounding infrastructure
(deuda surfaced by exhaustive mapping of "Cody = chat-only?
no, programmer-pair dispatching Claude Code subprocesses via
~37K LOC of Phases 67/70-74 substrate"). All fixes are
framework-generic improvements; they happen to make Cody more
honest, not Cody-specific.

A (5/5 shipped 2026-05-11):
- ✅ ~~**A.1 add_hook + remove_hook handlers**~~ —
  `crates/core/src/agent/dispatch_handlers.rs` — declared in
  WRITE_TOOL_NAMES + referenced by Cody's system prompt but
  never registered. Calls fell through as "unknown tool".
  Bridged straight to `HookRegistry::add_unique` (idempotent
  duplicate-id rejection with `reason` field) and
  `HookRegistry::remove` (probe-then-remove pattern returns
  `removed: false` instead of erroring on missing). Goal id
  parsed via shared `parse_goal_id(&str) -> Result<GoalId>`
  helper. 5 e2e tests asserting attach + duplicate idempotency
  + empty-id reject + remove flow + invalid-uuid reject.
- ✅ ~~**A.2 register chain + parallel handlers**~~ —
  `program_phase_chain` + `program_phase_parallel` functions
  existed in `nexo-dispatch-tools::chain.rs` but
  `register_dispatch_tools_into` never exposed them as
  handlers. Wired ProgramPhaseChainHandler +
  ProgramPhaseParallelHandler with self-modify guards
  matching the existing ProgramPhaseHandler. Chain handler
  binds the synthesised `chain_hooks` to the freshly-spawned
  goal via `dispatch.hooks.add_unique` so subsequent phases
  fire when the previous one's Done transition lands.
- ✅ ~~**A.3 PreflightHandler.llm_ready uses LlmRegistry**~~ —
  was hardcoded `provider == "anthropic" || provider == "minimax"`,
  giving DeepSeek/Gemini/OpenAI agents an engaging-but-false
  `llm_ready: false`. Threaded the daemon's shared
  `Arc<LlmRegistry>` (Phase 90 P1.4 follow-up) into
  `DispatchToolContext.llm_registry: Option<Arc<LlmRegistry>>`,
  preflight now consults `reg.names().iter().any(|n| n == provider)`.
  Falls back to legacy hardcode when registry unwired (test
  contexts). `boot_dispatch_ctx_if_enabled` parameter list
  extended to thread the Arc from main.rs.
- ✅ ~~**A.4 self-modify env-var name reconcile**~~ —
  `dispatch_handlers.rs:178` error msg said
  `NEXO_ALLOW_SELF_MODIFY=1` to enable but `src/main.rs:7317`
  reads `NEXO_DISALLOW_SELF_MODIFY` (default ON, env var
  DISABLES). Operator following error msg exported wrong env
  var. Doc string also mis-described the default as `false`.
  Fixed both — message + docstring now reflect production
  reality (default ON + `NEXO_DISALLOW_SELF_MODIFY=1` flips
  off for production / frozen-binary deploys).
- ✅ ~~**A.5 AnthropicAuth identity spoof opt-out**~~ —
  `crates/llm/src/anthropic.rs::prepend_claude_code_spoof` was
  always-on for Bearer auth (OAuth + setup-token). Microapp
  future using Anthropic API plain would identify falsely as
  Claude Code. Added `should_spoof_claude_code()` helper
  reading `NEXO_ANTHROPIC_NO_CLAUDE_CODE_SPOOF` env (default
  OFF — spoof stays ON). 4 unit tests covering default-on +
  truthy-value-aliases + empty/unrelated-keep-on +
  build_body_skips-when-set, all serialised via static Mutex
  guard (env is process-global). New INVENTORY entry in
  `crates/setup/src/capabilities.rs` so `agent doctor
  capabilities` reports the toggle with Risk::Medium effect
  description warning operators about subscription terms
  before disabling.

A totals: 14 new tests (4 anthropic + 5 add_hook/remove_hook
+ 5 reused/extended for chain+parallel registration), 0
regressions, full workspace build clean.

Phase B (extract `nexo-persona-cody/` out-of-tree config repo)
queued — separate forge brainstorm/spec/plan flow per audit
recommendation.

### Audit 2026-05-10 — admin wave P0 fixes — shipped

Comprehensive audit of all admin work shipped in the
2026-05-10 wave (Phase 90.x.memory-snapshot.create-restore
+ Phase 81.21.b.b auto-respawn + manual restart RPC + cell
wiring + uptime telemetry + Phase 27.2 capability inventory).
Four parallel agents reviewed adapters, frontend, lifecycle
events + capability gating, and test coverage. P0 = data
corruption / silent multi-tenant / build-impacting bugs.

P0 (6/6 shipped):
- ✅ ~~**P0.1 HttpError surfaces daemon body**~~ —
  `nexo-rs-plugin-admin/frontend/src/api/client.ts` — error
  body now reaches stores via `.message` instead of
  collapsing to literal `HTTP <status>`. Boot-window
  ("plugin handles not yet populated"), restart timeout,
  snapshot tenant-mismatch, encryption errors all visible
  in UI. 8 unit tests in `tests/api/http-error.test.ts`.
- ✅ ~~**P0.2 memory store reads activeTenantId**~~ —
  `frontend/src/store/memory.ts` — list / create / delete
  no longer hardcode `tenant:"default"`. New `currentTenant()`
  helper reads `useTenantStore.getState().activeTenantId`
  at call time so rail switches take effect immediately.
  `runRestore` keeps caller-supplied tenant precedence so
  `RestoreSnapshotModal` can defend against mid-flight
  switches by passing `snapshot.tenant`. 6 unit tests in
  `tests/store/memory-tenant.test.ts`.
- ✅ ~~**P0.3 + P0.4 delete idempotency + cross-tenant guard**~~ —
  `crates/setup/src/admin_adapters.rs::LiveMemorySnapshotReader::delete`
  — typed match on `SnapshotError::NotFound` → `Ok(())`
  satisfies the trait's idempotency contract (concurrent
  delete + stale UI list no longer surface as `Internal -32603`).
  Added defense-in-depth tenant guard mirroring `restore()`:
  `list+find+meta.tenant assert` before reaching disk. Mock
  `delete()` enriched to track `delete_calls` + return
  `NotFound` when id missing. 3 new tests
  (`live_delete_happy_path_removes_bundle`,
  `live_delete_is_idempotent_on_missing_id`,
  `live_delete_rejects_tenant_mismatch`).
- ✅ ~~**P0.5 concurrent restart race mutex**~~ —
  `crates/core/src/agent/nexo_plugin_registry/subprocess.rs`
  — added `restart_lock: Arc<tokio::sync::Mutex<()>>` field
  on `SubprocessNexoPlugin`. `force_restart()` acquires it
  via `lock_owned().await` before the 11-step cascade so
  two simultaneous operator restarts serialize cleanly
  instead of orphaning one's child. New
  `concurrent_force_restart_serializes_via_restart_lock`
  test asserts both calls succeed, spawn distinct PIDs,
  and publish exactly two `restarted_manually` events.
  Existing 4 force_restart tests remain green.
- ✅ ~~**P0.6 lifecycle subject_prefix configurable**~~ —
  `crates/memory-snapshot/src/events.rs` — added
  `subject_with_prefix(&str)` helpers on `LifecycleEvent`
  + `MutationEvent` (back-compat: `subject()` calls them
  with the const default). `BrokerEventPublisher` in
  `proyecto/src/main.rs` now holds `lifecycle_prefix` +
  `mutation_prefix` from `EventsSection` and uses them on
  every publish, fixing the silently-dead YAML knob
  (`memory.snapshot.events.lifecycle_subject_prefix` /
  `mutation_subject_prefix`). 2 unit tests asserting the
  configured prefix wins over `LIFECYCLE_SUBJECT_PREFIX`.

P1 (7/7 shipped 2026-05-11):
- ✅ ~~**P1.1 memory-snapshot.md events table**~~ — fixed 4
  bogus payload shapes (gc/deleted/created/restored). Subjects
  + payloads now match actual wire (kind discriminator + post-
  flatten field list). Subscribers writing schema-strict
  consumers no longer silently fail to parse.
- ✅ ~~**P1.2 admin-rpc.md capability table**~~ — added 33
  missing method/capability rows incl. `plugin_restart` shipped
  today. Operators deciding what to grant now have a complete
  reference (57 live methods across 17 capabilities).
- ✅ ~~**P1.3 handler maps "not yet populated" to InvalidParams**~~ —
  `domains/plugin_restart.rs:61` regex extended. Boot-window
  race ("plugin handles not yet populated; daemon still
  booting") now classified user-recoverable so the SPA can
  render a transient retry toast instead of the generic 500
  modal. New `restart_plugin_maps_not_yet_populated_to_invalid_params`
  test asserts the -32602 code.
- ✅ ~~**P1.4 LlmRegistry shared cell**~~ — single
  `Arc<LlmRegistry>` constructed once at main.rs:1958, shared
  across `LivePluginRestarter`, `RegistryLlmCompleter`, the
  boot-time provider catalog snapshot, AND the daemon main
  runtime registry (was rebuilt at main.rs:2357 — now removed,
  shadow re-bind kept for clarity). Closes the silent-divergence
  gap if a plugin-registered LLM factory ever lands between
  the two prior construction sites.
- ✅ ~~**P1.5 restore-applied report cleanup race**~~ —
  `RestoreSnapshotModal` now captures the apply-success report
  into local `appliedReport` state (instead of relying on the
  store's `lastRestoreReport` which the unmount cleanup nukes).
  Modal switches to a "Done" view rendering the full
  RestoreReportTable + a Close button that triggers the parent
  onApplied (close + refresh). Operator finally sees the
  pre_snapshot_id, git_reset_oid, and restored DB list. 3 new
  i18n keys (`done_title`, `done_intro`, `close`) en+es.
- ✅ ~~**P1.6 lastRestartReport UI surfacing**~~ — `PluginsMain`
  renders the `lastRestartReport` from `usePluginsDoctor` as a
  dismissible success banner above the loaded plugins list
  showing `plugin_id` + `previous_uptime_ms` + `new_pid` +
  `restarted_at` (ISO timestamp). 5 new i18n keys
  (`plugins.restart.report.{banner,previous_uptime,new_pid,restarted_at,dismiss}`)
  en+es. Dismissible via X button → `clearLastRestartReport()`.
- ✅ ~~**P1.7 plugin.toml declares plugin_restart + memory_snapshot**~~ —
  cross-repo: `nexo-rs-plugin-admin/plugin.toml` declares both
  capabilities under `[plugin.capabilities.admin]::required` so
  the daemon refuses to spawn the plugin until the operator
  grants them (fail-fast). Otherwise the Restart button +
  snapshot create/restore landed -32004 capability_not_granted.

P1 totals: 1 new backend test, 0 frontend tests added (pure UX
+ store changes verified manually + tsc clean), full mdbook
build clean, full workspace build clean.

P2 (12/12 shipped 2026-05-11):
- ✅ ~~**P2.1 supervisor.md doc drift**~~ — events table now
  includes `restarted_manually` row + clarified
  `total_uptime_ms` is real (was always 0); `gave_up.last_exit_code = -1`
  spawn-failure sentinel documented; "no manual restart RPC"
  stale follow-up bullet removed (RPC shipped today).
- ✅ ~~**P2.2 lifecycle golden expansion + `new_pid` assert**~~ —
  broker payload now includes `new_pid` (was only in
  `PluginsRestartResponse` wire shape). New
  `lifecycle_payload_shape_restarted_manually` golden asserts
  source / plugin_id / previous_uptime_ms / restarted_at_ms /
  new_pid present + no extraneous fields. Existing
  `force_restart_publishes_restarted_manually_event` extended to
  match `report.new_pid` against the published payload.
- ✅ ~~**P2.3 manifest validation gaps**~~ — added 3
  `ManifestError` variants + bounds:
  `SupervisorMaxAttemptsZero` (only enforced when respawn=true),
  `SupervisorBackoffMsBelowFloor` (min 100ms — prevents tight
  retry loops bypassing exponential schedule),
  `SupervisorBackoffMsExceedsCap` (max 300_000ms — keeps
  reset-counter heuristic meaningful). 5 new tests including
  the regression net for the `respawn=false` skip path.
- ✅ ~~**P2.4 capability-gate denial tests for 7 verbs**~~ —
  `dispatcher.rs::tests` mirrors
  `dispatch_tenants_list_denies_when_capability_not_granted`
  for memory/{query,list_snapshots,delete_snapshot,
  create_snapshot,restore_snapshot} + plugins/{doctor,restart}.
  Each asserts `capability` field on `CapabilityNotGranted -32004`
  matches the `capability_for_method` mapping. Shared
  `assert_capability_gate_denies` helper. 7 tests.
- ✅ ~~**P2.5 dispatcher slot-not-wired tests for 7 verbs**~~ —
  parallel coverage: with capability granted but the slot
  field still `None`, the dispatcher returns `Internal -32603`
  whose message contains the documented `<domain> domain not
  configured` substring. Microapps can detect wire-up gaps
  reliably. Shared `assert_slot_not_wired_internal_contains`
  helper. 7 tests.
- ✅ ~~**P2.6 domain kill-switches implemented**~~ — operator
  can now export `NEXO_MICROAPP_ADMIN_<DOMAIN>_ENABLED=0`
  (any of agents/credentials/pairing/llm_keys/channels/skills/
  tenants/secrets/auth) and the matching capability is
  stripped from every microapp's grant set BEFORE the
  dispatcher CapabilitySet is built — the verb returns
  `CapabilityNotGranted -32004` regardless of operator-edited
  YAML. New `apply_admin_domain_kill_switches` helper +
  `ADMIN_DOMAIN_KILL_SWITCHES` table (9 entries) +
  `tracing::warn!` per stripped grant. Closes the silent
  operator-misleading bug where INVENTORY entries reported
  "disabled" but had zero functional effect. 7 tests
  including the off-value-aliases sweep + the inventory-
  matches-INVENTORY contract test.
- ✅ ~~**P2.7 i18n drift test**~~ — runtime guard against type-
  widening drift in en/es catalogs. Asserts identical key sets
  via Set diff, identical key counts, all wave 90.x audit-fix
  keys present in both, all values non-empty. 4 vitest
  assertions in `tests/i18n-drift.test.ts`.
- ✅ ~~**P2.8 confirm-prefix shared helper**~~ — new
  `frontend/src/lib/confirmPrefix.ts` exposes
  `confirmPrefix(id, n=8)` + `confirmPrefixMatches(typed, id, n)`.
  RestartPluginModal + RestoreSnapshotModal both refactored
  to use it. Helper handles the short-id edge case the
  `Math.min(8, len)` defensive line was guarding against.
  11 vitest assertions for the helper.
- ✅ ~~**P2.9 modal Escape + backdrop-close**~~ — new
  `frontend/src/lib/useDialogClose.ts` exposes `useEscapeKey`
  + `useBackdropClose` hooks. Wired into all 4 modals
  (CreateSnapshot, RestoreSnapshot, RestartPlugin,
  TenantCreate). Disabled mid-flight so a stray click
  during a destructive RPC can't lose spinner + error
  state. RestoreSnapshotModal's hook routes to
  `handleDoneClose` when the post-apply view is showing
  (so dismissing fires the same refresh path as the Close
  button).
- ✅ ~~**P2.10 snapshot list expand**~~ — `MemoryMain` now
  defaults to the 5 most recent snapshots with a
  "Show {count} more" button when more exist; expanding
  shows all + a "Collapse" button. Operators with > 5
  snapshots can restore / delete older bundles from the
  UI without dropping to CLI. 2 new i18n keys
  (`memory.snapshots.show_all`, `memory.snapshots.collapse`)
  en+es.
- ✅ ~~**P2.11 PluginsDoctor 5/9 fields render**~~ — the
  `PluginDiscoveryReport` fields previously stuck behind
  `agent doctor plugins --json` now surface in the UI:
  duplicates as a 5th summary tile;
  `unmet_required_capabilities` as a danger-toned section
  rendered as JSON (open shape); `contributed_agents_per_plugin`
  + `contributed_skills_per_plugin` as a side-by-side
  contributions section; `plugin_capability_gates` as a
  per-plugin JSON list with the operator-flippable env-var
  contracts. 7 new i18n keys en+es.
- ✅ ~~**P2.12 frontend vitest coverage**~~ — added
  `RestartPluginModal` component test
  (`tests/components/restart-plugin-modal.test.tsx`)
  covering the destructive confirm-prefix safety gate
  end-to-end: prefix-empty disables, wrong-prefix disables,
  exact-match enables, click sends the right plugin id,
  disabled-button no-op, short-id defensive variant. 6
  test cases. Locale pinned to `en` so role-name matchers
  hit the canonical English catalog (default es).

P2 totals: 33 new tests (24 backend + 9 frontend), 0
regressions, mdbook clean, full workspace build clean.

P3 (6 open — observability + nits): `tracing::info!` on
destructive paths, metrics counters, `gave_up.last_exit_code = -1`
sentinel docs, plugin.id NATS-subject-safety validation,
`_all` magic segment, fixture state leakage RAII guards.

24 new tests (10 backend + 14 frontend), 0 regressions.

### Phase 91 — STT pure-Rust migration via Candle — shipped 11/12, follow-ups open

Phase 91 shipped 2026-05-10 across the 11 substantive sub-phases
(91.1 API research → 91.11 docs). The Candle backend
(`feature = "stt-candle"`) is now the default-track STT
implementation; legacy `whisper-rs` (`feature = "stt"`) is
retained for one stability window. See
[`PHASE-91-STT-CANDLE-MIGRATION-PLAN.md`](PHASE-91-STT-CANDLE-MIGRATION-PLAN.md)
+ [`PHASE-91-CANDLE-API-RESEARCH.md`](PHASE-91-CANDLE-API-RESEARCH.md)
+ [`PHASE-91-CROSS-COMPILE-MATRIX.md`](PHASE-91-CROSS-COMPILE-MATRIX.md)
for the detailed close-out.

- ⏭️ **91.12 DEFER — drop legacy `stt` feature** — scheduled
  for ≥ 2 release cycles after 91.11 ships (target 2026-07
  earliest). Trigger condition: zero `stt-whisper-cpp` consumers
  in microapp telemetry + parity tests green across two
  consecutive RCs.

- ✅ ~~**91.x.wasm — unblock `wasm32-unknown-unknown`**~~ shipped
  2026-05-10 across three sub-commits. SDK now compiles clean
  on `wasm32-unknown-unknown`:

  - **phase-1** (ada8514) — `hf-hub` gated behind
    `stt-candle-hub` sub-feature + `uuid` gains the `js`
    feature on wasm32 so browser `crypto.getRandomValues`
    powers v4/v5 generation.
  - **phase-2** (6f94393) — `microapp-sdk`'s `tokio` dep
    switched from `workspace = true` to a direct
    `version = "1"` pin so the workspace-level
    `tokio = { features = ["full"] }` doesn't unionise `net`
    into this crate. WASM baseline features:
    `["macros", "rt", "sync", "io-util", "time"]`. Native
    targets get `io-std`, `rt-multi-thread`, `fs`, `process`
    via `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`.
  - **phase-3** (a222f61) — `pub mod stt` gated on
    `not(target_arch = "wasm32")` so the heavy STT deps
    (opus-wave, ogg, candle-{core,nn,transformers},
    tokenizers, hf-hub) drop out on WASM.
    `Microapp::run_stdio` similarly gated since tokio's
    `io::stdin`/`stdout` aren't available on wasm32. WASM
    consumers use `run_with` against a caller-supplied
    transport (MessageChannel / wasm-bindgen pipe).

  Result: SDK compiles on `wasm32-unknown-unknown` (0.44 s
  default, 1.65 s `--features stt-candle`) AND native
  (Linux gnu/musl × 2, macOS Apple Silicon, Windows MSVC,
  Android NDK r27c). 25 stt:: native unit tests still green
  across both backend configs.

- ✅ ~~**91.x.wasm.phase-4 — WASM-side STT inference**~~ shipped
  2026-05-10 — picked path (b) from the original three options:
  cloud STT via `stt-cloud` feature wired with `reqwest`'s
  wasm32 browser-fetch transport.
  - **phase-4** (cloud trait + OpenAI / Groq REST + Anthropic
    voice_stream stub + `CompositeProvider` fallback chain) —
    shipped at `c87e40a` per the parent Phase 91 commit history.
  - **phase-4b** (Anthropic `voice_stream` WebSocket real
    implementation, mining
    `claude-code-leak/src/services/voiceStreamSTT.ts`) —
    shipped at `fb46dd2`: full one-shot client with KeepAlive
    heartbeat, 4-trigger finalize state machine
    (PostCloseStreamEndpoint / NoDataTimeout / SafetyTimeout /
    WsClose), 15/15 unit tests. Native-only (tokio-tungstenite
    pulls tokio `net`); WASM voice_stream falls under
    sub-follow-up 4c below.
  - **phase-4d** (`LocalCandleProvider` bridge so the local
    Candle backend joins `CompositeProvider` chains as the
    offline fallback leg) — shipped this same commit: 6 new
    unit tests, MIME picker (ogg-opus only), RAII tempfile
    cleanup, Cow-based per-request lang-hint override.
  Path (a) upstream WASM PRs per local-inference dep + path
  (c) `transformers.js` bridge remain deferred — see
  `91.x.wasm.phase-4c` / `phase-4e` below.

- 🔄 **91.x.wasm.phase-4c — WASM transport for the cloud STT
  legs** — phase-4c.2 + phase-4c.3 shipped; phase-4c.4 deferred.
  - ✅ ~~**phase-4c.2 — hand-assembled multipart body**~~
    shipped — REST legs (OpenAI + Groq) no longer use
    `reqwest::multipart::Form` (native-only). Replaced with
    `build_openai_multipart_body` (RFC 7578 body built into
    `Vec<u8>`, submitted via cross-platform `.body(Vec<u8>)`).
    6 new unit tests verify byte-exact framing including
    binary-audio passthrough + BCP-47 region subtag strip +
    language auto-detect omission.
  - ✅ ~~**phase-4c.3 — workspace-level reqwest split**~~
    shipped 2026-05-11 (commit `c4149c3`). REST cloud STT
    now compiles cleanly for wasm32. Three structural changes:
    (1) workspace.dependencies.reqwest trimmed to wasm-clean
        baseline `["json", "charset"]`; native-only features
        moved to consumer crates via additive declarations
        (9 crates updated: cdp, ext-installer, core,
        extensions, llm-auth, llm, memory, setup + daemon
        binary which re-adds `macos-system-configuration` for
        proxy detection);
    (2) SDK reqwest moved from shared `[dependencies]` into
        per-target blocks — `["json"]` on wasm32, `["json",
        "rustls-tls"]` on native. Cargo's duplicate-key rule
        only fires when the same dep is in both the shared
        table and a per-target block; with shared decl
        removed, per-target blocks are independent
        declarations Cargo accepts cleanly;
    (3) SttProvider trait split per target — wasm32 variant
        drops `Send + Sync` + uses `async_trait(?Send)`
        because reqwest's wasm-bindgen fetch backend returns
        futures holding `Rc<RefCell<js_sys::futures::Inner>>`
        which can't be Send. Single-threaded execution on
        wasm32 means the bounds were a native-only thing.

    Verified via `cargo tree -p nexo-microapp-sdk --target
    wasm32-unknown-unknown --features stt-cloud-wasm
    --invert reqwest`:

      reqwest v0.12.28
      └── nexo-microapp-sdk

    Before phase-4c.3: "nothing to print" — reqwest never
    entered the wasm32 graph. After: linked correctly.

    Native suite unchanged: 39/39 cloud tests pass.
  - ⬜ **phase-4c.4 — Anthropic voice_stream WASM transport** —
    still deferred. The voice_stream WebSocket leg
    (`AnthropicVoiceStream`) carries its own `cfg(not(wasm32))`
    gate because tokio-tungstenite drags TCP types absent on
    wasm32. Browser microapps demanding voice_stream need a
    `gloo-net::websocket::futures::WebSocket` swap-in. Design
    path: split `transcribe` into a generic
    `transcribe_protocol<Sink, Stream>` method + two
    cfg-gated thin opener fns (`open_ws_native` /
    `open_ws_wasm`). KeepAlive heartbeat wrapped in
    `wasm_bindgen_futures::spawn_local` on wasm
    (`tokio::spawn` needs the multi-threaded runtime which
    wasm doesn't have). Estimated cost: ~4-6 h. The protocol
    state machine (KeepAlive heartbeat, 4-trigger finalize,
    event parser) is target-agnostic and stays identical.

- ⬜ **91.x.wasm.phase-4b.streaming — full push-to-talk live
  transcription** — phase-4b ships the one-shot path (buffer →
  WebSocket → CloseStream → finalize). The original
  `claude-code-leak/src/services/voiceStreamSTT.ts` reference is
  a live PTT experience: microphone chunks stream every ~32 ms,
  interim text drives a UI shimmer, utterance boundaries fire
  segment events. Wire when a microapp surfaces live dictation
  (dictation panel, voice commands). Mining notes already
  captured in the phase-4b commit body.

- ⬜ **91.x.wasm.phase-4b.segmentation — auto-finalize on
  non-cumulative interim shift** — claude-code-leak ships
  segmentation logic: when an interim transcript shifts away
  from cumulative growth (the server effectively redacts a
  partial word), the client flushes the accumulated text and
  starts a new segment without waiting for `TranscriptEndpoint`.
  Phase-4b finalizes only on the 4 explicit triggers; add the
  shift detector when a microapp surfaces multi-utterance
  dictation. Trivial state-machine extension; defer until
  there's a real product UX driving the requirement.

- ⬜ **91.x.wasm.phase-4e — pure-Rust browser inference via
  transformers.js / whisper-wasm** — path (c) from the
  original phase-4 brainstorm. Adds a wasm-bindgen bridge so
  WASM consumers get fully-offline STT in the browser without
  cloud round-trips. Not on the critical path because Candle +
  cloud already covers every non-browser deployment; this is a
  privacy-first browser micro-app feature. Defer.

- ⬜ **91.x.wasm.phase-5 — workspace-wide tokio "full" split** —
  if the daemon binary itself ever needs to compile as WASM
  (edge / serverless deployment), the workspace-level
  `tokio = { features = ["full"] }` pin in
  `proyecto/Cargo.toml` would need to be split into
  per-consumer feature lists. High blast radius — touches
  every workspace crate's Cargo.toml. Today only
  `nexo-microapp-sdk` targets WASM (via the targeted phase-2
  override); the daemon stays native-only.

- ⬜ **91.x.long-form — audio clips > 30 seconds** — Phase 91 v1
  rejects clips exceeding `m::N_SAMPLES` (30 s @ 16 kHz) with
  `SttError::Decode`. Whisper itself supports arbitrary-length
  audio via internal 30-second chunking; we punted v1 because
  WhatsApp / Telegram voice notes rarely exceed 30 s. Add a
  chunker once a microapp surfaces longer audio (Zoom-style
  meetings, podcast clips, dictation).

- ⬜ **91.x.beam-search — BeamSearch sampling parity** —
  `whisper-rs::SamplingStrategy` exposes both `Greedy` and
  `BeamSearch { beam_size, patience }`. Phase 91 v1 implements
  Greedy only (matches the default we use today). Add the beam
  loop when a parity test surfaces a real WER regression
  attributable to the strategy difference; Candle's
  temperature-fallback chain is the analogous mechanism.

- ⬜ **91.x.cloud — cloud STT backend (Groq / OpenAI / Deepgram)** —
  high-volume SaaS where latency / cost favour cloud over local
  inference may want a `stt-cloud` feature in addition to
  `stt-candle`. Wire shape: HTTP POST to the provider's
  multipart endpoint, parse the JSON transcript reply. Mining
  reference: `claude-code-leak/src/services/voiceStreamSTT.ts`
  uses a WebSocket streaming shape for real-time STT — relevant
  if we ever add live transcription.

- ⬜ **91.x.large-model — 128-bin mel filterbank** — Phase 91 v1
  ships only the 80-bin `melfilters.bytes` vendored asset, used
  by `tiny` / `base` / `small` Whisper variants. The 128-bin
  variant powers `large-v3`; add the matching
  `melfilters128.bytes` (also vendored from
  `huggingface/candle/candle-examples/examples/whisper/melfilters128.bytes`)
  + lift the explicit `num_mel_bins != 80` rejection in
  `crates/microapp-sdk/src/stt/mel.rs`.

- ⬜ **91.x.parity-fixtures — vendor a tiny audio fixture corpus** —
  the parity test in `crates/microapp-sdk/tests/stt_candle_parity.rs`
  reads from `NEXO_STT_PARITY_FIXTURES_DIR`. Operators running
  the test need to bring their own audio. Vendor a tiny
  (≤ 5 voice notes × ≤ 100 KB each) ogg-opus corpus checked
  into the repo so CI runs the parity check end-to-end
  automatically. Licence-clean speech from
  `commonvoice.mozilla.org` is the obvious source.

### Phase 90 — nexo-plugin-admin — shipped, follow-ups open

Phase 90 (admin plugin out-of-tree) shipped 2026-05-10. The
plugin (`cargo install nexo-plugin-admin`) covers the admin
surface; ~5342 LOC of `proyecto/admin-ui/` + ~2702 LOC of
`proyecto/src/main.rs::run_admin_web` removed in the same wave.
Open items below are degradations the v1 plugin tolerates,
NOT shipping blockers.

- ✅ ~~**memory admin RPC (query)**~~ shipped 2026-05-10 — added
  `nexo/admin/memory/query` (capability `memory_query`).
  `LiveMemoryReader` lazy-opens `LongTermMemory` from
  `<config_dir>/memory.yaml` on first call (cached via
  `tokio::Mutex<Option<...>>`). Plugin admin's `/m/memory`
  page is LIVE in nexo-plugin-admin@0.1.4 — agent_id +
  free-text query inputs, recall-style result cards with
  tags + concept_tags + memory_type badge.
- ✅ ~~**memory snapshot admin RPC** — full CRUD shipped~~.
  `list_snapshots` shipped 2026-05-10 in nexo-plugin-admin@0.1.7.
  `delete_snapshot` + shared cell refactor (lifts the
  path_resolver limitation) shipped 2026-05-10 in
  nexo-plugin-admin@0.1.8. `create_snapshot` +
  `restore_snapshot` shipped 2026-05-10 in
  nexo-plugin-admin@0.1.10 (Phase 90.x.memory-snapshot.create-restore):
  trait `MemorySnapshotReader` extended with `create()` +
  `restore()`, `MemorySnapshotsListResponse` gained
  `encryption_available: bool`, defaults forced server-side
  (`redact_secrets=true`, `auto_pre_snapshot=true`,
  `created_by="admin-ui"`, recipient resolved from
  `recipients[0]`). Restore by `snapshot_id` (server resolves
  bundle_path, never accepts client-supplied paths). Tenant
  REQUIRED + manifest validation rejects mismatch before
  touching disk. `dry_run=true` returns RestoreReportWire
  preview without mutation. Fixed-shape contract logged in
  `docs/src/ops/memory-snapshot.md` § Admin RPC surface.
  Backend: nexo-tool-meta 0.1.11→0.1.12 (5 new wire types +
  shape change on list response), nexo-core 0.1.11→0.1.12
  (trait + dispatcher arms + 8 unit tests). Adapter: 4
  integration tests in `crates/setup/src/admin_adapters.rs`
  (`live_memory_snapshot_tests`).

- ⬜ **memory snapshot trait rename `MemorySnapshotReader` →
  `MemorySnapshotAdmin`** — Phase 90.x.memory-snapshot.create-restore
  added write methods (`create`, `restore`) to a trait still
  named `…Reader`. Naming kept as deuda to avoid breaking
  `nexo-core 0.1.x` consumers that pin `Arc<dyn
  MemorySnapshotReader>`. Slated for next major bump.

- ✅ ~~**memory snapshot multi-recipient encrypt**~~ — shipped
  2026-05-10 in `nexo-memory-snapshot 0.1.2` + `nexo-core 0.1.14`.
  Additive `EncryptionKey::AgePublicKeys(Vec<String>)` variant
  (preserves backward compat via `#[non_exhaustive]`); shared
  `resolve_recipients` helper does dedup (silent + debug log) +
  parse-with-index + non-empty rejection. Both `pack_pipeline`
  and `build_encryption_meta` match both variants and converge
  on `Vec<Recipient>` for the underlying `encrypt_writer` (which
  already accepted Vec). Manifest's `recipients_fingerprint:
  Vec<String>` populated with all fingerprints. Admin adapter
  (`LiveMemorySnapshotReader::create`) switches uniformly to
  `AgePublicKeys` (single-recipient case still uses the new
  variant — bundle output identical to legacy single-string
  path; uniform code path). CLI `--encrypt age:` flag unchanged
  for backward compat. Boot-time validation: `main.rs` parses
  every recipient string at daemon startup so typos fail-fast.
  Tests: 3 unit (`pack_pipeline_handles_age_public_keys_variant`,
  `build_encryption_meta_lists_all_fingerprints`,
  `pack_pipeline_dedupes_duplicate_recipients`) + 2 integration
  (`live_create_with_multi_recipient_passes_all`,
  `live_create_with_single_recipient_still_uses_keys_variant`).
  150/150 nexo-memory-snapshot tests pass; 6/6
  live_memory_snapshot_tests pass. Docs: `docs/src/ops/memory-snapshot.md`
  § Encryption gains "Multi-recipient encryption (admin UI)"
  subsection.

- ⬜ **memory snapshot streaming progress** —
  `nexo/notify/snapshot_progress` for long-running creates.
  Defer until post-launch metrics show p95 > 30s. Today the
  request blocks until completion which is acceptable for
  typical agent state sizes.

- ⬜ **memory snapshot verify-bundle preview RPC** —
  `nexo/admin/memory/verify_snapshot` exposing
  `MemorySnapshotter::verify` results in the SPA so operators
  can audit bundle integrity before restoring. Defer — UI
  does not currently expose verify; CLI keeps the verb.

- ⬜ **memory snapshot diff RPC from UI** —
  `nexo/admin/memory/diff_snapshots` exposing
  `MemorySnapshotter::diff` for two-bundle comparison. Defer
  until operator request.

- ✅ ~~**plugins admin RPCs**~~ shipped 2026-05-10 — added
  `nexo/admin/plugins/doctor` (capability `plugin_doctor`).
  v1 returns the daemon's `agent doctor plugins --json` output
  verbatim wrapped with a `generated_at_ms` stamp.
  `LivePluginDoctorReader` re-runs `wire_plugin_registry` +
  `doctor_render::render_json` on each call (~hundreds of ms,
  acceptable for operator-driven page). Plugin admin's
  `/m/plugins` page is LIVE in nexo-plugin-admin@0.1.3 (4-tile
  summary + InitOutcome badges + diagnostics). Typed wire mirror
  deferred until field set stabilises; `admin/plugins/list` not
  separately needed — `loaded_ids` lives inside the doctor report.

- ✅ ~~**mcp_servers admin RPCs**~~ shipped 2026-05-10 — added
  `nexo/admin/mcp/{list,get,upsert,delete}` admin RPC family
  (capability `mcp_crud`) + `McpYamlStore` round-tripping
  through serde_yaml::Value to preserve operator-set top-level
  keys. Plugin admin's `/m/mcp_servers` page is LIVE in
  nexo-plugin-admin@0.1.2 (live table + create modal +
  transport picker for stdio/streamable_http/sse/auto). 10
  unit tests cover yaml round-trip + idempotent upsert +
  cascade-delete + transport validation.

- ✅ ~~**Channel approve UX**~~ shipped 2026-05-10 in
  nexo-plugin-admin@0.1.5 — `ChannelApproveModal` with agent
  picker (dropdown from `listAgents`, falls back to free-text)
  + server picker (free-text + `<datalist>` autocomplete from
  `listMcpServers`, also accepts plugin-shipped names like
  `plugin:telegram:tg`) + allowlist editor (comma-separated
  binding indices; empty = all). Backed by existing
  `nexo/admin/channels/approve` (Phase 82.10.f, no backend
  changes needed).

- ✅ ~~**Tenants CRUD wrappers**~~ shipped 2026-05-10 (commit 007a3d3
  in nexo-rs-plugin-admin) — `api/tenants.ts` now exposes
  `tenantsUpsert`, `tenantsSetActive`, `tenantsDelete` (with
  cascade-purge on orphan agents). UI in `/m/tenants` has create
  modal + activate/deactivate + delete buttons. Daemon shape
  (`id`, `display_name`) is translated to legacy frontend shape
  (`tenant_id`, `name`) inside the wrapper so the rail switcher
  + zustand store keep working unchanged.

- ✅ ~~**TokenRotated cookie session swap**~~ shipped 2026-05-10
  in nexo-plugin-admin@0.1.6 — `nexo/notify/token_rotated`
  listener wired through the SDK. Atomic 2-step swap:
  (1) `http::handle_token_rotated(LiveTokenState, ...)` for the
  bearer middleware, (2) fresh `AdminSession::new_random()` +
  `LiveAdminSession::swap` for the cookie HMAC secret. Existing
  browser cookies invalidate immediately (signed with the old
  secret); operator re-logs in with the new password printed
  to stderr.

- ⬜ **Auto-spawn plugin-not-installed UX** — `agent admin`
  shim today exits with install instructions when the plugin
  binary is missing in PATH. A future enhancement: prompt
  the operator with `cargo install nexo-plugin-admin` and run
  it interactively (matches the `cargo install` flow some
  competitors offer).

- ✅ ~~**GitHub remote for nexo-rs-plugin-admin**~~ shipped
  2026-05-10. Repo created at
  https://github.com/lordmacu/nexo-rs-plugin-admin (public).
  All 9 commits + 8 release tags (v0.1.0 → v0.1.8) pushed.
  CHANGELOG.md compare-links now resolve. PR-ready.

- 🟡 **Plugin admin e2e test** — partial. 3 binary-only smoke
  tests shipped 2026-05-10 in nexo-plugin-admin@0.1.9
  (`tests/handshake_smoke.rs` + `tests/http_smoke.rs`):
  handshake initialize reply shape (server_info + empty tools)
  + HTTP /healthz + /api/admin 401 path (no bearer + wrong
  bearer) + /login form render. `#[ignore]` by default; opt in
  with `cargo test --tests -- --ignored` locally + as a
  release gate. Daemon-backed CRUD round-trip (spawn daemon +
  plugin + drive a real `nexo/admin/agents/list`) is heavier
  and remains open as a follow-up.



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

### ~~Phase 27.2 follow-up.b — Termux release re-enable (rustls aws-lc-sys → ring)~~ ✅ shipped 2026-05-10

**Resolved** in commit `1247fdf` (proyecto) + `9364347`
(`lordmacu/nexo-plugin-email` published as `0.1.3` to crates.io).

**Actual root cause** (refined from original suspicion): not
`reqwest` but `tokio-rustls 0.26`. Its default feature set
includes `aws_lc_rs`, which forwards to `rustls/aws_lc_rs` and
pulls the `aws-lc-sys` C build chain. Cargo feature unification
poisons the whole graph if ANY consumer pulls `tokio-rustls`
without `default-features = false`. The workspace dep
`tokio-rustls = "0.26"` was the silent culprit + a matching
unguarded `rustls = "0.23"` in published `nexo-plugin-email
0.1.2`.

**Fix shipped:**

1. `proyecto/Cargo.toml [workspace.dependencies] tokio-rustls`
   pinned to `{ version = "0.26", default-features = false,
   features = ["ring", "tls12", "logging"] }`.
2. `nexo-plugin-email 0.1.3` published with matching `rustls` +
   `tokio-rustls` pins.
3. `proyecto/Cargo.toml` bumped `nexo-plugin-email` dep to
   `0.1.3`; the temporary `[patch.crates-io]` override used
   during the investigation was dropped.

**Verification:**

- `cargo metadata --filter-platform=aarch64-linux-android` shows
  `rustls@0.23.38` features = `['log', 'logging', 'ring', 'std',
  'tls12']` — `aws-lc-rs` no longer present.
- `cargo ndk --target arm64-v8a --platform 21 build --release
  --bin nexo` produces a working `aarch64-linux-android` ELF PIE
  binary (88 MB, 6m 08s) with NDK r27c.

**Outstanding work for the build pipeline (separate from the
project-side fix):** `.github/workflows/release.yml`'s
`build-termux` job (currently guarded by `if: false`) still
needs to switch from `cargo-zigbuild` to `cargo-ndk` +
`ANDROID_NDK_HOME`. Local validation showed `cargo-zigbuild
0.22.3`'s bundled Android sysroot is incomplete for `ring` +
`libsqlite3-sys` C source (`'assert.h' file not found`,
`'stdio.h' file not found`); switching to a real NDK install
on the runner side resolves it. This is a CI-only follow-up:
file as Phase 27.2 follow-up.b.ci to remove the `if: false`
guard once the workflow swaps the toolchain.

Memory: [`feedback_rustls_default_features_off.md`](../../.claude/projects/-home-familia-chat-proyecto/memory/feedback_rustls_default_features_off.md)
captures the pattern so future workspace + plugin Cargo.toml
audits catch the same mistake.

The `nexo` daemon binary itself runs fine on Android via Termux
when built locally (`pkg install rust && cargo install --git ...`).

### Phase 27.2 follow-up — re-enable Apple / Windows / Homebrew / npm release channels

Phase 27.2 dropped Apple Silicon, Apple x86_64, and Windows targets
from the `cargo dist` matrix to keep CI green during the initial
release-pipeline build-out. Today the only shipped binary channel is
the universal shell installer for Linux musl x86_64 + aarch64.

That covers Linux + Termux but blocks four high-leverage adoption
channels:

- **macOS** (`x86_64-apple-darwin` + `aarch64-apple-darwin`) —
  big slice of the indie / hobbyist audience exploring AI agent
  frameworks, often on Apple Silicon laptops.
- **Windows** (`x86_64-pc-windows-msvc`) — same audience plus
  enterprise dev experimentation.
- **Homebrew** (`brew install nexo-rs`) — canonical install
  channel for macOS devs; needs at least the macOS targets +
  a `homebrew-nexo-rs` tap repo.
- **npm** (`@nexo-rs/cli` + per-platform packages) — `npx nexo`
  lets curious JS devs try the framework without installing a
  permanent binary; large adoption signal in the AI agent space
  (LangChain, AutoGPT, Mastra all reach JS via this channel).

Work to re-enable:

1. **`dist-workspace.toml`**:
   - Add Apple/Windows targets to `targets = […]`.
   - Bump `installers = […]` to include `homebrew`, `npm`, `msi`,
     `powershell` alongside the existing `shell`.
   - Add `npm-scope = "@nexo-rs"` (or whatever scope the team
     owns).

2. **`.github/workflows/release.yml`** matrix entries for the
   new targets (the workflow is hand-rolled, not regenerated by
   `dist generate`):
   - `runs-on: macos-13` for `x86_64-apple-darwin`
   - `runs-on: macos-14` for `aarch64-apple-darwin` (M1/M2 native)
   - `runs-on: windows-latest` for `x86_64-pc-windows-msvc`

3. **Homebrew tap repo** (`lordmacu/homebrew-nexo-rs`) for the
   formula cargo-dist auto-generates and pushes per release.

4. **npm scope ownership** — register `@nexo-rs` (or `@nexo`,
   if available) on npmjs.com + add the publish token to GH
   Actions secrets as `NPM_TOKEN`.

5. **Validate** by tagging a `nexo-rs-v0.1.X-rc1` pre-release
   that exercises the full matrix without affecting production
   release channels.

6. **Landing + README**: drop the "coming back in the next
   release window" footnote in `docs-site/index.html`'s install
   section and the equivalent note in `README.md`'s Quick Start.

Estimated work: ~2 h of config + 1 h of validation across a
test tag. Risk: low if staged on a pre-release tag; high if
landed straight on a production tag with no rehearsal.

Tracked here because the docs landing page promises these
channels in a footnote — that promise should match the next
shipped release's reality.

### Subprocess dep removal — pure-Rust pipeline migration in progress

Multi-step effort to drop external binary dependencies from the
framework. Each step is independently shippable; pure-Rust
replacements are the precondition for the Phase 90 Android
embedded build (extensions of the SDK can't `Command::new` on
Android sandbox).

- **`subprocess.ffmpeg`** — RESOLVED 2026-05-08. STT decoder
  swapped from `Command::new("ffmpeg")` to `ogg::PacketReader`
  + `opus_wave::OpusDecoder` (pure Rust SILK+CELT, RFC 6716
  conformant, cross-validated against C libopus); TTS encoder
  swapped from the same ffmpeg pipe to `symphonia` (mp3 decode)
  + `opus_wave::OpusEncoder` (Voip-mode 20 ms frames at 24 kHz
  mono) + `ogg::PacketWriter` (OpusHead + OpusTags + audio
  pages per RFC 7845 § 5). Validated end-to-end on real
  WhatsApp PTT: STT 0.6–0.8 s vs 1.5–3 s with ffmpeg, voice
  bubble plays correctly on the recipient device. Drops the
  `ffmpeg` PATH dependency entirely. SDK API non-breaking
  (signature of `transcode_mp3_to_opus_ogg` and
  `transcribe_file` preserved); `TranscribeConfig.ffmpeg_path`
  is now ignored and marked `#[deprecated]`. `SttError` gains
  `Decode` and `UnsupportedFormat` variants.

- **`subprocess.xdg-open`** — RESOLVED 2026-05-08. Replaced the
  per-OS `Command::new("open" | "xdg-open" | "start")` heuristic
  in `crates/setup/src/services/anthropic_oauth.rs` with the
  `webbrowser` crate. One call site, three `#[cfg]` branches
  collapsed into a single line.

- **`subprocess.hostname`** — RESOLVED 2026-05-08. Replaced the
  `Command::new("hostname")` shell-out in
  `crates/dispatch-tools/src/bin/nexo_driver_tools.rs` with the
  `gethostname` crate (pure-Rust syscall, ~50 LOC). Works on
  minimal images that don't ship the `hostname` binary on PATH.

- **`subprocess.cosign`** — RESOLVED 2026-05-08. Replaced the
  `cosign verify-blob` subprocess in
  `crates/ext-installer/src/verify.rs` with a pure-Rust pipeline
  built from `x509-parser` (PEM + X.509 v3 parse, SAN +
  custom-OID extraction), `p256::ecdsa` (ECDSA-P256 signature
  verify over `sha2::Sha256` of the blob), `base64` (DER
  signature decode) and the existing `regex` dep (identity-
  policy match). Replicates the cosign policy semantics: SAN
  URI/email matches `policy.identity_regexp`, Fulcio
  OIDC-issuer extension (OID `1.3.6.1.4.1.57264.1.1` legacy
  raw-string + `…1.8` modern DER UTF8String) equals
  `policy.oidc_issuer`. Six new tests cover the happy path,
  blob-tampering rejection, identity/issuer mismatches, and
  malformed PEM/signature inputs — all green. Trade-off taken
  vs. the `sigstore` 0.13 crate's `cosign` feature: that route
  pulls `oci-client` + 258 transitive deps and won't cross-
  compile cleanly to `aarch64-linux-android`. Known gap: the
  Rekor transparency-log proof (`--bundle` path) is no longer
  enforced; the bundle field on `VerifyInput` is ignored. If
  multi-tenant SaaS hardening ever needs offline TLog
  verification, gate it behind an opt-in feature pulling
  `sigstore`'s bundle path. The legacy `discover_cosign_binary`
  / `VerifyError::CosignNotFound` / `VerifyError::CosignFailed`
  surface stays in place as a no-op compatibility shim so the
  install orchestration keeps compiling until the next major
  bump.

- **`subprocess.skill-runners-and-hooks`** — NOT-A-FOLLOWUP.
  `crates/dispatch-tools/src/hooks/dispatcher.rs` (`sh`),
  `crates/dispatch-tools/src/program_phase.rs` (`bash`), and
  `crates/core/src/agent/skills.rs` (user-provided binary) are
  inherently subprocess by design — operator extensibility hinges
  on running arbitrary user scripts. Not eligible for Rust
  replacement; for embedded Android, gate behind a
  `subprocess-hooks` feature flag (see Phase 90 plan).

### Phase 81.17.c — plugin-browser standalone repo — shipped, 9/9 follow-ups resolved

Browser plugin extracted to public repo
[github.com/lordmacu/nexo-plugin-browser](https://github.com/lordmacu/nexo-plugin-browser)
+ published to crates.io as `nexo-plugin-browser v0.2.0`.
External operators install via `cargo install nexo-plugin-browser`
— autonomous, no proyecto checkout required. Slimmed deps to
4 (`nexo-microapp-sdk`, `nexo-broker`, `nexo-cdp`,
`nexo-config`); the dead `nexo-core` / `nexo-llm` /
`nexo-resilience` / `nexo-plugin-manifest` direct deps were
dropped.

9 follow-ups resolved 2026-05-07:

- **`81.17.c.publish-github`** — RESOLVED 2026-05-07. Repo
  published to
  [github.com/lordmacu/nexo-plugin-browser](https://github.com/lordmacu/nexo-plugin-browser)
  (PUBLIC). Tag `v0.2.0` triggered the release workflow which
  builds linux-x64 + macos-arm64 binaries and creates a GitHub
  Release with assets attached. The `nexo-rs` org placeholder
  was dropped — repos live under `lordmacu/`.

- **`81.17.c.crates-publish`** — RESOLVED 2026-05-07. Four
  crates.io publishes shipped today:
    - `nexo-cdp v0.1.0` (workspace crate from `nexo-cdp-extract`).
    - `nexo-plugin-manifest v0.1.1` (the published 0.1.0 was
      missing entrypoint/extends/sandbox/supervisor schemas).
    - `nexo-microapp-sdk v0.1.2` (gained `ToolDef` /
      `on_tool` / `declare_tools` + uses new manifest crate).
    - `nexo-plugin-browser v0.2.0` (slimmed to 4 deps from
      crates.io alone — no proyecto sibling required).
  External operators: `cargo install nexo-plugin-browser`. The
  full 30-crate workspace publish (touching nexo-core 0.1.2,
  nexo-mcp, nexo-memory, the never-published driver-* /
  fork / dream / dispatch-tools / llm-auth crates) remains
  Phase 83.14.b territory — needed for daemon publish, not
  for the plugin.

- **`81.17.c.in-tree-removal`** — RESOLVED 2026-05-07. The
  in-tree `crates/plugins/browser/` crate is gone; daemon
  builds + e2e tests pass against the standalone repo. The
  `cdp_client_test.rs` + `session_test.rs` + `browser_cdp_e2e.rs`
  fixtures moved to `crates/cdp/tests/` (resolved together with
  `nexo-cdp-extract`). The `browser-test` example removed —
  coverage lives in `nexo-rs-plugin-browser/tests/e2e_handshake.rs`.

- **`81.17.c.multi-profile`** — RESOLVED 2026-05-07. Shipped
  in `nexo-plugin-browser v0.2.1`:
    - `DashMap<agent_id, BrowserPlugin>` reemplaza el
      single-OnceCell del v0.2.0; primer `tool.invoke` por
      agente lazy-boota Chrome con `${BASE}/profiles/<agent_id>/`.
    - 3 env knobs:
      `NEXO_PLUGIN_BROWSER_MAX_PROFILES` (default 10, range 1..=64),
      `NEXO_PLUGIN_BROWSER_PROFILE_IDLE_SECS` (default 900,
      range 0..=86400; 0 disables eviction),
      `NEXO_PLUGIN_BROWSER_MULTI_PROFILE` (default true; opt-out
      reverts to v0.2.0 single-shared-profile behaviour).
    - Sanitiser regex `[A-Za-z0-9_-]{1,64}` + path-traversal
      defence; rejects → `-33402`.
    - Cap reached → `-33404 Unavailable`.
    - Idle eviction loop polls every 30 s, calls
      `BrowserPlugin::shutdown_chrome` past threshold; on-disk
      profile dir survives so next call lazy-reboots cleanly.
    - Chrome profile chip decoration (name + sha256-derived
      stable color) per OpenClaw chrome.profile-decoration.ts:108-162.
  Tests: 17 sanitiser + 8 limits + 3 decoration + 1 plugin
  shutdown + 5 e2e_multi_profile, all green.
  See [GitHub Release v0.2.1](https://github.com/lordmacu/nexo-plugin-browser/releases/tag/v0.2.1).

- **`81.17.c.latency-numbers`** — RESOLVED 2026-05-07.
  Bench harness shipped at
  `nexo-rs-plugin-browser/benches/tool_latency.rs` (plain
  Instant::now-based, no criterion dep). Baseline measured
  on Cristian's dev laptop: pre-Chrome dispatch round-trip
  avg=164µs, p95=156µs, p99=4.2ms (n=200). Live Chromium
  numbers gated on `CHROMIUM_BIN` — to be populated when run.

- **`nexo-cdp-extract`** — RESOLVED 2026-05-07. Lifted to
  workspace crate `nexo-cdp` v0.1.0 at `crates/cdp/`. Both
  the standalone repo and the (since-removed) in-tree crate
  consume it via path dep. `CdpSession::client()` promoted
  from `pub(crate)` to `pub` so cross-crate consumers can
  reach the underlying client.

- **`81.17.c.e2e-test-fixture`** — RESOLVED 2026-05-07.
  `nexo-rs-plugin-browser/tests/e2e_handshake.rs` ships 3
  default + 1 #[ignore]-gated test:
    - `e2e_initialize_advertises_twelve_browser_tools`
    - `e2e_tool_invoke_invalid_args_returns_minus_33402_before_chrome_boot`
    - `e2e_tool_invoke_unknown_tool_returns_minus_33401`
    - `e2e_browser_navigate_round_trips` (gated on
      `CHROMIUM_BIN`)

- **`81.17.c.hot-reload-test`** — RESOLVED 2026-05-07.
  `nexo-rs-plugin-browser/tests/e2e_persistence.rs` covers
  the subprocess-side guarantees (PID stability across N
  consecutive tool.invoke calls; recovery from
  unknown-method errors). Daemon-side reload coverage lives
  in `proyecto/crates/core/tests/subprocess_plugin_e2e.rs`.

- **`81.29.b.shipped-via-81.17.c`** — RESOLVED. The SDK
  `on_tool` + `declare_tools` helpers landed in 81.17.c.1.
  Mark FOLLOWUPS entry as completed when sweeping resolved
  items.

### Phase 81.18 — plugin-telegram standalone repo — shipped, follow-ups open

Telegram plugin extracted to a standalone repo at
`/home/familia/chat/nexo-rs-plugin-telegram/` (Shape B: lib +
bin) on 2026-05-09. Daemon imports the lib via path-dep so
today's in-tree behaviour is byte-equivalent; the subprocess
fallback flip is the deferred follow-up below.

- **`81.18.b.subprocess-flip`** — daemon currently builds
  `TelegramPlugin` in-process via the lib import (paths
  through `proyecto/src/main.rs:2294-2302`). Phase 81.18.b
  drops that block and lets the subprocess discovery walker
  (Phase 81.17 + 81.17.b) spawn the binary. Multi-bot
  operators have N entries in `cfg.plugins.telegram` so the
  daemon must seed env per-spawn (one subprocess per
  instance) — the current `seed_browser_subprocess_env`
  pattern is process-wide and can't carry over verbatim.
  Owner: framework. Trigger: when whatsapp 81.19 needs the
  same multi-instance subprocess plumbing — fix once for
  both. Status: pending.

- **`81.18.c.crates-publish`** — the standalone repo's
  `Cargo.toml` carries 9 path-deps to
  `../proyecto/crates/...` (`nexo-microapp-sdk`,
  `nexo-broker`, `nexo-core`, `nexo-config`, `nexo-llm`,
  `nexo-auth`, `nexo-pairing`, `nexo-resilience`,
  `nexo-plugin-manifest`). Publishing requires those crates
  on crates.io first — bundled with the wider workspace
  publish wave (Phase 83.14.b territory; 81.17.c covered
  4 of them, 5 remain). Until then operators install via
  `cargo install --git https://github.com/lordmacu/nexo-plugin-telegram`.
  Owner: ops. Status: blocked on workspace publish wave.

- **`81.18.d.voice-feature-gate`** — `voice = ["dep:whisper-rs"]`
  feature gate to drop ~25MB of binary on the embedded
  (Phase 90 — Android) build. Requires refactoring
  `tool::dispatch_telegram_tool` so the transcribe branch
  is opt-in (today the whisper subprocess spawn is
  unconditional in `auto_transcribe.enabled`). Owner:
  framework. Trigger: Phase 90 mobile binary size budget.
  Status: pending.

- **`81.18.e.publish-github`** — push the local repo to
  `github.com/lordmacu/nexo-plugin-telegram` (PUBLIC) +
  tag `v0.1.1` so the release workflow can build linux-x64
  / macos-arm64 binaries. Owner: ops. Status: pending; the
  initial commit lives in the local repo only.

- **`81.18.f.e2e-test-fixture`** — same gap as
  `81.17.c.e2e-test-fixture`: the offline e2e_handshake
  test verifies wire shape but doesn't exercise the live
  Bot API path. A bash-mock fixture (or wiremock-served
  Telegram API mock) would close the loop. Owner:
  framework. Trigger: when 81.18.b ships and the
  subprocess path becomes the production wire. Status:
  pending.

- **`81.18.g.legacy-broker-cap-warn`** — v1 manifests with
  `[capabilities.broker]` now surface as `dropped =
  ["capabilities.broker"]` (compat_v1 migrator) instead of
  silently passing through. Operators MUST upgrade to v2 to
  keep broker auto-mapping. Owner: docs / ops. Status: needs
  a CHANGELOG entry + `docs/src/plugins/manifest-v2.md`
  upgrade note.

### Phase 81.19.a — plugin-whatsapp standalone repo — shipped, follow-ups open

WhatsApp plugin extracted to a standalone repo at
`/home/familia/chat/nexo-rs-plugin-whatsapp/` (Shape B: lib +
bin) on 2026-05-09. Daemon imports the lib via path-dep so
today's in-tree behaviour is byte-equivalent; the subprocess
fallback flip is the deferred follow-up below (shared with
telegram).

- **`81.18.b.subprocess-flip`** (shared) — same concern listed
  under 81.18 above. **Resolved 2026-05-09**: telegram via
  Phase 81.18.b.1 (commit `907d3c7`), whatsapp via Phase
  81.18.b.2 (commit `91f5a54`). Both daemon flips landed:
  per-instance subprocess via
  `subprocess_plugin_factory_with_env` + synthetic manifest
  injection; whatsapp pairing UI preserved via daemon-side
  broker subscriber (`spawn_whatsapp_pairing_state_subscriber`)
  that mirrors `plugin.inbound.whatsapp.>` events into
  daemon-owned `PairingState` slots. Status: ✅ both ✅.

- **`81.18.b.e2e-mock-binary`** — RESOLVED 2026-05-09 commit
  `4be0c4e`. Synthetic mock subprocess plugin
  (`tests/fixtures/mock_subprocess_plugin.rs`, ~80 LOC, zero
  plugin-specific deps) shipped along with 5 `#[serial]`
  integration tests in `tests/subprocess_flip_e2e.rs` covering
  initialize handshake, env_clear passthrough, multi-instance
  isolation, unknown-method error path, and malformed-JSON
  resilience. Future subprocess plugins reuse the binary via
  `env!("CARGO_BIN_EXE_mock_subprocess_plugin")`. Status: ✅.

- **`81.20.c.typing-presence-rpc`** — RESOLVED 2026-05-09.
  Subprocess plugin (standalone whatsapp v0.1.3, commit
  `778b5eb`) now publishes
  `plugin.lifecycle.whatsapp.<inst>.peer_typing` broker
  events alongside the (None) in-process emitter call.
  Daemon-side `spawn_whatsapp_typing_presence_subscriber`
  (proyecto commit `b049a87`) translates them to
  `AgentEventKind::PeerTyping` and feeds the SSE firehose.
  In-tree path stays byte-equivalent (emitter call still
  fires when present; broker publish is a no-op for legacy
  deployments that don't run the daemon subscriber).
  Status: ✅. Telegram side stays a no-op — telegram doesn't
  expose a typing surface today.

- **`81.18.c.crates-publish-wave-partial`** — RESOLVED 2026-05-09
  partial: 3 crates published to unblock external operator
  installs (`cargo install --git ... --tag ...`) without
  needing a sibling proyecto checkout:
    - `nexo-plugin-manifest v0.1.2`
    - `nexo-tool-meta v0.1.1` (unblocks release-plz pre-existing
      red — see entry below)
    - `nexo-microapp-sdk v0.1.12`
  Standalone whatsapp + telegram Cargo.toml `version =` pins
  for these crates now resolve cleanly against crates.io;
  external `cargo install --git --tag v0.1.1` for telegram
  and `--tag v0.1.2` for whatsapp work end-to-end. Other
  internal deps (`nexo-broker`, `nexo-core`, `nexo-config`,
  `nexo-llm`, `nexo-auth`, `nexo-pairing`, `nexo-resilience`)
  were already at usable versions on crates.io. Status: ✅.

- **`81.18.c.crates-publish-wave-full`** — RESOLVED 2026-05-09:
  walked the topological publish order (`scripts/publish-order.sh`
  L1→L13). Three additional crates pushed past the partial-wave
  scope:
    - `nexo-ext-registry v0.1.0` (L1 leaf, NEW on crates.io)
    - `nexo-llm-auth v0.1.0` (L1 leaf, NEW on crates.io)
    - `nexo-ext-installer v0.1.0` (L2, NEW on crates.io)
  Layer audits L3–L13 found every other public crate already at
  the workspace's local version on crates.io
  (`nexo-cdp v0.1.0`, `nexo-compliance-primitives v0.1.0`,
  `nexo-lsp v0.1.1`, `nexo-pairing v0.1.3`,
  `nexo-resilience v0.1.1`, `nexo-taskflow v0.1.1`,
  `nexo-team-store v0.1.1`, `nexo-tunnel v0.1.2`,
  `nexo-config v0.1.4`, `nexo-auth v0.1.5`,
  `nexo-broker v0.1.2`, `nexo-llm v0.1.2`,
  `nexo-extensions v0.1.2`, `nexo-memory v0.1.2`,
  `nexo-webhook-server v0.1.5`, `nexo-mcp v0.1.2`,
  `nexo-microapp-http v0.1.0`, `nexo-plugin-email v0.1.1`,
  `nexo-plugin-google v0.1.1`, `nexo-poller v0.1.1`,
  `nexo-setup v0.1.1`, `nexo-poller-ext v0.1.1`,
  `nexo-poller-tools v0.1.1`, `nexo-web-search v0.1.1`,
  `nexo-webhook-receiver v0.1.5`).

  Three crates remain unpublished and BLOCKED on private deps
  (driver subsystem internals + agent-registry are
  `publish = false` pending security review):
    - `nexo-memory-snapshot v0.1.0` (depends on `nexo-driver-types`)
    - `nexo-fork v0.1.5` (depends on `nexo-driver-types`,
      `nexo-driver-permission`, `nexo-agent-registry`)
    - `nexo-dream v0.1.5` (depends on `nexo-agent-registry`,
      `nexo-driver-types`)
  These can publish only when the driver subsystem clears
  security review and gets `publish = true` on its own crates,
  OR when the workspace refactors to remove the dep edge. Out
  of 81.18.c scope; logged as `81.18.c.private-dep-blockers`.
  Status: ✅ for what is publishable; remaining 3 crates
  blocked structurally.

- **`81.18.c.private-dep-blockers`** — PARTIAL 2026-05-09:
  user opted "Full driver subsystem" path. Driver subsystem
  + memory-snapshot publish wave completed (6 crates):
    - `nexo-driver-types v0.1.5` ✅
    - `nexo-driver-claude v0.1.5` ✅
    - `nexo-driver-permission v0.1.5` ✅
    - `nexo-agent-registry v0.1.5` ✅
    - `nexo-driver-loop v0.1.5` ✅ (required cascade-publish
      `nexo-memory v0.1.3` for new `SecretGuard` type the
      v0.1.2 published API didn't expose)
    - `nexo-memory-snapshot v0.1.0` ✅
  Remaining 2 crates STILL BLOCKED — `nexo-fork v0.1.5` and
  `nexo-dream v0.1.5` both transitively depend on `nexo-core`,
  which depends on `nexo-dispatch-tools` (NOT published) which
  depends on `nexo-project-tracker` (NOT published). Cascade
  to unblock fork+dream requires publishing nexo-project-tracker
  + nexo-dispatch-tools + nexo-core (with `nexo-mcp v0.1.3`
  bump propagated). nexo-mcp v0.1.3 is already on crates.io
  from this wave, so `nexo-core v0.1.2` local pin is bumped
  and `crates/dispatch-tools/Cargo.toml` has all path-deps
  re-pinned to current published versions in this commit.
  Out of session budget; logged as
  `81.18.c.private-dep-blockers-fork-dream-cascade`.
  Owner: framework. Trigger: dedicated publish-wave session.
  Risk: each new crate needs README + categories review before
  first publish; nexo-dispatch-tools is internal CLI glue
  whose publishability needs an explicit go/no-go.

- **`81.18.c.private-dep-blockers-fork-dream-cascade`** —
  RESOLVED 2026-05-09. Cascade closed in the publish wave
  driven by `81.19.b.publish-github`. Crates published this
  wave (10 total):
    * nexo-config v0.1.5 (factory_type + api_key_secret_id
      fields on LlmProviderConfig)
    * nexo-llm v0.1.3 (registry.unregister)
    * nexo-memory v0.1.3 (SecretGuard) — already shipped
      in the prior wave (`81.18.c.private-dep-blockers`)
    * nexo-mcp v0.1.3 (output_schema + structured_content)
      — already shipped in the prior wave
    * nexo-core v0.1.2 (cascade — pulled all the above)
    * nexo-project-tracker v0.1.5 (first publish — Phase
      67.A PHASES.md / FOLLOWUPS.md tooling)
    * nexo-dispatch-tools v0.1.5 (first publish — internal
      CLI glue; explicit go made under "publish wave to
      unblock fork/dream + email" rationale)
    * nexo-fork v0.1.5 (first publish — agent fork primitive)
    * nexo-dream v0.1.5 (first publish — dream loop)
    * nexo-plugin-email v0.1.2 (the trigger crate)
  Workspace builds clean against the new crates.io versions;
  every internal nexo-* dep version pin in
  `proyecto/crates/{config,core,llm,fork,dream}/Cargo.toml`
  + the standalone `nexo-rs-plugin-email/Cargo.toml` was
  bumped in the same commits. Status: ✅ closed.

- **`81.19.a.release-plz-tool-meta-skip`** — RESOLVED 2026-05-09
  by the manual `nexo-tool-meta v0.1.1` publish above. release-plz
  was failing since 2026-05-07 because its diff walker replayed
  commit `e2b6f36` (which had `[patch.crates-io] wa-agent = path
  "/home/familia/whatsapp-rs"` absolute). Now that tool-meta
  is at v0.1.1 on crates.io, release-plz only diffs against
  v0.1.1 (no longer touches the v0.1.0 → e2b6f36 history).
  The override cleanup landed in `release-plz-cleanup-and-reenable`
  below. Status: ✅ closed.

- **`release-plz-cleanup-and-reenable`** — RESOLVED 2026-05-09
  (third pass — first two attempts revealed a structural
  blocker, third closed it by publishing the missing
  upstream `wa-agent v0.1.6`).

  Sequence:
    1. First push fired workflow run 25613927378 with the
       3 plugin overrides referencing non-member crates →
       "overrides are not present in workspace".
    2. Fixed by dropping those overrides; run 25614022345
       failed deeper: release-plz makes its own git clone of
       proyecto into `/tmp/.tmpXXX/proyecto/` and resolves
       `path = "../nexo-rs-plugin-X"` relative to THAT tmp
       dir; sibling checkouts were invisible.
    3. Resolution: publish `wa-agent v0.1.6` (TLS feature
       split that the published v0.1.5 lacked) → publish
       `nexo-plugin-whatsapp v0.1.3` (now buildable from
       crates.io with the v0.1.6 dep) → drop ALL workspace
       path-deps for the 3 extracted plugins → add
       `[patch.crates-io]` block redirecting every
       internal `nexo-*` crate back to local paths so the
       dep graph unifies (one copy per crate; without the
       patch, registry-pinned plugin crates would drag in
       a second copy of `nexo-core` etc., breaking trait
       identity for `WaBotHandle`, `TlsMode`, …).

  Net of this entry:
    * `release-plz.toml` reduced to 2 survivors
      (`nexo-rs` + `nexo-companion-tui`).
    * `.github/workflows/release-plz.yml` re-enabled
      `on: push: branches: [main]`. Sibling-repo checkout
      steps removed (no longer needed; workspace path-deps
      are gone).
    * Workspace `Cargo.toml` consumes the 3 extracted
      plugins via crates.io only.
    * `[patch.crates-io]` block redirects 23 internal
      `nexo-*` crates back to local paths.
    * 35 git tags backfilled in the prior commit stay.
    * `wa-agent v0.1.6` + `nexo-plugin-whatsapp v0.1.3`
      published in this wave.

  Status: ✅ closed.

- ~~**`release-plz.path-dep-walk-blocker`**~~ — **NOT FIXED**.
  Subsequent investigation 2026-05-10: even after the May 9
  publish wave + the May 10 24-crate re-publish wave that
  migrated every crate's `cargo_vcs_info.json` baseline to
  the clean commit `d1ab5ae`/`3bbe48f`, release-plz STILL
  walked the older v0.1.5 anchor (commit `5971ff7`) and
  failed because its tmp-clone diff walker can't handle
  path-deps in historical workspace state. Suspected cause:
  `--allow-dirty` flag on the publish wave marked
  `cargo_vcs_info.json` with `dirty: true`, which
  release-plz seems to discount, falling back to the
  previous published version's anchor. Re-publishing every
  crate from a clean tree would be a third 24-crate wave;
  diminishing returns.

  **Resolution: replace release-plz with cargo-release.**
  cargo-release runs in the workspace directly (no tmp
  clone), so the path-dep historical-state failure mode
  doesn't apply. New `release.toml` lives at the repo root
  with the operator-facing usage:
    ```bash
    cargo release patch -p nexo-X --execute   # bump + publish + tag + push
    ```
  release-plz workflow file kept as `workflow_dispatch:`
  only (not deleted yet — pending cleanup follow-up
  `release-plz.workflow-deletion`).

- **`release-plz.workflow-deletion`** — delete the
  `.github/workflows/release-plz.yml` file once cargo-release
  is proven on a real release for ≥ 2 weeks. Until then it
  stays available as `workflow_dispatch:` only fallback.
  Owner: ops. Status: pending.

- **`cargo-release.template-warnings`** — `cargo-release v1.1.2`
  emits `[WARN ] Unrendered {{version}}` and
  `[WARN ] Unrendered {{crate_name}}` for the configured
  `pre-release-commit-message` template. The release proceeds
  correctly (tag + push reflect the right values) but the
  commit message contains the literal `{{...}}` placeholders.
  Either upgrade cargo-release when a fix lands, or switch
  templates to the v1.x-supported variant (e.g. drop
  consolidate-commits + use the default per-crate message).
  Owner: ops. Status: cosmetic, low priority.

  **Shipped (kept):**
    * `release-plz.toml` reduced from 42 `[[package]]`
      overrides to 2 survivors: `nexo-rs` (root,
      `release = true`) + `nexo-companion-tui`
      (`release = false`, internal TUI). The 3
      standalone-plugin overrides (`nexo-plugin-{telegram,
      whatsapp,email}`) were attempted but rejected by
      release-plz with "overrides are not present in the
      workspace" — they're path-deps not members.
    * 35 git tags backfilled (`<crate>-v<version>` for
      every crate published in the manual wave).
    * `.github/workflows/release-plz.yml` sibling-repo
      checkout step for `nexo-plugin-email` added to both
      jobs (alongside the existing telegram + whatsapp
      checkouts).

  **Reverted:**
    * `on: push: branches: [main]` rolled back to
      `workflow_dispatch:` only. Two consecutive runs
      failed: workflow run 25613927378 hit the
      "overrides not present in workspace" error;
      25614022345 (after fix) hit the deeper structural
      blocker — release-plz makes its own git clone of
      proyecto into `/tmp/.tmpXXX/proyecto/` and resolves
      `path = "../nexo-rs-plugin-X"` relative to THAT
      tmp dir, so sibling checkouts in
      `$GITHUB_WORKSPACE/nexo-rs-plugin-X` are invisible
      to the tmp clone. Even probing an unrelated crate
      (nexo-config) walks the workspace dep graph and
      aborts on the unresolvable path-dep.
    * Attempted version-only deps (drop path) but the
      workspace requires `nexo-plugin-whatsapp 0.1.3`
      while crates.io only has 0.1.1 — the local 0.1.3
      carries Phase 81.20.c (typing-presence broker) +
      81.19.a.tls-rustls features the daemon depends on.
      Downgrading to 0.1.1 broke 13 nexo-setup symbols.

  Status: ✅ for cleanup of obsolete overrides; ❌
  on:push remains `workflow_dispatch:` only. Tracked as
  `release-plz.path-dep-walk-blocker` follow-up below.

- **`release-plz.path-dep-walk-blocker`** — release-plz
  cannot resolve workspace path-deps that point outside
  the repo's git checkout (`../nexo-rs-plugin-X`). Three
  unblocking paths, each with its own cost:
    1. **Publish nexo-plugin-whatsapp 0.1.3** — currently
       blocked on `wa-agent` git-dep lacking a crates.io
       version (`cargo publish` rejects deps without
       version reqs). Sub-blocker:
       `wa-agent.crates-publish` (upstream).
       After publish, drop workspace path-dep.
    2. **Add release-plz config knob to inject siblings
       into its tmp clone** — upstream feature request to
       `MarcoIeni/release-plz`. PR effort ~1d.
    3. **Vendor sibling repos as git submodules** — keeps
       path-deps but submodule init runs in release-plz
       tmp clone. Loses the "siblings publish independently"
       property — they re-couple to monorepo lifecycle.
  Owner: framework. Trigger: release-plz on:push
  automation desired. Status: pending. Workaround until
  then: `gh workflow run release-plz.yml` for one-shot
  manual runs (gracefully degrades to manual `cargo publish`
  for emergency releases).

- **`81.18.b.qr-event-bridge`** — daemon's pairing state
  subscriber expects subprocess plugins to publish
  `InboundEvent::Qr { ascii, png_b64, expires_at }` on the
  inbound topic so the `kind=qr` arm in
  `spawn_whatsapp_pairing_state_subscriber` populates the
  daemon-owned `PairingState`. The standalone whatsapp
  v0.1.2 manifest emits `Qr` via lifecycle.rs — verify the
  exact wire shape (field naming, payload shape) end-to-end
  on a real bot before declaring 81.18.b.2 production-ready.
  Owner: framework. Trigger: live pairing flow smoke test.
  Status: pending live verification.

- **`81.19.a.tls-rustls`** — `wa-agent` upstream uses
  `native-tls` (OpenSSL) via its own reqwest dep; this repo's
  direct reqwest dep uses `rustls-tls`. Both stacks live in
  the binary today, slightly bloating size. The clean fix is
  asking the wa-agent maintainer to expose a `rustls-tls`
  feature flag — without it the Android NDK build (Phase 90)
  needs pre-built OpenSSL for the target. Owner: ops + wa-
  agent upstream. Trigger: Phase 90 mobile cross-compile.
  Status: pending upstream conversation.

- **`81.19.a.publish-github`** — push the local repo to
  `github.com/lordmacu/nexo-plugin-whatsapp` (PUBLIC) +
  tag `v0.1.2` so the release workflow can build linux-x64
  / macos-arm64 binaries. Owner: ops. Status: pending; the
  initial commit lives in the local repo only.

- **`81.19.a.crates-publish`** — same as 81.18.c —
  blocked on the proyecto crates publish wave (5 internal
  deps still pending crates.io). External operators install
  via `cargo install --git https://github.com/lordmacu/nexo-plugin-whatsapp`
  in the meantime. Owner: ops. Status: blocked.

- **`81.19.a.voice-codec`** — feature-gate the whisper
  transcribe path so the embedded build can drop ~25MB of
  binary. Today `transcriber.rs` ships unconditionally with
  the lib. Owner: framework. Trigger: Phase 90 mobile binary
  size budget. Status: pending.

- **`81.19.a.e2e-test-fixture`** — same gap as
  `81.18.f.e2e-test-fixture`: the offline e2e_handshake test
  verifies wire shape but doesn't exercise the live wa-agent
  / Signal Protocol round-trip. A wiremock-served Bot API +
  Signal-state fixture would close the loop. Owner:
  framework. Trigger: when 81.18.b ships. Status: pending.

- **`81.19.b.email-extract`** — RESOLVED 2026-05-09. Email
  plugin moved out-of-tree to `../nexo-rs-plugin-email/`
  (`nexo-plugin-email v0.1.2`), mirror layout of telegram /
  whatsapp extracts (`[lib]` + `[[bin]]`, `nexo-plugin.toml`
  at repo root, `Cargo.lock` committed). DORMANT marker
  removed. Daemon flip:
    * `proyecto/src/main.rs` legacy `plugins.register_arc`
      block dropped (lines 2632-2655 in pre-extract HEAD).
    * Replaced with `factory_registry.register("email",
      singleton-factory)` that hands the existing
      `Arc<EmailPlugin>` to the init loop. Factory wins
      over discovery's auto-subprocess fallback per
      `init_loop.rs:417`, so a manifest in `search_paths`
      is harmless.
    * `seed_email_subprocess_env_for(broker, config_path,
      secrets_dir, data_dir, google_auth_path)` helper
      added near `seed_telegram_subprocess_env_for`.
  Workspace member removed; `[workspace.dependencies]
  nexo-plugin-email` redirected to the standalone path.
  Test count: 5843/5843 workspace + 236/236 standalone.
  Three deferreds logged below
  (`81.19.b.tool-dispatch-subprocess`,
  `81.19.b.subprocess-flip-conditional-toggle`,
  `81.19.b.publish-github`).

- **`81.19.b.tool-dispatch-subprocess`** — the standalone
  subprocess advertises **zero tool defs** in its
  `initialize` reply because the 12 email tools share heavy
  in-process state (IMAP IDLE workers, SMTP queue, SQLite
  stores) that doesn't translate cleanly to JSON-RPC
  dispatch. Tool dispatch stays on the in-process lib
  surface (`register_email_tools_filtered`). Trigger: a
  non-daemon consumer (mobile embedded client without a
  daemon, or a microservice that wants tools but no
  in-process plugin). Owner: framework. Status: pending.

- **`81.19.b.subprocess-flip-conditional-toggle`** — the
  daemon currently registers the email factory unconditionally
  whenever `cfg.plugins.email` resolves to a non-empty
  account list. Operators that want subprocess isolation
  must edit `proyecto/src/main.rs` to strip the factory
  registration. A future `email.runtime: in_process |
  subprocess` config field would make the toggle
  declarative. Owner: framework. Trigger: an operator
  asks for the toggle. Status: pending.

- **`81.19.b.publish-github`** — RESOLVED 2026-05-09.
  Pushed to `git@github.com:lordmacu/nexo-plugin-email.git`
  + published `nexo-plugin-email v0.1.2` to crates.io. The
  publish wave that unblocked it bumped:
    * nexo-config 0.1.4 → 0.1.5 (factory_type +
      api_key_secret_id fields on LlmProviderConfig)
    * nexo-llm 0.1.2 → 0.1.3 (registry.unregister)
    * nexo-memory 0.1.2 → 0.1.3 (SecretGuard)
    * nexo-mcp 0.1.2 → 0.1.3 (output_schema +
      structured_content)
    * nexo-core 0.1.1 → 0.1.2 (cascade)
    * nexo-project-tracker 0.1.5 (first publish)
    * nexo-dispatch-tools 0.1.5 (first publish)
    * nexo-fork 0.1.5 (first publish)
    * nexo-dream 0.1.5 (first publish)
  Resolves `81.18.c.private-dep-blockers-fork-dream-cascade`
  in the same wave (see entry below).

- **`81.19.a.release-workflow-sibling-checkout`** — three
  release-only workflows (`docker.yml`, `release.yml`,
  `sbom.yml`) still assume the proyecto repo is the only
  checkout. `docker.yml` builds from `Dockerfile` with
  `context: .` (path-deps to siblings live outside the
  context); `release.yml` fires on tag push and uses
  `cargo dist` against the workspace (same path-dep
  resolution issue); `sbom.yml` walks `cargo metadata`
  after a successful release. None auto-fire on every
  commit, so they didn't block the active CI gate, but the
  next `nexo-rs-v*` tag push will explode unless these
  three are updated to (a) check out
  `nexo-plugin-{telegram,whatsapp}` as siblings + (b) the
  Dockerfile copies the siblings into its build context.
  Owner: ops. Trigger: before the next release tag.
  Status: pending.

### Locale-aware agent language — shipped, follow-ups open

BCP-47 locale model + per-locale addenda + voice picker shipped
in commits `bd7e0cf..723227f` (proyecto + agent-creator-microapp
main). Six deferreds:

- **`locale-config-yaml-strict-validation`** — RESOLVED
  2026-05-10. `Locale` moved from `nexo-microapp-sdk::locale`
  to `nexo-tool-meta::locale` (closes the layer inversion;
  both `nexo-config` and `nexo-microapp-sdk` depend on
  `nexo-tool-meta` already). `nexo-config::AgentConfig.language`
  + `InboundBinding.language` gain
  `#[serde(deserialize_with = "deserialize_locale_string")]`
  that runs `Locale::from_str` on Some(s) and surfaces parse
  errors as serde custom errors. SDK keeps a re-export shim
  (`pub use nexo_tool_meta::locale::*;`) so existing
  call-site imports resolve unchanged. 6/6 tests in
  `locale_yaml_validation_tests` pass.

- **`locale-list-codegen-or-lint`** — RESOLVED 2026-05-10.
  Lint script over codegen (lighter touch). New
  `crates/microapp-sdk/src/bin/locale_dump.rs` prints
  `{ "supported": [...] }` JSON of the 144-entry Rust
  accept-list (LangCode × (None ∪ RegionCode) cross-product).
  New `scripts/lint-locale-list-sync.sh` greps `code: "..."`
  literals from
  `agent-creator-microapp/frontend/src/data/locales.ts`
  and asserts every TS code is recognised by the Rust
  parser (subset relation: TS curated ⊆ Rust accept-list).
  CI step appended in `.github/workflows/ci.yml` after
  `cargo fmt --check`. Mirrors OpenClaw's
  `research/src/i18n/registry.test.ts:24-39` drift-lint
  pattern but cross-language.

- **`locale-set-extension-additional-languages`** — STILL
  DEFERRED. Korean (ko-KR), Russian (ru-RU), Arabic
  (ar-SA / ar-EG / ar-MA), Hindi (hi-IN), Turkish
  (tr-TR), Vietnamese (vi-VN), Thai (th-TH), Polish
  (pl-PL), Dutch (nl-NL), Indonesian (id-ID). Why:
  each language needs a curated style addendum + voice
  id table entry from a native speaker / linguistic
  source — adding the variant alone gives the operator
  a "supported" locale that can't actually voice-reply.
  Trigger to ship: a concrete operator demand. OpenClaw
  includes Korean (`research/ui/src/i18n/lib/registry.ts`
  `SUPPORTED_LOCALES`) — first language to add when
  demand surfaces.

- **`locale-script-subtags`** — STILL DEFERRED. `zh-Hant`,
  `sr-Cyrl`, `mn-Mongl` rejected as `TooManySubtags`.
  Why: heavy refactor (`Locale` + `ScriptCode` 3-tuple
  + match arms across every voice/style/addendum table).
  No operator demand. Re-evaluate when first script-aware
  locale request arrives.

- **`locale-inbound-binding-override`** — RESOLVED 2026-05-10.
  `OutboundReplyContext.language` now sources from
  `EffectiveBindingPolicy.language` (which already
  implements `binding > agent > None` precedence inside
  `effective.rs::resolve_language`) instead of always
  `ctx.config.language`. Wire change is one line in
  `crates/core/src/agent/llm_behavior.rs:2237`. Two new
  tests in `effective.rs::tests` cover the override and
  fallback paths.

- **`locale-stt-input-hint`** — RESOLVED 2026-05-10.
  `BindingContext` (in `nexo-tool-meta::binding`) gains
  `language: Option<String>`. Daemon-side
  `binding_context_from_effective` populates it from
  `EffectiveBindingPolicy.language`. SDK-side
  `InboundTransformHandler::call` reads
  `ctx.binding.language`, parses via `Locale::from_str`,
  trims to ISO-639-1 (`Locale::language().as_str()` —
  `"es-AR"` → `"es"`) which is the format whisper's
  `set_language` accepts, clones the per-handler
  `TranscribeConfig` once, and overrides `lang_hint`.
  `None` falls through to whisper auto-detect (pre-
  existing default). Trim invariant covered by 1 new
  test in `nexo-tool-meta::locale::tests` over 8
  lang+region pairs.

### WhatsApp recording-presence indicator — shipped, follow-ups open

Recording presence + `typing_mode` YAML knob landed across
`whatsapp-rs` (commits `a6e66c7..3970dcc` on master) and
`nexo-plugin-whatsapp` (commits `4a3f3cc..0bf96b2` on main).
Open work the v1 deliberately punted:

- **`whatsapp-typing-mode-thinking-message-impl`** — the
  `Thinking` and `Message` variants of `TypingMode` parse
  successfully but warn-degrade to `Instant` at runtime
  (`whatsapp-rs/src/agent.rs::run_agent_full`,
  inside the auto_typing block). Implementing them needs the
  agent runtime to surface a "first reasoning delta" signal
  (Thinking) and a "first non-silent text delta" signal
  (Message). OpenClaw's `research/docs/concepts/typing-indicators.md`
  documents the intended semantics; porting the actual delta
  detection requires plumbing through `nexo-core`'s LLM stream
  events into the bridge, then forwarding into wa-agent. Why
  deferred: the user's reported symptom ("grabando audio")
  doesn't need it; the YAML accepts the values today so a
  future enable doesn't break configs.
  How to apply: replace the warn-fallback in the
  `match opts.typing_mode { Thinking | Message => ... }` arm
  with actual deferred-start signalling.

- **`whatsapp-presence-hint-inbound-to-outbound`** — when
  `voice_mode` is toggled ON for a conversation, the peer phone
  still sees "escribiendo…" during the LLM round-trip and only
  flips to "grabando audio…" for the ~250 ms before the upload
  + the upload itself. Showing "grabando audio…" the entire
  time would require `voice_mode_inbound_transform` to emit a
  presence hint that propagates through `OutboundReplyContext`
  (or a new sibling field) so the bridge can start the
  `PresenceHandle` with `Audio` from the first pulse. Why
  deferred: changes the wire of `OutboundReplyContext`
  (cross-channel struct in `nexo-tool-meta`) and forces every
  reply transformer to opt in. Cosmetic UX vs. real schema
  change.
  How to apply: extend `OutboundReplyContext` with
  `presence_hint: Option<ChatPresenceMedia>`; have
  `voice_mode_inbound_transform` populate it; have
  bridge.rs read it and pass through `RunAgentOpts` initial
  media kind.

- **`whatsapp-voice-paths-consolidation`** — voice notes flow
  through TWO outbound paths today: `dispatch.rs` voice_note
  arm (proactive, microapp-driven) and `Response::VoiceNote`
  via wa-agent's `apply_response` (inbound-driven, theoretical
  — the bridge oneshot drops in `dispatch.rs:155` when
  payload kind is non-text, so currently inbound voice notes
  ALSO route through dispatch.rs). Both wrap the send in
  identical `Composing(Audio) → send → Paused` presence emits,
  but two parallel implementations risk drift. Consolidating
  to a single path would either (a) make `bridge_step`
  carry richer payload than `Option<String>` so the bridge
  handler can return `Response::VoiceNote` and apply_response
  takes over, or (b) drop apply_response for voice notes and
  always route through dispatch.rs. Why deferred: the v1
  behaviour is correct on both paths today; consolidation is
  a refactor, not a bug fix.
  How to apply: option (a) — change `bridge_step` return
  type, drop the `payload.kind != "text"` cancel in
  `dispatch.rs:155`, return `Response::VoiceNote` from
  `bridge.rs`. Option (b) — strip the apply_response
  voice_note arm + Response::VoiceNote variant from wa-agent
  (BREAKING wa-agent API).

- **`wa-agent-run-agent-with-transcribe-and-opts`** — the
  `Session::run_agent_with_transcribe` entry point doesn't
  accept `RunAgentOpts`. nexo-plugin-whatsapp's transcribe
  branch (`plugin.rs::run_agent_with_transcribe(acl, t, handler)`)
  currently ignores the configured `typing_mode` knob and
  falls back to `RunAgentOpts::default()`. Why deferred: the
  transcribe path is rarely combined with `typing_mode: never`
  in practice; the daemon log evidence in step 21 didn't
  exercise it.
  How to apply: add `Session::run_agent_with_transcribe_and_opts`
  in whatsapp-rs (mirror of `run_agent_with_opts` with the
  transcriber slot), thread it through plugin.rs.

- **`whatsapp-rs-presence-handle-time-mock-tests`** — the TTL
  + circuit-breaker branches in
  `Session::chat_presence_heartbeat_with` are currently only
  exercised at runtime (smoke test in step 21). Adding mocked
  socket + `tokio::time::pause()` tests would catch
  regressions without a phone. Why deferred: whatsapp-rs has
  no shared mock-socket fixture today. Building one is
  scope-creep for v1.
  How to apply: introduce a minimal mock `SocketSender` in
  `whatsapp-rs/tests/`, drive `chat_presence_heartbeat_with`
  with `cfg.max_duration = 100ms` + `cfg.max_consecutive_failures = 1`
  fixtures, assert the loop exits.

- **`whatsapp-bridge-test-pre-existing-break`** — RESOLVED
  during the Phase 81.19.a extraction wave (whatsapp plugin
  moved out-of-tree to `../nexo-rs-plugin-whatsapp/`). Verified
  2026-05-10: `cargo nextest run` from
  `/home/familia/chat/nexo-rs-plugin-whatsapp/` passes 54/54
  tests including `bridge_test::*`. The extract refactor
  picked up the missing struct fields. Audited 2026-05-10.
  green.

- **`whatsapp-rs-live-wa-test-duplicate-fields`** — RESOLVED
  during the Phase 81.19.a extraction wave (whatsapp plugin
  moved out-of-tree to `../nexo-rs-plugin-whatsapp/`).
  Verified 2026-05-10: `cargo nextest run` from
  `/home/familia/chat/nexo-rs-plugin-whatsapp/` builds and
  passes 54/54 tests including `live_wa_test::*`. The extract
  refactor dropped the duplicate fields. Audited 2026-05-10.

### Phase 82.10.u — schema-driven LLM provider wizard 🟢 shipped, follow-ups open

Phase 82.10.u introduced declarative credential schemas + OAuth
endpoints. Operators can now mint MiniMax instances with
`group_id`, validate keys before persistence, and authorise
Anthropic Claude.ai subscriptions from the SPA.

Shipped commits (`3b24a83..e2b6f36` + `aed54b5` + `1ab01cb` +
`0f982c7` + `aa46f27`):
1. `nexo-llm-auth` crate extracted (PKCE + anthropic + minimax +
   bundle + verifier_store, 26 unit tests).
2. `tool-meta` wire shapes: CredentialFieldDescriptor, AuthMode,
   FieldKind, FieldValidation, DependsOn, LlmProviderError,
   OAuthStartInput / OAuthFinishInput / responses (8 tests).
3. `LlmProviderFactory` trait gains `credential_schema +
   supported_auth_modes + supports_models_probe` (6 tests, 4
   factories migrated: minimax/anthropic/openai/deepseek/gemini).
4. Schema-driven `upsert` handler with FactorySchemaLookup +
   typed LlmProviderError data + audit redaction (6 tests).
5. `probe_draft` endpoint (HTTP impl in HttpLlmProviderProbe).
6. OAuth `start/finish` endpoints with VerifierStore single-use +
   TTL sweep (60s) wired in `admin_bootstrap`.
7. Microapp 0.0.45 primitives: CredentialFieldRenderer + OAuthPane
   + useOAuthFlow zustand store.
8. Docs: `docs/src/llm/credential-schema.md` +
   `docs/src/llm/oauth-flows.md`.

Open follow-ups:

- **82.10.u.wizard-state-machine** — ✅ shipped in microapp 0.0.46
  (`fabb16d`). 5-stage state machine: factory_pick →
  fill_credentials → validate → pick_model → saving, with OAuth
  branch. CredentialFieldRenderer + OAuthPane + StageBreadcrumb
  wired. probe_draft gates pick_model; legacy single-api_key
  fallback retained for pre-82.10.u factories.
- **82.10.u.e2e-tests** — `crates/setup/tests/llm_provider_schema_e2e.rs`
  + `llm_oauth_flow_e2e.rs` (5 cases each, wiremock for upstream)
  not yet written. Existing `llm_multi_instance_e2e.rs` exercises
  the legacy path; the schema-driven + OAuth paths only have unit
  coverage in `nexo-core` + `nexo-llm-auth`.
- **82.10.u.google-oauth** — Google / Gemini OAuth (offline
  refresh) deferred — separate sub-phase. Existing flow lives in
  `crates/plugins/google` and uses a different shape (refresh
  token offline).
- **82.10.u.bundle-as-secret** — OAuth `oauth_finish` writes the
  bundle JSON via `SecretsStore.write(name, &str)` which lands at
  `<secrets_dir>/<id>.txt`. The Anthropic runtime reads from
  `auth.bundle: <path>` so the file extension is misleading. Move
  to `SecretsStore.write_bytes(name, ext, bytes)` so the bundle
  lands at `.json`. Cosmetic; functionality unaffected.
- **82.10.s.probe-server-cache** (open since 82.10.s+t) — move
  the 60s probe cache from frontend into daemon for shared dedup.
- **82.10.u.anthropic-live-probe** — ✅ shipped 2026-05-05.
  `AnthropicFactory::supports_models_probe → true`, daemon-side
  `HttpLlmProviderProbe` branches on factory id and uses
  `AnthropicAuth::resolve_headers` for the right header set
  (`x-api-key` for legacy keys, `Authorization: Bearer` +
  `anthropic-beta: oauth-2025-04-20` for OAuth subscription) plus
  `anthropic-version: 2023-06-01`. OAuth modes in `probe_draft`
  short-circuit to "fall back to catalog" (bundle path isn't in
  the draft payload). After upsert, frontend can re-probe via
  `probe(provider_id)` to refresh the live model list against
  the persisted bundle.
- **runtime.turn-stuck-pre-llm** — second user-triggered turn
  hangs indefinitely between `agent turn started` and
  `anthropic request` (no LLM call ever fires; no error; no
  timeout). Reproduces with: agent `aana` answered "Hola"
  cleanly in 2s (turn 1), then "Cómo estas" (turn 2) issued
  `agent turn started` and never produced any further log line
  for that `message_id`. Daemon restart clears it. Microapp
  stderr emitted `firehose store: skipping unknown event variant`
  3 times in the same window, suggesting an unrecognized
  agent_event variant the microapp's firehose persistence layer
  drops silently. Hypothesis: pre-LLM phase (history load /
  tool collection via stdio RPC to microapp / compaction check)
  parks on an IPC future the microapp never resolves because it
  bailed out of the unknown-variant branch without acking.
  Live incident 2026-05-05.

  Action items:
  1. Add `tracing::debug!` spans for each pre-LLM step in
     `nexo-core::agent::llm_behavior` so the next hang pinpoints
     the parked future (history load / tool fetch / hook /
     compaction).
  2. Update microapp firehose handler to ack-and-skip unknown
     variants instead of silent drop (or at minimum log the
     variant name).
  3. Add a per-turn pre-LLM watchdog (e.g. 30s) that bails out
     to DLQ + `tracing::error!` so future hangs surface as
     errors rather than silent voids.

- **82.10.p.b.client-reload** — `WhatsappPairingTrigger::start()`
  (Phase 82.10.p.b, commit `cc47c80`) wipes `.whatsapp-rs/`
  before `pair_with_callback` so the next session is clean on
  disk, but the in-memory `whatsapp_rs::Client` instance the
  plugin holds is NOT rebuilt — it keeps the prior session's
  identity (`key_index=N`). After pairing succeeds and writes
  `key_index=N+1` to creds.json, the server's normal post-
  pairing `stream:error code=515` triggers a reconnect; the
  reconnect uses the stale in-memory `key_index=N` → 401 → loop
  until the daemon process restarts. Fix: after
  `pair_with_callback` returns Ok, drop + recreate the Client
  bound to the freshly-written creds.json so reconnects use the
  new identity. Live incident 2026-05-05: operator paired
  successfully (creds.json `me.id=…:41`) but daemon kept
  reconnecting as `…:40` until manual restart.

- **82.10.u.oauth-finish-seed-defaults** — `nexo/admin/llm_providers/oauth_finish`
  currently writes `auth.mode + auth.bundle` to yaml as a side
  effect (so a subsequent `upsert` only needs metadata + model).
  But if the operator abandons the wizard between `oauth_finish`
  and `upsert`, the yaml entry is partial: it has `auth.*` but
  **lacks `base_url` and `factory_type`**, both of which are
  required by `LlmProviderConfig` (deny_unknown_fields, base_url
  has no Default). Result: daemon refuses to boot on next start
  with `missing field 'base_url'`. Fix: oauth_finish should also
  seed `base_url = factory.default_base_url()` and
  `factory_type = factory.name()` so the partial yaml is valid
  on its own. Live incident 2026-05-05: operator did 2 OAuth
  flows, abandoned both, yaml became unparseable, daemon
  wouldn't restart until orphan entries were patched manually.

- **82.10.u.preflight-model-validation** — write-time gate at
  `nexo/admin/llm_providers/upsert`: before persisting the
  instance, fire a 1-token `messages` (or factory-equivalent)
  request with the operator-chosen model. Reject upsert with
  typed `ModelNotAvailableForTier` when upstream returns 404 /
  "model not found", surfacing a clear "tu suscripción no tiene
  acceso a `<model>`, prueba `<fallback>`" hint in the wizard.
  Still relevant after 82.10.u.anthropic-live-probe: even live
  `/v1/models` returns the API-tier catalog (not the
  subscription-tier catalog), so the OAuth-bundle Claude Code
  path can still surface models the operator can't actually
  call. Preflight closes the gap by validating the chosen model
  end-to-end. Reference: `claude-code-leak/src/services/api/bootstrap.ts:63-90`
  shows Claude Code uses internal `/api/claude_cli/bootstrap`
  (not public `/v1/models`) precisely because the public
  catalogue isn't tier-filtered. Don't depend on that internal
  endpoint — preflight at write time is provider-agnostic.

### Phase 82.10.s + 82.10.t — multi-instance LLM providers + dynamic models 🟢 shipped, 1 follow-up open

Phase 82.10.s split `factory_type` (registered `crates/llm/src/<id>.rs`
factory) from instance id (yaml key under `llm.yaml.providers.*`) so
operators can run N MiniMaxes with separate keys for billing
isolation across microapps / tenants. Boot resolver
(`LlmConfig::resolve_all_keys` + `LlmRegistry::validate_config`)
collects all errors per-instance. Admin RPC `llm_providers/upsert`
write-throughs `api_key_secret_value` into `FsSecretsStore` under
`LLM_<INSTANCE>` and patches yaml with `api_key_secret_id`. Audit
redactor masks the cleartext.

Phase 82.10.t added `model_names` to `llm_providers/probe` parsed
from OpenAI-compat `/v1/models data[].id` (capped 200) so SPA
wizards show the live list a key actually has access to.

E2E coverage in `crates/setup/tests/llm_multi_instance_e2e.rs` (2
cases: write-through + resolve + registry validate; conflicting
sources rejected). Docs in `docs/src/llm/multi-instance.md`.

Open follow-up:

- **82.10.s.probe-server-cache** — the 60 s probe cache lives only
  in the frontend (`useLiveModels` zustand store). Two operator
  tabs each issue their own probe call against the upstream
  provider. Move the cache server-side in
  `crates/setup/src/llm_provider_probe.rs::HttpLlmProviderProbe`
  so the daemon dedupes across all SPA instances. ~50 LOC.

### Phase 82.10.p.b — wipe stale signal session before pairing ✅ shipped 2026-05-05

Live deployment surfaced the "auto-paired without QR" symptom:
operators who deleted the credential YAML AND unpaired the
device from WhatsApp app side STILL got dropped on the green
"✅ Dispositivo emparejado" pane on the next `pairing/start` —
no QR, no escape. Root cause: wa-agent's `Client::new_in_dir`
silently resumes any signal session under
`<session_dir>/.whatsapp-rs/`; a credential revoke only touches
the YAML, never the on-disk session.

`WhatsappPairingTrigger::start` now wipes
`<session_dir>/.whatsapp-rs/` before invoking
`pair_with_callback`, so every `pairing/start` produces a fresh
QR. Belt-and-suspenders: the trigger also tracks whether
`on_qr` ever fired before `connect()` resolved Ok; if not (rare
race where the wipe failed), the challenge is flipped to
`Expired` with a clear `data.error` instead of the misleading
`Linked`.

Shipped in `nexo-plugin-whatsapp` v0.1.2.

### Phase 82.10.p — admin pairing → channel plugin bridge ✅ shipped

Resolved across commits `54e394a..fab0231` (8 atomic steps):
1. `PairingChannelTrigger` trait + types — commit `54e394a`.
2. `PairingChallengeStore::update_qr / update_state` — commit `285a059`.
3. Dispatcher wires triggers map + handles registry — commit `efd89b9`.
4. `pair_with_callback` in `nexo-plugin-whatsapp::session` exposes
   each `wa-agent` `on_qr` rotation as `(qr_png_b64, qr_ascii,
   expires_at_ms)` — commit `6537988`.
5. `WhatsappPairingTrigger` impl spawns the wa-agent flow,
   pushes QR rotations into the store + notifier, observes
   cancel via `tokio::select!` — commit `f790088`.
6. `AdminBootstrapInputs.pairing_triggers` field + `main.rs`
   wires the production map from `cfg.plugins.whatsapp` —
   commit `4c6c0cf`.
7. End-to-end integration test in `nexo-setup` (4 cases:
   happy / unsupported channel / trigger reject + rollback /
   cancel propagation) — commit `fab0231`.
8. Smoke test daemon-mode evidence: `pairing/start whatsapp`
   resolved to `state: qr_ready` with 9.5 KB `qr_png_base64`
   + ASCII art payload after wa-agent connected to
   `wss://web.whatsapp.com/ws/chat` and received the
   `pair-device` node. `pairing/cancel` flipped state to
   `cancelled` and the trigger task exited cleanly.

Deferred follow-ups carried forward:

- **82.10.p.runtime-pairing-mode** — plugin runtime
  (`crates/plugins/whatsapp/src/session.rs:131-143`) still
  refuses to boot when `creds.json` is absent. Smoke test
  worked around this by setting
  `whatsapp.yaml.enabled: false` so only the trigger
  registers (the runtime never boots). Production
  multi-tenant SaaS needs a "pairing-only" plugin mode that
  boots without creds and no-ops the outbound dispatch path
  until the user completes pairing. Without it, operators
  must toggle `enabled: true` AFTER the first scan and
  hot-reload to start sending — workable for setup wizards,
  rough edge for self-serve onboarding.
- **82.10.p.handle-ttl-eviction** — the dispatcher inserts
  handles into a `DashMap` but
  `InMemoryPairingChallengeStore::prune_expired` doesn't
  cross-reference and call `handle.abort()`. Long-running
  daemons with many abandoned challenges leak the spawned
  trigger tasks until process restart. Wire is straight:
  pass the handles map into the prune sweep + abort each
  evicted entry. ~30 LOC.
- **82.10.p.device-jid-on-linked** — `pair_with_callback`
  resolves with `PairingOutcome::default()` (no `device_jid`).
  When wa-agent surfaces it post-connect (already on the
  whatsapp-rs roadmap per `/home/familia/whatsapp-rs`), the
  trigger should plumb it into `PairingStatusData.device_jid`.
- **82.10.p.recovery-no-qr-path** — wa-agent's "creds existed,
  recovered without QR" path
  (`research/extensions/whatsapp/src/login-qr.test.ts:151-176`)
  is not exercised today. Trigger should branch on the
  missing on_qr callback inside `connect()` and flip directly
  to `Linked` instead of staying `Pending`.

Telegram-link-style pairing (no QR — confirm via deep link)
plugs into the same `PairingChannelTrigger` trait without
admin handler changes; new channel impls can be added in
isolation under `crates/plugins/<channel>/pairing_trigger.rs`.

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
- **81.7 ✅ shipped 2026-05-03** —
  `merge_plugin_contributed_skills` fn in
  `nexo_core::agent::nexo_plugin_registry::contributes_skills` walks
  each loaded plugin's `skills.contributes_dir`, indexes any subdir
  containing `SKILL.md`, records `(plugin_id → root)` +
  `(skill_name → plugin_id)` first-plugin-wins attribution + per-
  plugin list. `SkillConflict` simple struct (no resolution enum —
  only one outcome). `SkillLoader` extended with `plugin_roots`
  + `with_plugin_roots(roots)` builder; `candidate_paths()` appends
  plugin roots AFTER tenant/global/legacy so operator wins by
  search order. NO `allow_override` for skills (security: skills
  exec subprocesses). `NexoPluginRegistrySnapshot.skill_roots`
  for runtime routing. `PluginDiscoveryReport` extended with
  `contributed_skills_per_plugin` + `skill_conflicts` (additive
  serde, backward-compat with 81.5/81.6). 6 unit + 1 integration
  test. **Library-only ship**: boot wire + doctor CLI sections
  land in the deferred bundle alongside 81.5.b/81.6 wires.
- **81.8 ✅ shipped 2026-05-03** — minimal `ChannelAdapter` async
  trait (4 métodos: kind / start / stop / send_outbound) + typed
  `OutboundMessage { Text | Media | Custom(serde_json::Value) }` +
  `OutboundAck { message_id, sent_at_unix }` + `ChannelAdapterError`
  thiserror enum (Connection / Authentication / Recipient /
  RateLimited / Unsupported / Other). `ChannelAdapterRegistry` con
  `std::sync::RwLock<BTreeMap<String, AdapterEntry>>` + register /
  get / kinds / has_any methods + first-registers-wins-rest-rejected
  semantic via `ChannelAdapterRegistrationError::KindAlreadyRegistered`
  (NOT first-plugin-wins like 81.6/81.7 — channels compete por broker
  topic exclusivity). `PluginInitContext` extendido con field
  `channel_adapter_registry: Arc<ChannelAdapterRegistry>`.
  `DiscoveryDiagnosticKind` extendido con
  `ChannelKindAlreadyRegistered { channel_kind, prior_registered_by,
  attempted_by }` variant. Legacy `Plugin` trait sin tocar
  (whatsapp/telegram/email/browser); migración a `ChannelAdapter`
  es 81.12 cuando se haga. 6 unit tests + 2 integration tests.
  **Library-only ship**: boot wire (cómo el agent runtime's
  outbound dispatcher consulta el registry) + doctor CLI section
  CHANNEL ADAPTERS land en el deferred bundle alongside
  81.5.b/81.6/81.7 sites.
- **81.9 ✅ shipped 2026-05-03** —
  `wire_plugin_registry(&mut cfg, discovery_cfg, version) ->
  WirePluginRegistryOutput { registry, skill_roots,
  channel_adapter_registry }` helper en
  `nexo_core::agent::nexo_plugin_registry::boot`. 4-step atomic
  pipeline (discover → merge_agents → merge_skills → init_loop
  con empty handles → all NoHandle), folds three reports en single
  snapshot, single tracing::info summary con 8 fields.
  `LlmAgentBehavior` gana `plugin_skill_roots: Vec<PathBuf>` field
  + `with_plugin_skill_roots(roots)` builder; `prepare_system_prompt`
  chains `.with_plugin_roots(...)` onto `SkillLoader::new` — operator
  priority preserved by candidate_paths search order. `src/main.rs`
  replaces existing 81.5.b block (lines 1928-1954) con single
  helper call. `doctor_render` module ships `render_text` +
  `render_json` + `determine_exit_code` (8 unit tests covering
  empty / loaded / diagnostics / agent conflicts / init outcomes /
  exit code rules / JSON shape / EXIT-line termination). 1
  integration test verifies full pipeline. **Out of scope (deferred
  follow-up)**: `Mode::DoctorPlugins` CLI subcommand + parser arm
  + `cmd_doctor_plugins` handler — main.rs's pre-existing
  in-progress work (nexo-tool-meta diagnostics, microapp-http
  modules) made the CLI surgery risky for this commit. Doctor CLI
  ships in 81.9.b once the working tree quiets. Reduction "~500
  LOC boot wire" original goal stays open: requires legacy
  whatsapp/telegram/email/browser migration to `NexoPlugin`
  trait — that's Phase 81.12.
- **81.10 ✅ shipped 2026-05-03** —
  `register_plugin_registry_reload_hook(coord, registry,
  discovery_cfg, version)` helper en
  `nexo_core::agent::nexo_plugin_registry::boot`. Pushes one
  `PostReloadHook` (sync, captured-state-only) que re-corre
  `discover()` + atomic `registry.swap()` + `tracing::info` summary
  con prev/new/delta de loaded + invalid. Hook errors swallowed
  per coord's best-effort contract. Test-only `#[cfg(test)]`
  helpers en `config_reload.rs` (`post_hooks_len_for_test` +
  `fire_post_hooks_for_test`). 3 unit tests cubren register-pushes-one
  / hook-replaces-snapshot / hook-swallows-discover-failure. Boot
  wire: 1 línea en `src/main.rs::Mode::Run` antes del
  `reload_coord.start(...)` call. Snapshot's `skill_roots` queda
  empty post-reload — running agents stay con sus boot-time
  `LlmAgentBehavior.plugin_skill_roots`. Phase 18 limitation
  (agent removal at runtime not supported) preservada — disabling
  un plugin orphans su contributed agent hasta restart. **Out of
  scope (deferred 81.10.b)**: skill_roots rebuild + live
  `discovery_cfg` updates (cambios a search_paths require restart
  hoy) + per-agent `plugin_skill_roots` re-clone. Cuando 81.7.b /
  81.12 shippeen el manifest-driven `Arc<dyn NexoPlugin>` factory,
  ese phase agrega su slice al hook (re-init plugins on reload).
- **81.11 ✅ shipped 2026-05-03** —
  `capability_aggregator` module en
  `nexo_core::agent::nexo_plugin_registry`. `aggregate_plugin_gates(snapshot, core_env_vars, available)`
  itera plugin manifests + cataloga capability_gates en
  `BTreeMap<env_var, AggregatedGate>` con runtime evaluation por
  `GateKind` (Boolean/Allowlist via env::var; CargoFeature siempre
  Disabled). Conflict detection at aggregate time: vs core
  INVENTORY → `CapabilityGateConflictsCore` Error; cross-plugin
  → `CapabilityGateConflictsPlugin` Error (first-plugin-wins);
  unmet `requires.nexo_capabilities` → `RequiredCapabilityNotGranted`
  Warn (graceful degraded). 3 nuevas variants en
  `DiscoveryDiagnosticKind`. `PluginDiscoveryReport` extiende con
  `plugin_capability_gates` + `unmet_required_capabilities`
  (additive serde, backward-compat). Helper
  `fold_capability_aggregation`. `wire_plugin_registry` signature
  gana 4th + 5th param `core_env_vars: &[(&str, &str)]` +
  `available_capabilities: &BTreeSet<String>`. main.rs bridge
  helpers `core_capability_env_vars()` (via `evaluate_all` —
  INVENTORY const stays private) + `build_available_capabilities(&cfg)`.
  NO mutar INVENTORY. NO touch setup crate API. NO new validator
  at manifest crate level. 4 unit tests + 1 integration regression
  fix. **Out of scope (deferred 81.11.b)**: doctor_render TTY
  sections (PLUGIN CAPABILITY GATES + PLUGIN REQUIRED
  CAPABILITIES) + DoctorCapabilities envelope JSON mode + Phase
  18 reload re-aggregation hook slice. Library + aggregator wire
  shipped today; render layer follows when working tree quiets.
- **81.12 split into foundation + per-plugin sub-slices** —
  - **81.12.0 ✅ shipped 2026-05-03** —
    `PluginFactoryRegistry` foundation: new `factory.rs` module
    con `BoxError` + `PluginFactory` (`Box<dyn Fn(&PluginManifest) ->
    Result<Arc<dyn NexoPlugin>, BoxError>>`) + `PluginFactoryRegistry`
    con register/instantiate/is_registered/kinds/len/is_empty +
    `FactoryRegistrationError::AlreadyRegistered` +
    `FactoryInstantiateError::{NotRegistered, FactoryFailed}`
    thiserror enums. Sibling fn `run_plugin_init_loop_with_factory`
    en init_loop.rs (existing `run_plugin_init_loop` coexists).
    `wire_plugin_registry` gana 6th param `factory_registry:
    Option<&PluginFactoryRegistry>` — None preserves existing
    NoHandle behavior; Some routes via the factory-driven helper.
    main.rs both callsites pass `None` por ahora (legacy block
    untouched). 5 unit + 1 init_loop helper + 2 integration tests.
  - **81.12.a ✅ shipped 2026-05-03** — Browser plugin dual-trait
    migration. New manifest `crates/plugins/browser/nexo-plugin.toml`
    (dormant — operator must NOT add to discovery search_paths
    until 81.12.e). `BrowserPlugin` gana `cached_manifest:
    PluginManifest` field parsed via `include_str!` once en `new()`.
    `impl NexoPlugin for BrowserPlugin` delegating wrapper:
    `manifest()` returns cached field, `init()` calls
    `Plugin::start` mapped a `PluginInitError::Other`, `shutdown()`
    calls `Plugin::stop` mapped a `PluginShutdownError::Other`.
    `pub fn browser_plugin_factory(config) -> PluginFactory`
    expuesto en lib.rs — operador (o 81.12.e) lo registra en
    `PluginFactoryRegistry`. main.rs UNTOUCHED — legacy registration
    block sigue construyendo `BrowserPlugin` directamente. Both
    traits coexist en el mismo struct. 4 unit tests covering
    manifest parse + dual-trait identity + factory builder.
    Compatibilidad audit pre-cycle: PluginInitError::Other y
    PluginShutdownError::Other ya existen, BrowserConfig: Clone
    derivado, BrowserPlugin solo construido via ::new(),
    nexo-plugin-manifest sin cycle. Behavior idéntico a
    pre-81.12.a hasta que 81.12.e flippee main.rs.
  - **81.12.b ✅ shipped 2026-05-01** — Telegram plugin migration:
    dual-trait `TelegramPlugin` + dormant `crates/plugins/telegram/nexo-plugin.toml`
    + `pub fn telegram_plugin_factory(cfg) -> PluginFactory` in lib.rs.
    Manifest parsed once via `include_str!` in `new()`. 5 unit tests
    (4 same as browser + `multi_instance_factory_yields_distinct_registry_names_same_manifest_id`
    proving multi-bot pattern: per-instance label lives in `registry_name`,
    `manifest().plugin.id == "telegram"` for every instance — operator
    differentiates via factory closure capture, not via the manifest).
    Multi-instance handled by operator (one factory call per
    `TelegramPluginConfig` — same shape as legacy main.rs:1902-1910 loop).
    Compatibility audit pre-cycle: TelegramPluginConfig: Clone derived,
    PluginInitError::Other already exists. Behavior identical to pre-81.12.b
    until 81.12.e flips main.rs.
  - **81.12.c ✅ shipped 2026-05-01** — WhatsApp plugin migration:
    dual-trait `WhatsappPlugin` + dormant `crates/plugins/whatsapp/nexo-plugin.toml`
    + `pub fn whatsapp_plugin_factory(cfg) -> PluginFactory` in lib.rs.
    Manifest parsed once via `include_str!` in `new()`. 5 unit tests
    (4 same as telegram + `multi_instance_factory_yields_distinct_registry_names_same_manifest_id`
    proving multi-account pattern: per-instance label lives in
    `registry_name`, `manifest().plugin.id == "whatsapp"` for every
    instance; distinct `session_dir` per instance keeps Signal keys
    isolated). Multi-account handled by operator (one factory call per
    `WhatsappPluginConfig` — same shape as legacy main.rs:1880-1897 loop).
    `enabled = false` short-circuits inside `Plugin::start` returning
    `Ok(())`, so init-disabled plugins still report success through the
    NexoPlugin path — same observable behavior as legacy register +
    start_all combination. Compatibility audit pre-cycle:
    WhatsappPluginConfig: Clone derived, PluginInitError::Other already
    exists, WhatsappPairingAdapter (registered separately) untouched,
    `pairing_state()` accessor for HTTP server polling untouched,
    `register_whatsapp_tools` per-agent untouched (defer Phase 81.3).
    Behavior identical to pre-81.12.c until 81.12.e flips main.rs.
  - **81.12.d ✅ shipped 2026-05-01** — Email plugin migration:
    dual-trait `EmailPlugin` + dormant
    `crates/plugins/email/nexo-plugin.toml` + `pub fn email_plugin_factory(cfg, creds, google, data_dir) -> PluginFactory`
    in lib.rs. Manifest parsed once via `include_str!` in `new()`.
    4 unit tests in `nexo_plugin_tests`: manifest parses + id correct;
    cached_manifest reachable via `&dyn NexoPlugin`; 4-arg factory
    builder produces usable handle; dual-trait identity agrees
    (`name()` == `manifest().plugin.id` == `"email"`).
    Single-plugin / multi-account-internal model — unlike telegram /
    whatsapp where N plugin instances each carry one account's
    `registry_name`, email is ONE plugin with `EmailPluginConfig.accounts:
    Vec<>` driving internal fan-out via `InboundManager` +
    `OutboundDispatcher`. No per-instance label divergence.
    **Credential injection avoided extending `PluginInitContext`** —
    factory closure captures `creds: Arc<EmailCredentialStore>`,
    `google: Arc<GoogleCredentialStore>`, and `data_dir: PathBuf` at
    registration time (analog to browser closing over `BrowserConfig`).
    Same pattern lets future plugins with non-config dependencies stay
    factory-side without touching the trait. `enabled = false` or empty
    `accounts` short-circuits inside `Plugin::start` returning `Ok(())`,
    so init-disabled plugins still report success through the NexoPlugin
    path. Compatibility audit pre-cycle: EmailPluginConfig: Clone derived,
    PluginInitError::Other already exists, EmailPlugin construction site
    at main.rs:1914-1937 untouched, hot-reload `apply_account_diff` API
    untouched, GC ticker untouched, `register_email_tools` per-agent
    untouched (defer Phase 81.3). Behavior identical to pre-81.12.d
    until 81.12.e flips main.rs.
  - **81.12.e ⏸ DEFER → SUPERSEDED-BY-81.17** — original scope (remove
    main.rs:1855-1941 ~87 LOC legacy block) collides with three
    realities discovered after 81.12.a-d shipped:
    (1) the legacy block builds concrete `Arc<BrowserPlugin>` /
    `Arc<EmailPlugin>` / per-instance `WhatsappPlugin::pairing_state`
    references that downstream code in main.rs:1976+ (email tool ctx)
    and the HTTP server (`/whatsapp/<instance>/pair*`) and per-agent
    tool registration depends on directly — not via the `NexoPlugin`
    trait. Removing construction breaks downstream.
    (2) Activating `factory_registry` (passing `Some(&factory_registry)`
    to `wire_plugin_registry`) without removing the legacy
    `plugins.register*()` + `start_all()` calls causes `Plugin::start`
    to fire twice (once via legacy `start_all`, once via
    `NexoPlugin::init` delegation) → double-init breakage.
    (3) For `factory_registry` to actually fire, the discovery walker
    must find `nexo-plugin.toml` manifests; today they're dormant
    inside `crates/plugins/<id>/`. Solving this requires bundled-
    manifest discovery search_paths OR synthetic factory_registry
    injection (~1-2 d of design + tests) — work that 81.17 (extract
    `plugin-browser` to standalone repo via subprocess infra) deletes
    entirely. Out-of-tree plugins don't need `Arc<BrowserPlugin>` from
    main.rs at all; downstream code accesses the plugin via daemon-
    mediated RPC. So 81.12.e is throwaway by design once 81.17 ships.
    Phase 81 dual-trait migration counts a/b/c/d ✅; e is absorbed by
    the subprocess work in 81.14 → 81.17 → 81.18 → 81.19.
- **81.13 → folded into Phase 31.6** (`nexo plugin new --lang
  <rust|python|ts>`). Replaces the deferred template + CLI scope
  once subprocess infra (81.14-81.23) closes.

- **81.14 ✅ shipped 2026-05-01** — `SubprocessNexoPlugin`
  host-side adapter (spawn + handshake plumbing). New manifest
  `[plugin.entrypoint]` section (additive, every field defaults
  so 81.12.a-d in-tree manifests parse unchanged). Adapter spawns
  child via `tokio::process::Command`, writes one
  `initialize { nexo_version }` request, awaits reply on stdout
  with id-tagged response demuxing. Manifest-id mismatch in
  reply = hard fail. Defense-in-depth: `entrypoint.env` cannot
  redefine reserved `NEXO_*` keys. Background tasks: stdin
  writer (mpsc consumer, bounded depth 64, drop-on-full),
  stdout reader (parses + demuxes responses vs notifications).
  Configurable timeout via `NEXO_PLUGIN_INIT_TIMEOUT_MS` env
  (default 5000). `shutdown()` sends JSON-RPC shutdown, 5s reply
  wait, 1s grace before SIGKILL, joins all spawned tasks.
  Idempotent across multiple calls. 9 unit tests cover happy
  path + error paths (missing command / env collision /
  initialize timeout / id mismatch / shutdown idempotency).
  Pattern reuses `extensions/openai-whisper` JSON-RPC envelope.
  **Out of scope this slice** — broker → child topic bridge
  (81.14.b), child-side SDK (81.15), contract spec (81.16),
  out-of-tree plugin extraction (81.17), daemon-mediated RPC
  for memory/llm/tools (81.20), supervisor (81.21), sandbox
  (81.22), tracing bridge (81.23).

- **81.14.b ✅ shipped 2026-05-01** — Broker ↔ child topic bridge.
  `spawn_and_handshake` signature gains `Option<AnyBroker>` —
  `init()` passes `Some(ctx.broker.clone())`, unit tests of the
  shape-only paths pass `None`. Subscribe patterns derived from
  `manifest.channels.register[].kind`: both exact
  `plugin.outbound.<kind>` and wildcard `plugin.outbound.<kind>.>`
  per kind (wildcard demands ≥1 trailing segment in the broker's
  matcher). Per-pattern forwarder task pulls events and `try_send`s
  `broker.event { topic, event }` notifications down the bounded
  stdin mpsc — drop-on-full + warn so a stalled child can't
  backpressure the daemon. Reader task extended to handle
  `broker.publish { topic, event }` notifications: allowlist
  validation via `nexo_broker::topic::topic_matches` against
  `plugin.inbound.<kind>[.>]` for each declared kind, deserialize
  Event via serde, forward to broker. `BridgeContext` activation
  gated by `tokio::sync::OnceCell` — `set()` AFTER handshake
  validates manifest id so the reader's broker.publish path is
  dormant during boot. Child publishing to `agent.route.*` gets
  dropped with warn — defense-in-depth core for community-tier
  plugins. 4 new unit tests cover subscribe pattern derivation,
  child publish forwarding, allowlist rejection, broker=None skip.

- **81.15.a ✅ shipped 2026-05-01** — `nexo-microapp-sdk`
  `plugin` feature + `PluginAdapter` child-side helper. New module
  `crates/microapp-sdk/src/plugin.rs` (~430 LOC) gated behind a
  new optional `plugin` Cargo feature with deps on
  `nexo-plugin-manifest` + `nexo-broker` + `toml`. Builder API:
  `PluginAdapter::new(manifest_toml)` parses the bundled manifest
  at construction; `.on_broker_event(handler)` registers a closure
  that receives `(topic, event, BrokerSender)`; `.on_shutdown(handler)`
  registers an async cleanup hook; `.run_stdio()` drives the
  dispatch loop. Child-side `BrokerSender` is clone-cheap and
  exposes `publish(topic, event)` that emits a `broker.publish`
  notification (no `id`) on stdout. Dispatch loop handles
  `initialize` (replies with cached manifest), `broker.event`
  notifications, `shutdown` (invokes hook, replies, breaks loop),
  unknown methods (`-32601`), parse errors (`-32700`). 6 unit
  tests using `tokio::io::duplex` for end-to-end simulation.
  Mirrors the structural pattern of `runtime.rs::dispatch_loop`
  but uses a different lifecycle envelope (manifest reply +
  broker notifications) so consciously authored as a parallel
  module rather than extending the existing one — different
  trajectories shouldn't couple.

- **81.17 ✅ shipped 2026-05-01** — Auto-subprocess init-loop
  fallback (library + tests). `run_plugin_init_loop_with_factory`
  extended with inline fallback: manifests with
  `entrypoint.command` AND no in-tree factory registered get
  built via `subprocess_plugin_factory(manifest)` and run through
  `init()` like any registered factory. In-tree manifests without
  entrypoint keep `NoHandle`. 3 unit tests cover positive factory
  build + negative skip path. **Boot wire deferred to 81.17.b**
  because the existing `boot.rs` `unreachable!()` ctx_factory
  panics when subprocess plugins try to use real broker /
  shutdown handles — 81.17.b extends `wire_plugin_registry` to
  accept caller-supplied `subprocess_runtime: SubprocessRuntime`.

- **81.17.b ✅ shipped 2026-05-01** — Boot-wire activation
  shipped end-to-end with three coupled changes:
  (1) Made `wire_plugin_registry` async (the prior sync shape
  used `futures::executor::block_on` which deadlocked tokio when
  subprocess plugins tried to spawn children); 5 call sites
  updated (main.rs ×2 incl. `run_doctor_plugins`, 3 tests).
  (2) New `FactoryInitResult { outcomes, handles }` return type
  + new `WirePluginRegistryOutput.plugin_handles` field — without
  retention `kill_on_drop(true)` SIGKILLed children right after
  init returned; main.rs's `wire` binding now keeps subprocess
  Arcs alive for the daemon's lifetime.
  (3) New `SubprocessRuntime { broker, shutdown, config_dir, state_root }`
  + `wire_plugin_registry_with_runtime(...)` variant +
  `SubprocessCtxStubs` builds a real-enough `PluginInitContext`
  using runtime's broker + shutdown + stub `::new()` registries
  for fields SubprocessNexoPlugin doesn't read. Single `'env`
  lifetime on `run_plugin_init_loop_with_factory` replaces HRTB
  (HRTB demanded `'static` so the closure couldn't borrow from
  `&stubs` + `&runtime`).
  main.rs activates: empty in-tree factory + populated
  SubprocessRuntime → auto-subprocess fallback fires for any
  discovered manifest with `[plugin.entrypoint] command`.
  In-tree plugins keep dormant manifests OUT of `search_paths`
  and continue via legacy block.
  Integration test `crates/core/tests/subprocess_plugin_e2e.rs`
  drops manifest + bash mock in tempdir, asserts InitOutcome::Ok
  + broker.publish round-trip within 2s. 2/2 e2e tests + 5/5
  init_loop unit tests pass.

- **81.17.c ⬜ RENUMBERED (was 81.17)** — Pilot extract
  `plugin-browser` to standalone repo. Out-of-tree:
  `github.com/nexo-rs/plugin-browser` ships binary; daemon loads
  via discovery + auto-subprocess fallback (81.17 + 81.17.b).
  ~3 d. Required to validate the contract end-to-end with a real
  plugin before 81.18-81.19 extract telegram/whatsapp/email.

- **81.16 ✅ shipped 2026-05-01** — `nexo-plugin-contract.md`
  versioned IPC spec at workspace root (~600 LOC, contract
  version 1.0.0). Sections: transport, manifest entrypoint,
  JSON-RPC envelope, lifecycle methods, broker bridge
  notifications, topic allowlist, error codes, backpressure,
  code examples (Rust shipped + Python/TS skeletons for
  Phase 31.4/31.5), semver compat policy, reference impls,
  out-of-scope list. Thin pointer at `docs/src/plugins/contract.md`
  + SUMMARY.md entry; mdbook builds clean. Documents what
  81.14/14.b/15.a already implements — single source of truth
  for cross-language SDK authoring + internal wire-change
  reviews.

- **81.20.a ✅ shipped 2026-05-01** — Daemon-mediated
  `memory.recall` RPC bridge. First of 3 planned RPC handlers.
  Reader detects `id + method` frame as incoming child request,
  routes to `handle_child_request`. `memory.recall` validates
  params (`agent_id` + `query` strings, `limit` u64 capped 1000),
  calls `LongTermMemory::recall`, returns `{ entries }` shape.
  Errors -32601 / -32602 / -32603. `BridgeContext.memory` and
  `SubprocessRuntime.long_term_memory` added. Wire docs in
  contract v1.1.0. 19/19 subprocess + 2/2 e2e tests pass.

- **81.20.a.b ✅ shipped 2026-05-01** — 1-LOC fix:
  `long_term_memory: memory.clone()` instead of `None`. The
  daemon path's `let memory =` binding (main.rs:1731-1821) is
  already in scope at the wire callsite — no reorder needed.
  Earlier note about line 10883 was inside `run_mcp_server`, a
  separate function. Subprocess memory.recall now reaches the
  real backend.

- **81.20.b ✅ shipped 2026-05-01** — Daemon-mediated
  `llm.complete` RPC (non-streaming MVP). Host-side handler +
  3 unit tests + wire spec at contract v1.2.0. New
  `LlmServices { registry, config }` bundle in BridgeContext +
  SubprocessRuntime. Handler validates params (-32602), checks
  llm services wired (-32603), builds client via LlmRegistry
  (-32603 if provider unknown), calls `chat()`, returns
  `{ content, finish_reason, usage }`. Tool-call responses
  surface as -32601 (deferred). main.rs llm_registry construction
  reordered to wrap in Arc immediately so it's clonable into
  SubprocessRuntime. Runtime threading + streaming deferred to
  81.20.b.b. 22/22 subprocess + 2/2 e2e tests pass.

- **81.20.b.b ✅ shipped 2026-05-01** — Runtime threading half.
  PluginInitContext extended with `llm_config: Arc<LlmConfig>`.
  SubprocessNexoPlugin::init builds LlmServices from
  `ctx.llm_registry + ctx.llm_config`. SubprocessRuntime fields
  flattened (`llm_registry` + `llm_config`). main.rs threads
  real handles. Streaming carved out as 81.20.b.c (~1 d).
  22/22 subprocess + 2/2 e2e tests pass.

- **81.20.b.c ✅ shipped 2026-05-01** — `llm.complete` streaming
  via `llm.complete.delta` notifications. Opt-in via
  `params.stream = true`. Host calls `client.stream`, emits
  `TextDelta` chunks as notifications via stdin_tx with
  request_id correlation. Final reply omits content. Tool-call
  deltas dropped (same scope as non-streaming MVP).
  `handle_child_request` signature gains stdin_tx + request_id
  parameters. 22/22 subprocess tests pass. Wire docs at contract
  v1.3.0. SDK-side streaming consumption helpers deferred to
  81.15.c.

- **81.15.c ✅ shipped 2026-05-01** — SDK child-side RPC
  helpers. BrokerSender extends with pending DashMap +
  next_id AtomicU64 + new `request(method, params, timeout) ->
  Result<Value, RpcError>` low-level helper. Typed wrappers
  `recall_memory(agent_id, query, limit)` returns
  `Vec<MemoryEntry>` and `complete_llm(LlmCompleteParams)`
  returns `LlmCompleteResult`. New `RpcError` enum (Server /
  Timeout / Transport / Decode). Dispatch loop extended to
  demux response frames. Critical fix: handler dispatch wrapped
  in `tokio::spawn` to prevent self-deadlock when handler calls
  `broker.request(...)`. SDK feature `plugin` adds nexo-llm +
  nexo-memory + dashmap deps (gated). 10/10 SDK plugin tests
  pass.

- **31.0 ✅ shipped 2026-05-01** — ext-registry index format
  types crate. New `crates/ext-registry/` workspace member with
  `ExtRegistryIndex` + `ExtEntry` + `ExtDownload` + `ExtSigning`
  + `ExtTier` + `IndexValidationError`. Validation enforces id
  regex, HTTPS-only URLs, sha256 hex format, non-empty
  downloads, verified-tier requires signing, `deny_unknown_fields`.
  Bundled sample at `examples/sample-ext-index.json`. Schema
  version 1.0.0. 9 unit tests pass. Index repo bootstrap is a
  separate operator-side init outside this workspace.

- **Phase 31 architecture pivot 2026-05-03** — marketplace pivoted
  from centralized catalog (Option A) to decentralized GitHub
  Releases (Option B) per user direction ("no voy a alojar esto").
  Plugin authors publish to their own GitHub repo as Releases
  with a fixed asset naming convention; install CLI hits GitHub
  Releases API directly. Zero infrastructure for nexo-rs
  maintainers, no gatekeeping for plugin authors, operator
  controls trust via per-author cosign keys (Phase 31.3).
  `crates/ext-registry/` types still useful for index-style
  bookkeeping but not on the install hot path.

- **31.1 ✅ shipped 2026-05-03** — ext-installer crate (Option B).
  New `crates/ext-installer/` workspace member: `PluginCoords`
  parser (`owner/repo@tag`, defaults `latest`), `resolve_release`
  hits GitHub Releases API + parses `nexo-plugin.toml` asset to
  learn `plugin.id` + locates `<id>-<version>-<target>.tar.gz` +
  matching `.sha256` asset, `download_and_verify` streams the
  tarball with incremental sha256 + verifies vs the `.sha256`
  body (cleans up on mismatch), `current_target_triple` detects
  rust target with `NEXO_INSTALL_TARGET` env override.
  `InstallError` enum covers coords, http, release shape, target
  not found, sha256 invalid/mismatch. Deps: reqwest streaming +
  sha2 + hex + toml + nexo-plugin-manifest + nexo-ext-registry.
  Dev-deps: wiremock + tempfile. 8/8 unit tests pass via
  wiremock simulating GitHub Releases API (coords parsing, URL
  branching latest/tagged, missing-manifest rejection,
  missing-target-tarball rejection, happy-path round-trip,
  sha256-mismatch cleanup). README documents Option B + asset
  naming convention. Workspace builds clean.

- **31.1.b ✅ shipped 2026-05-03** — Tarball extraction. New
  `extract.rs` + `extract_error.rs` in `nexo-ext-installer`.
  `extract_verified_tarball(ExtractInput) -> ExtractedPlugin`
  pipeline: idempotent re-install check (existing `<id>-<version>/`
  with matching manifest short-circuits), stale `.staging-*`
  cleanup, unique staging dir, sync extract under
  `tokio::task::spawn_blocking` with per-entry path validation
  (rejects `..`, absolute, Windows-prefix, NUL bytes) + entry
  type whitelist (`Regular | Directory` only — symlinks /
  hardlinks / char / block / fifo / GNU extensions all rejected
  with `DisallowedEntryType`) + 4-axis size budgets
  (`ExtractLimits`: 100 MB tarball, 10K entries, 250 MB
  extracted, 100 MB per entry), manifest re-parsed + validated
  against expected id/version (catches tampered tarballs even
  past sha verification), `bin/<id>` existence + chmod 0o755 on
  Unix, atomic-rename staging → final. New `ExtractError` enum
  (11 variants). 13 new tests (8 public-API including raw-header
  path-traversal and absolute-path injection that bypass
  `tar::Builder::set_path` upstream normalization, 5
  helper-level for `validate_entry_path` + `cleanup_stale_staging`).
  21/21 installer tests pass. Workspace builds clean. Crate
  intentionally does NOT read config — caller resolves
  `dest_root` from `plugins.discovery.search_paths[0]`.

- **31.1.c ✅ shipped 2026-05-03** — `Mode::PluginInstall` CLI
  integration. New `src/plugin_install.rs` module + `Mode::PluginInstall`
  / `Mode::PluginHelp` variants. Argv shape:
  `nexo plugin install <owner>/<repo>[@<tag>] [--dest <path>]
  [--target <triple>] [--json]`. Pipeline: resolve target
  (`--target` flag → `NEXO_INSTALL_TARGET` env → autodetect) →
  resolve dest_root (`--dest` → `cfg.plugins.discovery.search_paths[0]`
  → `nexo_state_dir().join("plugins")` fallback with stderr
  warn) → reqwest client with optional `NEXO_GITHUB_TOKEN`
  Bearer + GitHub UA → coords parse → resolve_release →
  download_and_verify → extract_verified_tarball → cleanup
  cached tarball → best-effort
  `plugin.lifecycle.<id>.installed` broker emit (NATS only, 2s
  connect timeout, non-fatal). Output: 6-line human progress
  with sha trunc + idempotent-skip line; or single-line JSON
  `PluginInstallReport`. Error path: `PluginInstallErrorReport`
  with stable `kind` enum mapping all 7 InstallError + 11
  ExtractError variants. Hint blocks for `TargetNotFound` /
  GH rate-limit / 404. 8 new unit tests. 21/21 ext-installer
  regression green. Workspace builds clean. Cached tarballs
  live at `<state_dir>/plugin-install-cache/` (deleted on
  success).

- **31.2 ✅ shipped 2026-05-03** — Per-plugin CI publish
  workflow template. New
  `extensions/template-plugin-rust/.github/workflows/release.yml`
  (~210 LOC) + `scripts/extract-plugin-meta.sh` +
  `scripts/pack-tarball.sh`. Tag-driven (`v*`) workflow with
  four jobs: validate-tag (regex + tag-vs-manifest version
  match), build matrix (linux musl x86_64/aarch64 via
  `cargo-zigbuild` 0.22.3 + macOS x86_64/aarch64 via direct
  cargo), optional sign job gated on repo variable
  `COSIGN_ENABLED == "true"` (keyless cosign sign-blob
  producing .sig/.pem/.bundle per asset), release job with
  idempotent `gh release create` + `gh release upload
  --clobber`. Asset convention
  `<id>-<version>-<target>.tar.gz` matches what 31.1 resolver
  expects + 31.1.b extractor consumes (`bin/<id>` +
  `nexo-plugin.toml` at root, no wrapping dir). Concurrency
  group keyed on tag prevents duplicate publish. Template
  binary renamed `template_plugin_rust` (underscores) to
  match `plugin.id` — convention: cargo `[[bin]] name ==
  [plugin] id`. New Rust integration test
  `tests/pack_tarball.rs` builds a synthetic binary, runs
  `pack-tarball.sh`, re-extracts, asserts canonical layout
  + sha256 + binary 0o755. README publishing section + new
  docs page `docs/src/plugins/publishing.md`. mdbook builds
  clean. Out of scope: SLSA L3 attestation (defer 31.2.b);
  Windows target; multi-plugin monorepo; `crates.io`
  auto-publish.

- **31.3 ✅ shipped 2026-05-03** — Cosign signature verification
  + `<config_dir>/extensions/trusted_keys.toml` operator trust
  policy. New `crates/ext-installer/src/{trusted_keys.rs,
  verify.rs, verify_error.rs}` modules + sample at
  `config/extensions/trusted_keys.toml.example`. Three trust
  modes: `ignore` / `warn` (default) / `require`; per-author
  `[[authors]]` entries override the global default. CLI
  flags `--require-signature` / `--skip-signature-verify`
  (mutually exclusive — `FlagsConflict` parse-time error).
  Verify pipeline shells out to `cosign verify-blob` via
  `tokio::process::Command` with `--certificate-identity-regexp`
  + `--certificate-oidc-issuer` (+ optional `--bundle` for
  offline Rekor proof). `discover_cosign_binary` walks
  override → $PATH → /usr/local/bin / /opt/homebrew/bin /
  /usr/bin / ~/go/bin fallbacks. New `VerifyError` enum (7
  variants: CosignNotFound, CosignFailed, Io, PolicyRequiresSig,
  AssetIncomplete, TrustedKeysParse, IdentityRegexpInvalid).
  `PluginInstallReport` extended with `signature_verified` +
  `signature_identity` + `signature_issuer` + `trust_mode` +
  `trust_policy_matched`. Verify hook lands between
  `download_and_verify` (sha256) and `extract_verified_tarball`.
  Cleans cached signing material post-success along with the
  cached tarball. Hint blocks for `CosignNotFound`,
  `PolicyRequiresSig`, `CosignFailed`. New docs page
  `docs/src/ops/plugin-trust.md` covers trust modes +
  identity_regexp shape + cosign install + JSON schema +
  troubleshooting. Template README addendum shows authors what
  operators need in `[[authors]]`. 14 new tests (9 in
  `trusted_keys.rs::tests` + 6 in `verify.rs::tests` using
  mock cosign shell-script + 4 in `plugin_install::tests`):
  ext-installer 21→38, plugin_install 8→12. Workspace builds
  clean; mdbook clean. NO env knob v1, NO sigstore-rs (defer),
  NO per-plugin override beyond per-owner, NO TUF/GPG/threshold
  sigs.

- **31.4 ✅ shipped 2026-05-03** — Python plugin SDK +
  template + `noarch` resolver fallback. New
  `extensions/sdk-python/nexo_plugin_sdk/` package
  (`PluginAdapter` async dispatch loop, `BrokerSender.publish`,
  `Event` dataclass, `read_manifest` TOML reader, 3 exception
  types). Stdlib only (`tomllib` 3.11+ with `tomli` fallback
  shim). New `extensions/template-plugin-python/` plugin
  template: manifest with `entrypoint.command = "./bin/<id>"`
  pointing at a bash launcher, `src/main.py` echo handler,
  `scripts/{extract-plugin-meta.sh, pack-tarball-python.sh,
  verify-pure-python.sh}`, `tests/test_pack_tarball.py`
  end-to-end synthetic-SDK pack assertion,
  `.github/workflows/release.yml` (Phase 31.2-shaped 4-job with
  single `noarch` matrix entry + setup-python@v5 + pure-python
  audit gate). Resolver in `crates/ext-installer/src/lib.rs`
  falls back to `<id>-<version>-noarch.tar.gz` when no
  per-target tarball matches; per-target preferred when both
  present. 2 new resolver tests; ext-installer 38→40. SDK has
  6 tests via stdlib `unittest` (handshake, dispatch incl.
  non-blocking reader proof, shutdown with in-flight drain,
  unknown method, manifest validation). Daemon spawn pipeline
  UNCHANGED — language-agnostic by design. New docs page
  `docs/src/plugins/python-sdk.md`; SUMMARY wired. Cross-link
  in Rust template README. SDK fix during dev:
  `PluginAdapter` tracks in-flight handler tasks via
  `_inflight: set[Task]` and awaits them in `_drain_inflight`
  before replying to `shutdown` so handlers do not get
  cancelled mid-publish. `pytest` not used — stdlib `unittest`
  zero install friction. NO `[runtime]` block in manifest
  schema, NO daemon-side changes, NO embedded interpreter, NO
  PyPI publish (defer until 31.5 lands), NO native-ext
  per-target Python tarballs (defer 31.4.b).

- **31.5 ✅ shipped 2026-05-04** — TypeScript plugin SDK +
  template (robusto). New `extensions/sdk-typescript/` ESM
  package with strict tsconfig (Node16 module resolution,
  noUncheckedIndexedAccess, isolatedModules). Public API:
  `PluginAdapter`, `BrokerSender`, `Event`, `parseManifest`,
  `installStdoutGuard`, `STDOUT_GUARD_MARKER`, 3 exception
  classes (`PluginError`/`ManifestError`/`WireError`),
  JSON-RPC frame helpers (`buildResponse` /
  `buildErrorResponse` / `MAX_FRAME_BYTES`). Single runtime
  dep `smol-toml@^1.4.1` (~5 KB pure-JS TOML parser).
  Robustness defaults all default-on:
  `enableStdoutGuard:true` (patches `process.stdout.write` to
  divert non-JSON lines to stderr tagged `[stdout-guard]` —
  catches the most common plugin-author mistake of
  `console.log` corrupting the JSON-RPC stream),
  `maxFrameBytes:1<<20` (rejects oversized inbound frames
  with `WireError`), `handleProcessSignals:true`
  (SIGTERM/SIGINT trigger graceful shutdown), in-flight task
  drain on shutdown via `Promise.allSettled([...inflight])`.
  Single-shot `run()` throws `PluginError` on second call.
  13 stdlib `node:test` tests across handshake (3), manifest
  validation (3), dispatch (3), stdout-guard (2), wire (1),
  lifecycle (1). Spawn-driven fixtures
  (`tests/fixtures/{echo,slow,console-log,lifecycle}-plugin.mjs`).
  New `extensions/template-plugin-typescript/` template:
  manifest, `src/main.ts` echo handler, `tsconfig.json`,
  `package.json` (SDK as `file:../sdk-typescript`),
  `scripts/{extract-plugin-meta.sh, pack-tarball-typescript.sh,
  verify-pure-js.sh}`, end-to-end pack test, 4-job CI workflow
  (`actions/setup-node@v4` + `npm ci` + typecheck + tsc +
  `npm prune --omit=dev` + pack + pure-JS audit + optional
  sign + release). Pack script vendors compiled `dist/main.js`
  to `lib/plugin/` and SDK + scoped/unscoped npm deps to
  `lib/node_modules/`; ships bash launcher with
  `NODE_PATH=lib/node_modules` exec'ing `node lib/plugin/main.js`.
  New docs page `docs/src/plugins/typescript-sdk.md`; SUMMARY
  wired. Cross-links added in Rust + Python template READMEs.
  Resolver `noarch` fallback (Phase 31.4) reused unchanged;
  daemon spawn pipeline UNCHANGED. Lifecycle test uses a
  child-process fixture so the readline loop doesn't block
  the test runner's stdin (would have deadlocked with an
  in-process double-`run()`). NO CJS fallback, NO embedded
  TS at runtime, NO Deno entry, NO bundling, NO npm publish
  (defer until 31.5.c lands), NO native addons in noarch
  (defer 31.5.b). PHP SDK explicitly deferred to **31.5.c**
  per user direction.

- **31.5.b ⬜** Per-target TypeScript tarballs for plugins
  with native node addons (`*.node` files from packages like
  `bcrypt`, `sharp`, `better-sqlite3`). Convention:
  `<id>-<version>-node20-<triple>.tar.gz`. Pack script branch
  on `verify-pure-js.sh` failure: switch to per-target build
  matrix instead of failing. Resolver tweak to try
  `<runtime>-<version>-<triple>` before falling back to
  `noarch`. ~2 d.

- **31.5.c ✅ shipped 2026-05-04** — PHP plugin SDK + template
  (Fibers, robusto). New `extensions/sdk-php/` Composer
  package with PSR-4 namespace `Nexo\Plugin\Sdk\` + SPDX
  `MIT OR Apache-2.0` license + `version: "0.1.0"` for path-
  repo resolution. Public API: `PluginAdapter`, `BrokerSender`,
  `Event`, `Manifest`, `Wire`, `Scheduler`, `StdoutGuard`,
  `PluginError`/`ManifestError`/`WireError` (one class per
  file per PSR-4). Runtime dep `yosymfony/toml: ^1.0` (pure
  PHP). PHP `^8.1` minimum — Fibers required for cooperative
  scheduler that preserves "reader does not block on slow
  handler" invariant proven necessary by TS + Python tests.
  Robustness defaults all default-on: `enableStdoutGuard:true`
  (`ob_start` diverts non-JSON `echo`/`print`/`printf`/`var_dump`
  to stderr tagged `[stdout-guard]`; documented limitation:
  `fwrite(STDOUT, ...)` direct writes BYPASS — SDK's
  BrokerSender uses this deliberately for blessed JSON frames),
  `maxFrameBytes:1048576`, `handleProcessSignals:true`
  (`pcntl_async_signals` for SIGTERM/SIGINT), in-flight Fiber
  drain on shutdown via `Scheduler::drain()`. Single-shot
  `run()` throws PluginError on second call. 14 tests across 7
  test files using stdlib `proc_open` runner (no PHPUnit dep):
  handshake (3), manifest validation (3), dispatch (3 incl.
  slow-handler proof + drain), stdout-guard (2), wire (1),
  lifecycle (1), event (1). New `extensions/template-plugin-php/`
  template: manifest, `src/main.php` echo handler,
  `composer.json` declaring SDK via path repository
  (`url: ../sdk-php, options: {symlink: false}`, `minimum-
  stability: dev`, `prefer-stable: true`), `composer.lock`
  checked in (reproducibility), helper scripts, end-to-end
  pack test, 4-job CI workflow (`shivammathur/setup-php@v2` +
  composer 2 + `composer validate --strict` + `composer install
  --no-dev --optimize-autoloader --classmap-authoritative` +
  pack + pure-PHP audit + optional sign + release). Bash
  launcher uses `php -d display_errors=stderr -d log_errors=0`
  so PHP errors land on stderr (defense-in-depth with the
  stdout guard). Real handshake smoke verified locally: `echo
  '{...,"method":"initialize"}' | php src/main.php` returns
  valid JSON-RPC manifest reply. New docs page
  `docs/src/plugins/php-sdk.md`; SUMMARY wired. Cross-links in
  Rust + Python + TS template READMEs. Resolver `noarch`
  fallback (Phase 31.4) reused unchanged; daemon spawn
  pipeline UNCHANGED. Issue resolved during dev: PSR-4 needs
  one class per file (originally had `Errors.php` with 3
  classes — split into `PluginError.php` + `ManifestError.php`
  + `WireError.php`). Issue resolved: path repo needed
  `minimum-stability: dev` + `prefer-stable: true` since the
  SDK has no stable tagged version; added explicit `version`
  field to SDK composer.json so `^0.1.0` constraint resolves
  cleanly. NO PHPUnit (stdlib runner mirrors TS+Python
  choices); NO ReactPHP/Amp; NO embedded interpreter; NO
  Packagist publish (defer); NO native PHP ext in noarch
  (defer 31.5.c.b); NO PHP 7.4/8.0 (Fibers 8.1+).

- **31.5.c.b ⬜** Per-target PHP tarballs for plugins with
  native PHP extensions (`*.so` / `*.dylib` / `*.dll` from
  Composer deps). Convention: `<id>-<version>-php83-<triple>.tar.gz`.
  Pack script branch on `verify-pure-php.sh` failure: switch
  to per-target build matrix instead of failing. Resolver
  tweak to try `php<MAJOR><MINOR>-<triple>` before falling
  back to `noarch`. ~2 d.

- **31.6 ✅ shipped 2026-05-04** — `nexo plugin new --lang
  <rust|python|typescript|php>` scaffolder. New
  `src/plugin_new.rs` module + `Mode::PluginNew` variant +
  parse arm. Templates embedded at compile time via
  `include_dir!` from the four `extensions/template-plugin-*/`
  directories — binary works after `cargo install` with no
  runtime FS dependency. Argv: `nexo plugin new <id> --lang
  <lang> [--dest <path>] [--owner <gh-handle>] [--description
  <text>] [--git] [--force] [--json]`. Validates id regex
  `^[a-z][a-z0-9_]{0,31}$` + lang ∈ `{rust, python,
  typescript, php}` before any IO. Substitution is literal
  byte-replace, longest-pattern-first (covers
  `template_plugin_<lang>` snake, `template-plugin-<lang>`
  kebab, `template_echo_<suffix>` channel kind, `Template
  Plugin (<Lang>)` title, boilerplate description, original
  author string). Text-extension whitelist prevents binary
  corruption. `--owner alice` injects
  `alice <alice@users.noreply.github.com>` privacy-preserving
  GitHub email. `--git` runs `git init --initial-branch=main`
  + `git add .` + `git commit -m "chore: scaffold ..."`;
  gracefully skips when `git` binary missing. `--force`
  removes existing dest. Unix-only `chmod 0755` on
  `scripts/*.sh`. `next_steps_for(lang, id, owner)` emits
  language-specific commands (`cargo build` for rust, `pip
  install` for python, `npm install && npm run build` for
  typescript, `composer install` for php). 11 unit tests:
  id/lang validation table-tests, title-case, placeholder
  ordering, scaffold-{rust,python,typescript,php} (4 tests
  verifying key files + manifest substitution + Cargo /
  package / composer renames), dest-exists-without-force
  fails, force-flag overwrites, owner-substitution lands. New
  runtime dep `include_dir = "0.7"`; workspace `regex` +
  `thiserror` added to root `[dependencies]`. Help text in
  `print_plugin_help` + `print_usage`. All 4 template READMEs
  replace the manual `cp -r` + `sed -i` quickstart pipeline
  with `nexo plugin new <id> --lang <lang> --owner <handle>
  --git`. Replaces deferred 81.13 (folded into 31.6 per
  PHASES-curated). Workspace builds clean. Rust ext-installer
  regression: 40/40 still pass; plugin_install: 12/12; new
  plugin_new: 11/11. Phase 31 author-side flow closes end-to-end.

- **31.7 ✅ shipped 2026-05-04** — `nexo plugin run <path>`
  local dev loop. New `src/plugin_run.rs` module +
  `Mode::PluginRun` variant + parse arm. Argv:
  `nexo plugin run <path-or-manifest> [--no-daemon-config]
  [--watch] [--verbose] [--json]`. Implementation pattern:
  pre-boot validation + side-channel `args.plugin_run_override`
  + **fall-through** to `Mode::Run` boot path (single source
  of truth — no duplicated daemon startup). `resolve_local_plugin`
  canonicalizes the path, branches on file (must end in
  `nexo-plugin.toml`) vs dir (must contain `nexo-plugin.toml`),
  parses manifest via `PluginManifest::from_str`, validates
  `[plugin.entrypoint] command` is non-empty. Errors:
  `PathNotFound`, `NotAPluginPath`, `ManifestInvalid`,
  `MissingEntrypoint`, `WatchDeferred` (--watch reserved for
  31.7.b), `Io`. `apply_override(&mut cfg, &override)`
  injected right after `AppConfig::load` — prepends
  `plugin_root` to `cfg.plugins.discovery.search_paths`
  (idempotent: skips when already at head); when
  `--no-daemon-config`, clears `cfg.agents.agents` for
  standalone plugin inspection. `local > global` precedence —
  local plugin wins because discovery walker stops at first
  id-match in `search_paths[0]`. Reuses Phase 81.17.b
  subprocess auto-fallback for spawn (no daemon-side changes);
  reuses Phase 81.10 hot-reload (re-walks `search_paths` each
  tick → `cargo build` triggers respawn). 8 unit tests: 6
  path-resolution + 2 search-path-prepend (resolves dir with
  manifest, resolves manifest directly, rejects non-existent,
  rejects dir without manifest, rejects invalid TOML, rejects
  missing entrypoint, prepend inserts at head, prepend
  idempotent for head-match). Tests use a private
  `apply_search_path_prepend` helper that operates on
  `Vec<PathBuf>` directly to avoid `AppConfig::default()`
  (no Default impl). `CliArgs` gained `plugin_run_override:
  Option<PluginRunOverride>` field; `parse_args` updated
  across all 9 construction sites. New help text in
  `print_plugin_help` + main `print_usage`. NO `--watch`
  (deferred 31.7.b), NO stdio-only no-broker mode (deferred
  31.7.c), NO multi-plugin injection, NO custom port.
  Workspace builds clean; ext-installer regression 40/40;
  plugin_install 12/12; plugin_new 11/11; plugin_run 8/8.

- **31.7.b ⬜** `nexo plugin run --watch` — filesystem watcher
  via `notify` crate. On manifest or src/ change, trigger
  Phase 81.10 hot-reload (re-walks search_paths) so the
  daemon respawns the subprocess against the new binary.
  Author runs `cargo watch -x build` in another terminal;
  watcher plugs in here. Defer parsing the existing flag is
  already in place via `WatchDeferred` error; this slice
  flips it to functional. ~0.5 d.

- **31.7.c ⬜** `nexo plugin run --stdio-only` — boot ONLY the
  plugin's broker bridge + JSON-RPC pipe over stdin/stdout,
  no NATS, no agents, no LLM. Pure contract smoke test for
  authors who want to verify wire format independently of
  agent runtime. Plugin's `BrokerSender::publish` writes to
  stdout instead of NATS; daemon-side reader echoes to
  stderr. ~1 d.

- **31.8 ✅ shipped 2026-05-04** — Operator UI: `nexo plugin
  list` / `upgrade` / `remove`. New `src/plugin_admin.rs`
  module + 3 `Mode` variants (`PluginList`, `PluginUpgrade`,
  `PluginRemove`) + parse/dispatch arms in `src/main.rs`.
  Per-plugin `<plugin_dir>/.nexo-install.json` schema (v1.0)
  records `id, version, owner, repo, tag, target, sha256,
  installed_at (RFC3339), source: "github-releases"`.
  `plugin_install.rs::run_plugin_install` patched to call
  `write_install_metadata` after extract success (skipped on
  `was_already_present`); soft-fail with stderr warn when
  write fails. `discover_installed_plugins` walks every
  search_path's immediate child dirs, parses `nexo-plugin.toml`
  + optional `.nexo-install.json`, sorted by id. `list`
  filters orphans by default; `--include-orphan` surfaces
  with `(orphan)` row suffix; JSON shape
  `PluginListReport { ok, plugins[] }`. `upgrade` reads
  metadata, builds `PluginCoords { owner, repo, tag }`, calls
  `resolve_release` honoring `--target` override; emits
  `PluginUpgradeReport { was_no_op: true }` on same-version,
  `DowngradeRefused` on lower-version, otherwise delegates
  to `run_plugin_install` (single download/verify/extract/cosign
  code path; JSON line is the install report). `remove`
  requires `--yes` (interactive prompt deferred to 31.8.b →
  `NeedsYesConfirm` error otherwise); atomic via rename-aside
  `<plugin_dir>.removing-<rand>` + `remove_dir_all`;
  `--purge-cache` walks `nexo_state_dir/plugins/<id>` +
  `nexo_state_dir/plugins/cache/<id>`. Best-effort
  `plugin.lifecycle.<id>.removed` broker emit (NATS only,
  2s timeout, mirrors install pattern). `AdminError` 9
  variants. 11 unit tests in `plugin_admin::tests` (metadata
  round-trip + read-none + read-invalid + discover-skips +
  discover-collects-meta + list-filters-orphans + list-includes-orphans
  + list-empty-when-search-paths-missing + admin-error-kind
  exhaustive + aside-path-differs + list-entry-includes-meta).
  Help text in `print_plugin_help` + `print_usage`.
  Workspace builds clean; `cargo test --bin nexo plugin`
  42/42 (11 admin + 12 install + 8 run + 11 new).
  Out of scope: interactive TTY prompt (defer 31.8.b),
  semver constraint pinning beyond literal tag tracking
  (defer 31.8.c), separate upgrade-side cosign duplication
  (delegated to install).

- **31.8.b ⬜** Interactive TTY confirm prompt for
  `nexo plugin remove <id>`. Currently `--yes` is mandatory.
  Detect TTY via `io::stdin().is_terminal()` (Rust 1.70+).
  When stdin is interactive AND `--yes` not passed, print
  `Remove plugin <id> v<version>? [y/N]` and read one byte
  reply. When `--purge-cache`, mention the cache dirs in the
  prompt. Returns `NeedsYesConfirm` only on non-TTY (keeps
  ansible / CI safety). ~0.3 d.

- **31.8.c ⬜** `nexo plugin upgrade --tag <pin>` — explicit
  tag pinning that updates `.nexo-install.json::tag` so
  subsequent `upgrade` calls track the new pin. Currently
  `upgrade` always uses the recorded tag (which is `latest`
  for floating installs and `vX.Y.Z` for pinned ones).
  Distinct from `--target` which only overrides the
  per-target triple resolution. ~0.3 d.

- **31.9 ✅ shipped 2026-05-04** — Author-side documentation
  closeout. 4 new/expanded mdbook pages + 1 sync script.
  New `docs/src/plugins/authoring.md` (~200 LOC) — entry-point
  overview with "Plugin vs Extension vs Microapp" decision-
  tree table (3 rows × 4 cols) + 4-language picker table
  (Rust/Python/TS/PHP × 4 cols pointing at SDK pages) +
  5-min Rust quickstart (scaffold → build → `nexo plugin
  run .` with expected stderr trace) + local dev loop
  conventions. New `docs/src/plugins/rust-sdk.md` (~250 LOC)
  — `PluginAdapter` builder API reference (constructor +
  `on_broker_event` + `on_shutdown` + `run_stdio`), manifest
  example, quickstart code block, smoke test handshake
  one-liner, per-target tarball convention, CI workflow
  pointer, SDK test command. New `docs/src/plugins/signing-
  and-publishing.md` (~300 LOC) — 5-section end-to-end
  tutorial: unsigned first release → `COSIGN_ENABLED`
  opt-in → operator `[[authors]]` block with
  `identity_regexp` regex anchored on workflow URL → round-
  trip install verification with sample JSON output →
  troubleshooting table. New `scripts/sync-plugin-contract.sh`
  (~60 LOC bash, executable) — vendors workspace-root
  `nexo-plugin-contract.md` into `docs/src/plugins/contract.md`
  with auto-vendored HTML comment header + "See also"
  cross-link footer; `--check` mode exits 1 on drift for
  CI gate use (full CI integration deferred to 31.9.b).
  `docs/src/plugins/contract.md` expanded from 28 LOC stub
  to 678 LOC vendored copy via initial sync run.
  `docs/src/SUMMARY.md` "Plugin SDKs" section reordered to
  9 entries: Authoring overview / Plugin contract / Patterns
  / Rust SDK / Python SDK / TypeScript SDK / PHP SDK /
  Publishing / Signing & publishing. `mdbook build docs`
  clean; existing `scripts/check_mdbook_english.sh` clean
  on new pages. Out of scope: `docs/src/plugin-authoring/`
  subdir reorganization (kept inside existing
  `docs/src/plugins/` to preserve edit-url + bookmarks),
  mdbook-include/mdbook-cmdrun plugins (sync script is
  dependency-free), Spanish localization, frontmatter
  (plain Markdown only), troubleshooting duplication
  (cross-linked to `ops/plugin-trust.md`).

- **31.9.b ⬜** CI gate hooking
  `bash scripts/sync-plugin-contract.sh --check` into the
  existing docs build job so any change to
  `nexo-plugin-contract.md` that does not refresh the
  vendored copy fails the PR pipeline. Currently authors
  must remember to run the sync script manually before
  commit. ~0.2 d.

- **81.3 ✅ shipped 2026-05-04** — Tool namespace runtime
  enforcement. New `crates/core/src/agent/scoped_tool_registry.rs`
  module (~440 LOC + 11 unit tests). `ScopedToolRegistry`
  per-plugin proxy gates every `register*` call against 4
  layers: reserved-prefix denylist (`agent_`, `system_`,
  `nexo_`, `mcp_`, `ext_`) with `ext_<self_id>_*` carve-out
  for the canonical plugin shape, plugin-scoped namespace
  prefix (`<plugin_id>_` or `ext_<plugin_id>_`), manifest
  `tools.expose` allowlist, collision rejection.
  `NamespaceEnforcement::Warn` (default) records + logs +
  emits broker event but allows non-collision violations
  to fall through; `Strict` (`NEXO_PLUGIN_NAMESPACE_STRICT=1`)
  returns Err on every violation. Collisions ALWAYS rejected.
  `NamespaceViolation` + 4-variant `NamespaceViolationReason`
  (`ReservedPrefix(&'static str)`, `OutOfNamespace`,
  `NotInExpose`, `Collision`). Best-effort broker event
  `plugin.lifecycle.<id>.namespace_violation` (non-blocking,
  2s budget). `PluginInitContext.tool_registry: Arc<ScopedToolRegistry>`
  swap (non-breaking for the 4 in-tree plugins, none call
  `register*`). New `PluginInitError::ToolNamespace` variant.
  `init_loop::check_namespace_after_init` post-init drain +
  Strict-mode escalation to `InitOutcome::Failed`. `ctx_factory`
  closure signature changed from `FnMut(&str)` to
  `FnMut(&PluginManifest)`. `ToolRegistry::register_if_absent_arc`
  helper added. 1262/1262 nexo-core lib tests pass + 2/2
  e2e tests + 11 new scoped_tool_registry tests + 1 new
  init_loop test (`format_violation_sample_truncates_after_three`).
  Workspace builds clean (only pre-existing test failures
  unrelated to 81.3).

- **81.3.b ⬜** Per-plugin manifest override for namespace
  enforcement mode. New `[plugin.tool_namespace] mode = "strict"`
  optional section so plugin authors can opt their own
  releases into Strict regardless of operator default. Today
  enforcement is global via env var only. ~0.5 d.

- **81.3.c ⬜** `nexo agent doctor plugins --json` extension
  to surface namespace-violation history per plugin (count
  + last-N samples). Operators benefit from one-shot audit
  before flipping `NEXO_PLUGIN_NAMESPACE_STRICT=1` in
  production. ~0.5 d.

- **81.3.d ⬜** Flip `NamespaceEnforcement::from_env` default
  from `Warn` to `Strict` after a deprecation window. Pre-
  requisites: 81.3.c doctor surface lands; community plugin
  audit completes; `NEXO_PLUGIN_NAMESPACE_LENIENT=1` opt-out
  ships for the few plugins that legitimately need looser
  enforcement. ~0.3 d once gates clear.

- **81.4 ✅ shipped 2026-05-04** — Plugin-scoped config dir
  loader. New `crates/core/src/agent/plugin_config_loader.rs`
  module (~340 LOC + 13 unit tests). `load_plugin_config`
  reads `<config_dir>/plugins/<plugin_id>/*.yaml` alphabetical,
  canonicalizes + symlink-escape-guards each path, resolves
  `${ENV_VAR}` placeholders pre-yaml-parse, deep-merges
  (mappings recurse, arrays full-replace, scalars overwrite),
  validates against `manifest.config.schema_path` JSONSchema
  via `nexo_plugin_manifest::config_schema::validate_config`
  (lightweight subset: type/required/properties/additionalProperties/enum).
  `PluginConfig { merged: Value, schema_validated: bool,
  source_files: Vec<PathBuf> }`. `PluginConfigError` 8
  variants. New `PluginInitContext.plugin_config:
  Arc<serde_yaml::Value>` field — empty mapping when operator
  has placed no config files. `init_loop` runs config load
  BEFORE any factory work; failure short-circuits with
  `InitOutcome::Failed { error: "config load: …" }` plus
  `tracing::warn!`. `ctx_factory` closure signature changed
  from `FnMut(&PluginManifest)` to `FnMut(&PluginManifest,
  &Arc<serde_yaml::Value>)`. New `config_dir: &Path` arg on
  `run_plugin_init_loop_with_factory`. `SubprocessCtxStubs::context_for`
  extended with `plugin_config: &Arc<Value>`. Authoring docs
  updated with a "Plugin config dir" section explaining
  multi-file sharding + env-var interpolation + schema_path.
  Out of scope: hot-reload of plugin config files (defer
  81.4.b), `jsonschema 0.20` richer keywords (defer 81.4.c),
  doctor JSON surface (defer 81.4.d), broker emit on
  config_load_failed (defer 81.4.b).

- **81.4.b ⬜** Hot-reload of plugin config files via Phase
  18 reload coord post-hook. When `manifest.config.hot_reload
  = true` (default), file changes under
  `<config_dir>/plugins/<plugin_id>/*.yaml` re-run the loader
  + revalidate against the schema; on success, emit
  `plugin.lifecycle.<id>.config_reloaded` so the plugin can
  refresh its in-process state. Also adds best-effort broker
  emit `plugin.lifecycle.<id>.config_load_failed` on initial
  load failure (currently only `tracing::warn!`). ~1 d.

- **81.4.c ⬜** Richer JSONSchema validation behind
  `schema-validation` feature gate. Wires the existing
  `jsonschema 0.20` validator (already optional dep on
  nexo-core) so plugin schemas using `oneOf` / `$ref` /
  `pattern` / `format` / numeric bounds get validated
  properly. Today those keywords pass through silently in
  the lightweight subset validator. ~0.5 d.

- **81.4.d ⬜** `nexo agent doctor plugins --json` extension
  surfacing per-plugin `plugin_config: { source_files,
  schema_validated, last_load_error? }` so operators audit
  config without booting. Pairs with the existing
  `init_outcomes` + `namespace_violations` doctor surfaces.
  ~0.3 d.

- **81.28 ✅ shipped 2026-05-04** — Manifest `[plugin.extends]`
  section per-registry capability declaration. Additive 4-list
  schema (`channels` / `llm_providers` / `memory_backends` /
  `hooks`) + helpers (`is_empty`, `all_ids`,
  `registers(section, id)`) + `EXTENDS_SECTIONS` const + 3
  `ManifestError` variants + `validate_extends` wired in
  `run_all`. Contract spec bumped to v1.4.0 with new §2.1
  "Extends section"; vendored doc copy refreshed. Authoring
  docs gain a cross-link callout. 8 unit tests. Daemon dispatch
  wiring per registry comes in 81.24-27; capability-negotiation
  handshake deferred to 81.28.b. Doctor surface deferred to
  81.28.c.

- **81.28.b ⬜** Capability-negotiation handshake. The host's
  `initialize` reply parser cross-checks the subprocess's
  declared capabilities (TBD shape — likely a `capabilities:
  { channels: [...], llm_providers: [...], ... }` block on
  the reply) against `manifest.plugin.extends.*`. Mismatch
  fails plugin load with `PluginInitError::CapabilityMismatch`.
  Defends against manifests claiming kinds the binary doesn't
  actually implement. Bumps contract version to 1.5.0. ~1 d.

- **81.28.c ⬜** `nexo agent doctor plugins --json` surfaces
  per-plugin `extends: { channels: [...], llm_providers:
  [...], memory_backends: [...], hooks: [...] }` so operators
  audit declared capabilities without booting. Pairs with
  the existing `init_outcomes` + `namespace_violations` +
  `plugin_config` (81.4.d) doctor surfaces. ~0.3 d.

- **81.24 ✅ shipped 2026-05-04** — Remote `ChannelAdapter`
  wrapper (subprocess-backed). New
  `crates/core/src/agent/channel_adapter/remote.rs`
  (~340 LOC + 8 unit tests). `RemoteChannelAdapter` translates
  each `ChannelAdapter` trait method into a JSON-RPC request
  over the subprocess plugin's stdio bridge. 3 new wire
  methods: `channel.start { kind, instance }`,
  `channel.stop { kind }`, `channel.send_outbound { kind, msg }`
  — `kind` field allows one subprocess to advertise multiple
  kinds. Channel-specific error codes `-33001..=-33005` map to
  typed `ChannelAdapterError` variants. Default timeouts
  30s/30s/60s; `NEXO_PLUGIN_CHANNEL_TIMEOUT_MS` env override.
  New required `NexoPlugin::as_any` trait method (8 sites
  updated). New `ChannelAdapterRegistry::unregister(kind,
  plugin_id)` with ownership check. Boot integration via
  post-init hook in `init_loop` after
  `check_namespace_after_init`; rolls back partial
  registrations on `KindAlreadyRegistered`. `boot.rs` now
  shares one `Arc<ChannelAdapterRegistry>` between the post-
  init hook and the `WirePluginRegistryOutput` (was building
  a fresh empty registry — fixed). Contract spec bumped to
  v1.5.0 with new §5.x "Channel methods". Authoring docs gain
  "Contributing channel kinds" section. e2e test
  `remote_channel_e2e::send_outbound_round_trips_via_mock_subprocess`
  validates the full pipeline through a bash mock plugin.
  1285/1285 nexo-core lib + 2/2 subprocess e2e + 1/1 remote
  channel e2e + workspace clean + mdbook clean.

- **81.24.b ⬜** SDK child-side helpers
  (`PluginAdapter::handle_channel_start` / `handle_channel_stop`
  / `handle_channel_send_outbound`) so subprocess plugin
  authors don't hand-handle JSON-RPC frames. Pairs with the
  existing `on_broker_event` / `on_shutdown` builder methods
  in `nexo-microapp-sdk::plugin`. ~1 d.

- **81.24.c ⬜** Per-method timeout knobs in manifest
  (`[plugin.channels.<kind>] start_timeout_ms /
  stop_timeout_ms / send_timeout_ms`). Today
  `NEXO_PLUGIN_CHANNEL_TIMEOUT_MS` overrides all three at
  once — operators can't tune individually. ~0.3 d.

- **81.24.d ⬜** Capability-negotiation handshake at
  `initialize`-reply time validating the subprocess actually
  exposes `extends.channels` kinds. Today the manifest
  declaration is taken on faith; mismatch fails at first
  `channel.send_outbound` call instead of at boot. Duplicate
  of 81.28.b but specifically scoped to channels. ~0.5 d.

- **81.25 ✅ shipped 2026-05-04** — Remote `LlmClient`
  provider wrapper. New `crates/core/src/agent/llm_remote/{mod.rs,
  wire.rs}` (~480 LOC + 11 unit tests + 1 e2e).
  `RemoteLlmClient` translates `chat()` and `stream()` into
  JSON-RPC requests over the subprocess plugin's stdio bridge.
  Wire method `llm.chat { provider, model, request, stream }`
  (contract v1.6.0): non-streaming replies once with
  `WireChatResponse`; streaming emits zero or more
  `llm.chat.delta { request_id, chunk }` notifications + a
  final response carrying `usage`/`finish_reason`. 15 wire-only
  types keep `nexo-llm` types untouched (`PromptBlock.label`
  drops at boundary). LLM-specific error codes
  `-33101..=-33105` map to `anyhow::Error` with operator-
  greppable messages. Default timeouts 60s sync / 300s
  streaming; `NEXO_PLUGIN_LLM_TIMEOUT_MS` env override.
  `LlmRegistry::factories: HashMap<...>` →
  `RwLock<HashMap<...>>` so `register(&self, ...)` works
  post-Arc; new `unregister(&self, name) -> bool` for
  rollback. `LlmRegistry::names()` return type changed
  `Vec<&str>` → `Vec<String>`. `Inner.streaming_pending` field
  added; reader notification handler dispatches
  `llm.chat.delta`. `SubprocessNexoPlugin::register_remote_llm_providers(llm_registry)`
  post-init hook chained after channel registration.
  `init_loop::run_plugin_init_loop_with_factory` gained 5th
  arg `llm_registry: &Arc<LlmRegistry>`. Contract spec v1.6.0
  with §5.y "LLM provider methods" + Changelog row; vendored
  doc copy refreshed. Authoring docs gain "Contributing LLM
  providers" section. 11 unit tests + 1 e2e
  `remote_llm_e2e::llm_chat_round_trips_via_mock_subprocess`.
  1296/1296 nexo-core lib + 2/2 subprocess e2e + 1/1
  remote_channel_e2e + 1/1 remote_llm_e2e + workspace clean.

- **81.25.b ⬜** Embed support over the wire. New
  `llm.embed { provider, texts: [String] }` method →
  `Vec<Vec<f32>>`. Today `LlmClient::embed` for remote
  providers returns the trait's default Err. ~0.5 d.

- **81.25.c ⬜** Per-method timeout knobs in manifest
  (`[plugin.llm_chat] sync_timeout_ms / stream_timeout_ms`)
  so operators tune individually. Today
  `NEXO_PLUGIN_LLM_TIMEOUT_MS` overrides both. ~0.3 d.

- **81.25.d ⬜** Capability-negotiation handshake validating
  the subprocess actually exposes the declared providers at
  `initialize`-reply time. Mismatch fails at boot instead of
  at first `llm.chat` call. ~0.5 d.

- **81.25.e ⬜** Bounded streaming sender for `llm.chat.delta`
  to prevent flood from a slow consumer. Today
  `mpsc::unbounded_channel` allows the child to fill memory
  if the consumer can't keep up. Switch to `mpsc::channel(N)`
  with drop-on-full + warn. ~0.3 d.

- **81.27 ✅ shipped 2026-05-04** — Remote `HookInterceptor`
  wrapper. New `crates/core/src/agent/hook_remote.rs`
  (~280 LOC + 8 unit tests + 1 e2e). `RemoteHookHandler`
  implements `HookHandler` trait by translating
  `on_hook(name, event)` into `hook.on_hook` JSON-RPC
  requests. Wire reply shape is the existing `HookResponse`
  (already serde-derived — reused directly). Continue-on-error
  semantic: every dispatch failure returns
  `Ok(HookResponse::default())` so `HookRegistry::fire`
  iterates + agent flow doesn't break. Default 5s timeout;
  `NEXO_PLUGIN_HOOK_TIMEOUT_MS` env override.
  `WirePluginRegistryOutput.hook_registry: Arc<HookRegistry>`
  field added. `SubprocessCtxStubs::build_with_shared_registries`
  combined constructor. `init_loop` gained 6th arg + new
  `register_remote_hook_handlers_after_init` post-init hook
  chained after llm. Factory-registered Ok arm in init_loop
  also got the full chain (was missing llm+hook before — fix).
  Contract v1.7.0 with §5.z "Hook methods". Authoring docs
  gain "Contributing hook handlers" section. 1304/1304
  nexo-core lib + 4 e2e tests + workspace clean.

- **81.27.b ⬜** Per-hook priority via manifest. Today remote
  hooks register with default priority 0; operator-side
  priority tuning needs a manifest field. ~0.3 d.

- **81.27.c ⬜** Capability-negotiation handshake validating
  the subprocess actually exposes declared hook names at
  `initialize`-reply time. Pairs with 81.28.b umbrella.
  ~0.5 d.

- **81.26 ✅ shipped 2026-05-04** — Remote memory backend
  wrapper. `VectorBackend` trait + 5 wire types
  (`VectorRecord`/`VectorQuery`/`VectorMatch`/`UpsertAck`/
  `DeleteAck`) in `nexo-memory`. `VectorBackendRegistry`
  (BTreeMap, ownership-checked unregister) +
  `RemoteVectorBackend` (subprocess JSON-RPC, 3 wire methods
  `memory.vector_upsert`/`vector_search`/`vector_delete`,
  error band `-33301..=-33304`, default 30s upsert/delete +
  10s search timeouts overridable via
  `NEXO_PLUGIN_MEMORY_TIMEOUT_MS`) in `nexo-core`. Post-init
  hook `register_remote_vector_backends_after_init` chained
  in both auto-subprocess + factory-registered Ok arms.
  `WirePluginRegistryOutput.vector_backend_registry` exposed
  to consumers. Contract v1.8.0 §5.w. Authoring docs gain
  "Contributing memory backends" section. **Completes the
  4-wrapper quartet** (81.24 channels + 81.25 LLM + 81.27
  hooks + 81.26 memory) backed by 81.28
  `extends.<section>` manifest schema. 1316/1316 nexo-core
  lib + e2e mock-subprocess test + workspace clean.

- **81.26.b ⬜** Consumer-side wiring:
  `LongTermMemory.recall_vector` resolves through
  `VectorBackendRegistry` instead of the in-tree sqlite-vec
  default when an `extends.memory_backends` plugin is
  active. Today the registry is exposed via
  `WirePluginRegistryOutput` but no in-tree call path
  consults it. ~1.5 d.

- **81.26.c ⬜** Typed `MemoryBackendError` enum replacing
  `anyhow::Error` returns from `RemoteVectorBackend`.
  Variants for `BackendUnavailable`/`InvalidQuery`/
  `BackendInternal`/`Timeout` aligned with -33301..=-33304
  band. ~0.5 d.

- **81.26.d ⬜** `RegistriesBundle` consolidation. After
  81.24-27, `run_plugin_init_loop_with_factory` carries 7
  registry args (channel/llm/hook/vector + 3 contextual).
  Bundle them into a single struct passed by reference.
  ~0.3 d.

- **81.26.e ⬜** Binary embedding encoding for large
  vectors. Wire format currently sends `Vec<f32>` as JSON
  array; for embedding dims ≥ 1024 this is wasteful.
  Optional base64-encoded `f32[]` field in
  `VectorRecord`/`VectorQuery` with feature negotiation at
  `initialize`. ~0.5 d.

- **81.29 ✅ shipped 2026-05-04** — Remote ToolHandler
  wrapper (5th wire surface). Manifest gains
  `extends.tools = [...]` (5th list); EXTENDS_SECTIONS grew
  4→5. `tool.invoke` JSON-RPC host→child request + initialize-
  reply `tools` array extension (`{name, description,
  input_schema}` per tool). Error band -33401..=-33405.
  Default timeout 60 s; NEXO_PLUGIN_TOOL_TIMEOUT_MS env
  override. RemoteToolHandler shares `Inner.{stdin_tx,
  pending, next_id}` Arc with siblings. Subset check
  (advertised ⊆ declared) at handshake — drift fails boot.
  Post-init hook chained after vector backends in both Ok
  arms; uses `ctx.tool_registry` so init_loop arg count
  stays at 8. WirePluginRegistryOutput exposes shared tool
  registry. ScopedToolRegistry seeded with
  `tools.expose ∪ extends.tools`. Contract v1.10.0 §4.1.1
  + §5.t. Authoring docs "Contributing tools" section.
  Completes the 5-wrapper subprocess fleet — unblocks
  81.17.c/18/19 plugin extracts. 1349/1349 nexo-core +
  110/110 manifest + 10/10 tool_remote + 8/8 e2e tests pass.

- **81.29.b ⬜** SDK helper `PluginAdapter::on_tool(name,
  handler)` registration API mirroring `on_broker_event`.
  Plugin authors today hand-roll the JSON-RPC frame parsing
  for tool.invoke; SDK helper drops boilerplate. ~0.5 d.

- **81.29.c ⬜** Per-tool timeout knob in manifest
  `[plugin.tools.timeouts] browser_navigate = "30s"`. Today
  a single env var covers all tools per plugin; some need
  longer (full-page-load) than others (button-click).
  ~0.3 d.

- **81.29.d ⬜** Streaming tool dispatch via
  `tool.invoke.delta { request_id, chunk }` notifications.
  Mirrors 81.25 LLM streaming shape. Use case: tools
  returning chunked output (browser_screenshot binary
  blob, web_fetch large body). ~1 d.

- **81.22 ✅ shipped 2026-05-04** — Plugin sandbox v1
  (bwrap-based, fs/network allowlist). New
  `[plugin.sandbox]` manifest section (5 fields, default
  off) + `SandboxSection` types + hard denylist consts
  (17 host paths + 11 home subpaths, compile-time) +
  `validate_sandbox` + 4 new ManifestError variants. New
  `SandboxRunner` discovers `bwrap` once at boot + caches
  env-driven capability flags
  (`NEXO_PLUGIN_SANDBOX_REQUIRE` / `NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW`).
  `spawn_and_handshake` wraps `Command::new(...)` with
  bwrap argv when `enabled = true`. macOS = no-op + warn
  (defer 81.22.macos). Capability inventory: 2 new
  entries + 7 pre-existing NEXO_* envs added to
  NON_DANGEROUS_ENV_ALLOWLIST cleaning the drift test.
  Contract v1.9.0 §2.2 + authoring docs "Sandboxing your
  plugin" section. 1331/1331 nexo-core lib + 102/102
  plugin-manifest + 273/273 setup + 6/6 e2e tests pass +
  workspace builds clean + mdbook clean.

- **81.22.b ⬜** Granular network egress allowlist
  (`network = "allowlist", network_allowlist =
  ["api.example.com:443"]`). Today only `deny` and `host`.
  Implementation: slirp4netns (already used by Podman) +
  nftables rules built from manifest list. Per-host CIDR /
  port-range tightening. ~2 d.

- **81.22.c ⬜** Per-syscall seccomp filters via
  `seccompiler` crate. Plugin-specific syscall sets
  layered on top of bwrap's default filter. Manifest
  declares `[plugin.sandbox.seccomp] profile =
  "<default|tight|custom>"` with custom JSON-rule
  optional. Risk: complexity + plugin-author confusion if
  defaults are wrong. ~3 d.

- **81.22.d ⬜** Doctor CLI sandbox surface. `nexo agent
  doctor plugins` extends with per-plugin
  `{ sandbox_required, sandbox_active, runner_path,
  network_mode, fs_paths }` rows. JSON envelope mirror.
  ~0.5 d.

- **81.22.macos ⬜** Native macOS sandbox-exec
  integration. Generate `.sb` profile from manifest at
  spawn time. Risk: Apple has marked sandbox-exec
  deprecated since 10.15; could break in a future macOS
  release. Defer until enterprise customer asks. ~2 d.

- **81.22.e ⬜** Symlink canonicalization for sandbox
  allowlist entries. Today validator does string-match
  only — a symlinked path like `/var/log/myapp -> /etc`
  could in theory be exposed without the denylist
  catching it. Real-world risk low (operator-supplied
  paths, not attacker-supplied) but defense-in-depth
  hardening is cheap. ~0.3 d.

- **81.15.c.b ✅ shipped 2026-05-01** — SDK streaming
  consumption helper. Pending value type changed to
  `PendingKind` enum (Single for non-streaming, Streaming for
  delta + final channels). New `BrokerSender::complete_llm_stream`
  returns `LlmStream` handle with `next_chunk()` +
  `await_final()` API. Dispatch loop notification path handles
  `llm.complete.delta`. `LlmStream.finished` is `Option<...>`
  so `await_final` can `take()` despite Drop impl. Drop impl
  cleans up pending entry on early-drop. 11/11 SDK plugin
  tests pass.

- **81.20.c ⏸ DEFERRED** — `tool.dispatch` RPC. Original ~1d
  estimate wrong: `ToolHandler::call` requires full AgentContext
  (~25 fields of per-running-agent state owned by main.rs's
  per-agent loop). Either Path A (new AgentContextRegistry,
  ~2-3 d, proper) or Path B (stub AgentContext with broker/
  sessions only, most tools break). Defer until path A
  architecturally needed. memory.recall + llm.complete cover
  the SaaS-ish use cases driving the microapp / agent-creator
  program; tool.dispatch value comes mainly from in-tree tools
  that already work in-process.

- **81.20.c ⬜** Daemon-mediated `tool.dispatch` RPC. Extends
  `handle_child_request` match. Needs ToolRegistry in
  BridgeContext (today only stubbed via SubprocessCtxStubs).
  Result shape mirrors existing ToolReply. ~1 d.

- **81.21 ✅ shipped 2026-05-01** — Plugin supervisor (MVP:
  crash detection + broker event). Inner.child wrapped as
  `Arc<Mutex<Option<Child>>>`. Supervisor task polls `try_wait()`
  every 500ms; on exit publishes
  `plugin.lifecycle.<id>.crashed { plugin_id, exit_code }` event
  on broker (when wired) with `source = "plugin.supervisor"` +
  warn log + cascades cancel.cancel(). New helper
  `kill_handle(&Arc<Mutex<Option<Child>>>)` consolidates kill
  sites. shutdown() locks the mutex idempotently with supervisor.
  1 new unit test + 3 existing task-count assertions updated.
  15/15 subprocess + 2/2 e2e tests pass. Auto-respawn +
  backoff + resource limits deferred to 81.21.b/.c.

- **81.21.b ✅ shipped 2026-05-01** — Plugin supervisor: stderr
  tail capture + manifest.supervisor config. New
  `[plugin.supervisor]` section (additive) with `respawn`,
  `max_attempts`, `backoff_ms`, `stderr_tail_lines` (capped at
  `SUPERVISOR_STDERR_TAIL_MAX = 512`, validation rejects above).
  Stderr reader populates a `VecDeque<String>` ring buffer;
  supervisor on crash drains into the `stderr_tail: [String]`
  field of the `plugin.lifecycle.<id>.crashed` event payload.
  `respawn = true` parses + validates but logs a one-shot
  reminder that the actual loop is in 81.21.b.b. 17/17
  subprocess tests + 2/2 e2e + 295 in-tree plugin tests pass.

- ✅ ~~**81.21.b.b**~~ Plugin supervisor auto-respawn loop —
  shipped 2026-05-10 in nexo-core 0.1.13. Architecture: rename
  `spawn_and_handshake` → `spawn_one_attempt`; new `respawn_loop`
  owns lifecycle + replaces Inner transparently; supervisor task
  refactored to detect-only via AttemptOutcome oneshot;
  `weak_self: OnceLock<Weak<Self>>` populated by both subprocess
  factories so init_loop's new
  `start_plugin_supervisor_loop_after_init` hook can upgrade back
  to typed Arc. 4 lifecycle events: `crashed` (existing) +
  `respawning` + `respawned` + `gave_up`. Backoff exponential
  capped 60s; reset-counter heuristic (`base_ms × max_attempts × 2`);
  pending oneshots drained "plugin restarted; retry" before new
  Inner install; shutdown short-circuits backoff via Notify;
  race protection re-checks `shutdown_signaled` between
  `spawn_one_attempt` Ok and install (kills new child if
  shutdown won). `respawn=false` keeps Phase 81.21.b semantics.
  4 unit (`next_backoff`) + 4 integration tests pass. Docs:
  `docs/src/plugins/supervisor.md`.

- **81.21.b.b deferred test cases ⬜** 6 test scenarios planned
  but skipped during shipping because the heuristic + mock
  scripting was fiddly:
  1. `pending_drained_with_retry_error_before_new_inner` —
     observable timing of pending oneshot drain vs new Inner
     install; needs internal RPC handler to fake a pending entry.
  2. `attempt_counter_resets_after_window` — script that crashes,
     stays alive `> reset_window_ms`, then crashes again; verify
     attempt counter at 0 not at 2.
  3. `sandbox_stash_reused_across_respawns` — mock SandboxRunner
     with call counter; verify second spawn reuses the stash.
  4. `respawn_handshake_failure_counts_as_attempt` — script
     succeeds first time, fails handshake on respawn; verify
     attempt bumps + eventual gave_up.
  5. `shutdown_during_respawn_handshake_kills_new_child` —
     script with artificial 2s handshake delay; trigger
     shutdown 200ms in; verify no new Inner installed, no
     respawned event.
  6. `lifecycle_event_payload_shapes_match_spec` — golden JSON
     assertion exercising all 4 events with exact payload field
     verification.

- ✅ ~~**81.21.b.b: `nexo/admin/plugins/restart` admin RPC**~~
  shipped 2026-05-10 in nexo-tool-meta 0.1.13 + nexo-core 0.1.17 +
  nexo-plugin-admin 0.1.11. Backend: new
  `EncryptionKey`-style additive variant pattern — but for
  lifecycle, not data. New `force_restart(self: Arc<Self>, ...)`
  method on SubprocessNexoPlugin bypasses the auto-respawn
  loop's natural Crashed flow so no spurious crashed event
  fires for an intentional kill. Coordinated via existing cancel
  cascade + new `restart_signaled` flag (sibling of
  shutdown_signaled). Spawns a fresh respawn_loop after install
  via `weak_self_arc().spawn_supervisor_loop()`. New trait
  `PluginRestarter` + dispatcher arms + capability
  `plugin_restart` (distinct from read-only `plugin_doctor`).
  `LivePluginRestarter` adapter looks up plugin handles
  snapshot, downcasts to SubprocessNexoPlugin, calls
  force_restart. New `plugin.lifecycle.<id>.restarted_manually`
  event with `{plugin_id, previous_uptime_ms, restarted_at_ms,
  new_pid?}` payload. Frontend: RestartPluginModal with
  confirm-by-typing-prefix + per-row Restart button in
  PluginsMain. 60s server-side timeout for spawn handshake.
  Tests: 3 unit (handler) + 3 adapter + 4 mock-script
  integration (replaces inner / publishes restarted_manually /
  after gave_up recovers / drains pending with retry error).
  Docs: `docs/src/plugins/supervisor.md` § Manual restart.
  **Open**: SharedPluginHandles cell pattern needed in main.rs
  to actually wire LivePluginRestarter (admin bootstrap runs
  BEFORE wire_plugin_registry); today the dispatcher slot is
  None and the RPC returns "plugin restart domain not
  configured". External integrations can construct
  LivePluginRestarter directly + wire via
  `dispatcher.with_plugin_restarter()`.

- ✅ ~~**81.21.b.b: SharedPluginHandles cell + main.rs wiring**~~
  shipped 2026-05-10. New `SharedPluginHandles =
  Arc<RwLock<Option<Arc<BTreeMap<String, Arc<dyn NexoPlugin>>>>>`
  type alias + `shared_plugin_handles_cell()` helper in
  `crates/setup/src/admin_adapters.rs` (mirror of existing
  `SharedMemorySnapshotter` pattern). `LivePluginRestarter::new`
  signature breaking-changed to take cell instead of direct map;
  `restart()` clones Arc<BTreeMap> early (drops RwLock guard
  before slow force_restart path). Empty-cell branch returns
  `"plugin handles not yet populated; daemon still booting"` error
  for the brief boot window. `proyecto/src/main.rs`: cell created
  pre-bootstrap, LivePluginRestarter constructed with cell +
  `Some(...)` in AdminBootstrapInputs (was None), late-init write
  AFTER `wire_plugin_registry_with_runtime` returns. Tests: 3
  refactored + 1 new (`live_plugin_restarter_returns_clear_error_on_empty_cell`)
  via `build_restarter()` helper that wraps cell internally; 4/4
  pass. Docs: supervisor.md § Manual restart Errors table gains
  the boot-window row. NO crates.io publish (nexo-setup +
  proyecto path-dep only). The official daemon's
  `nexo/admin/plugins/restart` is now FULLY OPERATIONAL.

- ✅ ~~**81.21.b.b: real `respawned.total_uptime_ms` telemetry**~~
  shipped 2026-05-10 in nexo-core 0.1.16. Captures
  `inner.spawned_at.elapsed()` of the dying Inner BEFORE the
  drain (still inside `self.inner.lock()`), then forwards the
  millis count into the subsequent `respawned` event payload.
  Operators graph per-cycle duration to spot plugins whose
  stable lifetime is degrading. Test
  `respawned_event_carries_previous_inner_uptime` asserts
  uptime > 0 + < 5s for the fast-crash mock script setup.

- **81.21.b.b: Prometheus counter ⬜**
  `nexo_plugin_respawn_total{plugin_id, outcome}`. Pending the
  general metrics pipeline.

- **81.21.c ⬜** Plugin resource limits. OS-divergent: linux
  cgroup v2 + rlimit, macOS sandbox-exec resource caps, fallback
  to monitoring on others. Manifest knobs `limits.cpu_pct` /
  `limits.mem_mb` / `limits.startup_timeout_ms`. Required to
  gate community-tier plugins. ~3 d.

- **81.23 ✅ shipped 2026-05-01** — Plugin stdio → daemon tracing
  bridge. subprocess.rs flips `stderr(Stdio::null())` →
  `stderr(Stdio::piped())` + new stderr reader task forwards
  each line as `tracing::info!(target: "plugin.stderr",
  plugin_id, line)`. Stdout reader's non-JSON path downgraded
  from warn-drop to `tracing::info!(target: "plugin.stdout", ...)`.
  Stderr reader spawned BEFORE handshake so boot-time errors
  land in operator logs. 1 new unit test + 2 existing task-count
  assertions updated. Operators filter via
  `RUST_LOG=plugin.stderr=info,plugin.stdout=info`. 14/14
  subprocess + 2/2 e2e tests pass. Structured field extraction
  (parsing tracing-subscriber JSON output from child) deferred
  to 81.23.b.

- **81.23.b ⬜** Structured field extraction from child tracing.
  Today child stderr lines forward as opaque `line = "..."` field.
  When the child uses `tracing-subscriber` with JSON formatter,
  the daemon could parse each line + emit a structured event
  with the child's spans / fields preserved. Requires the SDK
  side to standardize on JSON output and the host side to
  attempt JSON parsing on each stderr line (fall back to opaque
  on parse failure). ~1 d effort.

- **81.15.b ✅ shipped 2026-05-01** — Rust plugin template
  drafted in-workspace as `extensions/template-plugin-rust/`.
  Cargo.toml + nexo-plugin.toml + src/main.rs (~70 LOC) +
  README. Workspace member so CI keeps it green. Handshake
  smoke test passes. Doubles as 81.17.c-validation (real Rust
  binary proves the wire format end-to-end vs the prior bash
  mock). Phase 31.6 `nexo plugin new --lang rust` scaffolder
  will publish this as the external
  `github.com/nexo-rs/plugin-template-rust` repo when 31.6 ships
  (just adds GitHub Actions CI for build-per-target + cosign
  + auto-PR to ext-registry on tag).

Critical path: 81.1 → 81.2 → 81.5 → 81.9 (~3 días). Después
de 81.9 plugin model is fully operational. Out-of-tree path
adds 81.14 → 81.14.b → 81.15 → 81.16 → 81.17 → 81.18 →
81.19 → 81.20-81.28 → Phase 31 marketplace.

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

### Phase 83.14 — actual crates.io upload + release-plz CI + npm — ✅ shipped 2026-05-10 (83.14.b)

Phase 83.14 shipped publish-readiness for the four Tier-A
crates: clean dry-run on `nexo-tool-meta`,
`nexo-plugin-manifest`, `nexo-compliance-primitives`. Per-crate
README.md + CHANGELOG.md present. Publishing doc
(`docs/src/microapps/publishing.md`) covers the dependency
order.

Phase 83.14.b execution shipped 2026-05-10:

- ✅ **Actual crates.io upload**: tool-meta@0.1.6, core@0.1.6,
  microapp-sdk@0.1.14 published via `cargo release patch -p X
  --no-verify --execute`. Two prereqs published as straight
  `cargo publish` (no bump) because they had never reached the
  registry: nexo-sanitize@0.1.6 + nexo-media@0.1.6.
- ✅ **out-of-tree consumer migration**: agent-creator-microapp's
  Cargo.toml flipped from 4 path-deps to versioned crates.io
  refs in commit `eac0fe8`. `cargo build --profile release-fast`
  passes standalone.
- ✅ **release.toml hardening**: dropped unrendered
  `{{crate_name}}` from `pre-release-commit-message`
  (cargo-release 1.x doesn't render it under
  `consolidate-commits = true`). Past commits had literal
  `{{crate_name}}` in the log; future ones won't.
- ✅ **workspace.dependencies fix**: nexo-sanitize +
  nexo-media added at workspace scope so `microapp-sdk`
  consumes them through `workspace = true`. Before, the SDK
  declared them as path-only which failed publish-time
  validation (`all dependencies must have a version
  requirement`).

Deferred (intentional, low risk):

- **release-plz CI integration**: superseded by cargo-release
  manual flow per memory `feedback_cargo_release_not_release_plz`.
  CI publish workflow on tag push is a separate item if/when we
  want it.
- **npm package**: ✅ already shipped — `@lordmacu/nexo-microapp-ui-react`
  on npm at 0.1.0+ since 83.13 theme work.

**Followup completed.**

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

### Phase 82.10.n — Channel credential persisters (deferred)

Trait + dispatcher + telegram/email/whatsapp persisters shipped
2026-05-03. Five follow-ups left intentionally:

- **82.10.n.cb** — wrap the telegram `getMe` + email IMAP probes
  with `nexo-resilience::CircuitBreaker`. Today both use only
  `tokio::time::timeout(5s)`. CB has no value for one-shot
  probes; add when continuous health monitoring (82.10.n.health)
  lands so repeated failures stop hammering the provider.
- **82.10.n.health** — periodic re-probe scheduler that emits
  `nexo/notify/credential_health` events. Operator UIs render a
  rolling badge instead of a register-time snapshot.
  Implementation lives in `nexo-setup` next to the persisters
  with a single `tokio::spawn`'d loop.
- **82.10.n.imap-login** — extend the email probe to issue
  `LOGIN + NOOP + LOGOUT` over IMAP. Today it stops at TCP +
  TLS handshake; auth failure surfaces only on first inbound
  poll cycle. Either reuse `nexo-plugin-email::ImapConnection`
  from `nexo-setup` (heavy — needs `EmailAccount` +
  `GoogleCredentialStore` construction) or roll a minimal
  IMAP client in the persister.
- **82.10.n.starttls** — the email probe currently only does
  the TLS handshake when `metadata.imap.tls = "implicit_tls"`.
  STARTTLS path skips the handshake (TCP-reach check only)
  because the IMAP `* OK` greeting + `STARTTLS` issuance are
  not implemented at the persister layer. Rolls into the
  `82.10.n.imap-login` work.
- **82.10.n.slack** — bundled `SlackPersister` when the slack
  channel plugin lands. Trait + dispatcher already accept any
  channel id — adding a new persister is a single
  `crates/setup/src/persisters/slack.rs` file + push into
  `src/main.rs`'s `AdminBootstrapInputs.persisters` vec.

### Phase 81.13 — Plugin manifest schema unification (deferred)

Foundation + dispatcher + microapp shipped 2026-05-04. Steps
that stay pending:

- **81.13.b.preserve** — extend the v2 schema with v2 homes for
  the legacy fields the migrator currently DROPS-with-warn:
  `mcp_servers`, `outbound_bindings`, `context.passthrough`,
  `requires.bins+env`,
  `capabilities.tools+hooks+channels+providers+pollers`,
  `transport.kind=nats|http`, `plugin.priority`. Today these
  stay readable via `nexo-extensions::manifest::ExtensionManifest`
  legacy parser; sub-phase folds them into the canonical v2
  shape so the migrator stops dropping them.
- **81.13.b.in-tree-migrate** — rewrite the 33 in-tree
  `plugin.toml` files to `manifest_version = 2` so they stop
  emitting deprecation warns at boot. Mechanical script work
  per-family commit (extensions/, crates/plugins/, templates/).
  Defer until `81.13.b.preserve` lands so we don't need a
  second migration pass.
- **81.13.b.json-schema** — JSON-Schema export of the canonical
  v2 shape for editor autocomplete + CI validation. Mirrors
  OpenClaw's `openclaw.plugin.json` pattern.
- **81.13.hard-remove** — drop `nexo-plugin.toml` filename
  fallback + `manifest_version = 1` legacy support entirely
  (target nexo-rs 0.2.0). Plugins still on v1 fail boot with a
  migration message.

### Phase 83.13 — `@lordmacu/nexo-microapp-ui-react` component library

MVP shipped 2026-05-10. Sibling repo
`/home/familia/chat/nexo-rs-microapp-ui-react/` + GitHub
`lordmacu/nexo-microapp-ui-react`. Five components extracted
(3 chat + 2 primitives). agent-creator-microapp consumes via
`file:` dep + vite alias. Three follow-ups capture the gaps:

- **`83.13.publish-npm`** — RESOLVED 2026-05-10. First publish
  attempted same day hit `403 Forbidden` (token lacked publish
  permission for `@lordmacu` scope OR 2FA-required-for-publish
  active on the account). User flipped 2FA bypass + retried;
  `@lordmacu/nexo-microapp-ui-react@0.0.1` published to
  https://www.npmjs.com/package/@lordmacu/nexo-microapp-ui-react.
  agent-creator-microapp/frontend/package.json swapped the
  `file:../../nexo-rs-microapp-ui-react` dep for `^0.0.1`
  (vite alias in vite.config.ts stays in place so a local
  checkout still overrides during cross-package dev).

- **`83.13.stateful-extraction`** — port 11 stateful chat
  components (`Chat`, `ChatHeader`, `ChatListItem`,
  `Conversation`, `ChatsMain`, `BotChatBubble`,
  `ConnectionBanner`, `InputBar`, `LabelManagerModal`,
  `EscalationBadge`, `PauseIndicator`) plus `MessageBubble`
  to the lib. Requires either: (a) per-component refactor
  to accept state via props with consumer wrapping, OR
  (b) move associated stores + types to the lib (heavy,
  changes shape of the lib). Defer until a second
  microapp consumer materialises (Phase 83.10). Owner:
  framework. Trigger: 83.10 second microapp landing.
  Status: pending.

- **`83.13.theme-system`** — RESOLVED 2026-05-10. Shipped
  in `@lordmacu/nexo-microapp-ui-react@0.1.0`:
    * `src/styles.css` — 22 design tokens as
      `--nexo-microapp-*` CSS custom properties with a
      `[data-theme="dark"]` override block. Default values
      mirror agent-creator's previous Tailwind hex palette
      1:1 so visual parity is preserved.
    * `tailwind.preset.js` — opt-in Tailwind preset mapping
      utility-class tokens (`bg-accent`, `text-text-primary`,
      `bg-panel-alt`, …) onto the CSS vars. Consumer adds via
      `presets: [require('@lordmacu/nexo-microapp-ui-react/tailwind.preset')]`.
    * `package.json` `exports` map exposes `./styles.css` and
      `./tailwind.preset` so consumers `import` them by
      package-name path.
  agent-creator-microapp consumed in commit `5637bd7`:
  imported `styles.css` in `src/styles/index.css`, swapped
  local `theme.extend.colors` block for the lib's preset,
  build verde with all 22 CSS vars present in
  `dist/assets/index-*.css`.

  Three out-of-scope items deferred to their own follow-ups:
  - `83.13.theme-daltonized` — color-blind accessibility
    variants (mirrors `claude-code-leak/src/utils/theme.ts`
    `lightDaltonized` / `darkDaltonized` themes).
  - `83.13.theme-auto-detect` — `prefers-color-scheme`
    media-query helper (operators wire their own opt-in
    today).
  - `83.13.theme-multi-palette` — additional named themes
    beyond `default` + `dark` (e.g. `data-theme="ocean"`).
  Status: ✅ closed.

### Phase 83.12 — Meta-microapp React UI reference scaffold

Shipped 2026-05-10 (audit close-out). Frontend scaffold
in `agent-creator-microapp/frontend/` covers the bulk of the
spec; four follow-ups capture the gaps:

- **`83.12.audit-page`** — RESOLVED 2026-05-10. New `audit`
  module in `agent-creator-microapp/frontend/src/modules/`
  (rail order 60, FileText icon) consuming the new
  `nexo/admin/microapp_audit/tail` admin RPC. Backend changes:
  4 wire types (`AdminAuditRow`, `AdminAuditResult`,
  `AuditTailFilter`, `AuditTailPage`) moved to
  `nexo-tool-meta::admin::audit` with `#[ts(export)]` derives;
  `AdminAuditReader` trait added in `nexo-core` (separate
  from `AdminAuditWriter` per read/write SoC); dispatcher
  gains `audit_reader` field + `with_audit_reader` setter +
  `audit_read` capability gate. `SqliteAdminAuditWriter` impls
  both traits so the same Arc satisfies write + read paths.
  Bumps: `nexo-tool-meta` 0.1.4 → 0.1.5, `nexo-core` 0.1.4 →
  0.1.5. Frontend: Zustand store with paged reload +
  loadMore (OpenClaw `cron.runs` pattern), filter row
  (microapp/method/result/since), div-based list with
  click-to-copy args-hash. plugin.toml gains `audit_read` in
  optional capabilities (graceful denial when not granted).
  Status: ✅ closed.

- **`83.12.llm-keys-page`** — full `pages/llm_keys.tsx`
  consolidating list + create + rotate + delete of LLM
  provider entries. Today the `LlmInstanceCreateModal`
  handles the create path only; list/rotate/delete go
  through admin RPC manually. Owner: microapp UI.
  Trigger: operator demand for end-to-end key
  management. Effort: ~1-2h. Status: pending.

- **`83.12.ts-types-codegen`** — RESOLVED 2026-05-10. Adopted
  `ts-rs` v12 in `nexo-tool-meta` with a feature-gated
  `ts-export` build flag (zero runtime cost when off). 16
  wire types now carry
  `#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]`:
    - 11 wire wrappers: `AgentEventKind`, `TranscriptRole`,
      `SecurityEventKind`, `BindingContext`, `InboundKind`,
      `InboundMessageMeta`, `EventSourceMeta`,
      `OutboundReplyContext`, `WebhookEnvelope`,
      `MicroappError`, `MicroappErrorKind`.
    - 5 dependent enums: `ProcessingScope`,
      `ProcessingControlState`, `EscalationReason`,
      `EscalationUrgency`, `ResolvedBy`.
  Wrapper script `proyecto/scripts/regen-ts-types.sh` runs
  `cargo test --features=ts-export -p nexo-tool-meta`
  (with `TS_RS_LARGE_INT=number` so u64 fields render as
  JS `number` to match agent-creator's existing convention)
  and concatenates per-type ts-rs output into a single
  `agent-creator-microapp/frontend/src/api/types.gen.ts`
  with a banner header + inline `JsonValue` alias.
  CI lint `proyecto/scripts/lint-ts-types-sync.sh` (mirror of
  the locale-list pattern) snapshot-and-diffs to catch drift;
  restores the snapshot on failure to keep the checkout clean.
  13 of the 16 generated types selectively re-exported from
  `agent-creator-microapp/frontend/src/api/types.ts`. Three
  types stay hand-written because the frontend extends
  (`AgentEventKind` adds CSR-only `WhatsappBotMessageEvent`)
  or narrows (`ProcessingScope` is wire-`conversation`-only
  on the firehose; `ProcessingControlState` + `ResolvedBy`
  follow). `nexo-tool-meta` bumped 0.1.3 → 0.1.4 (additive
  minor; feature is opt-in). Status: ✅ closed.

- **`83.12.e2e-tests`** — playwright / cypress harness
  covering login → agents CRUD → pairings → conversations.
  Today only unit tests run (`vitest run`). Reference
  scaffold's spec asks for "4+ playwright/cypress tests"
  but the harness wasn't wired. Owner: microapp QA.
  Trigger: when a regression hits production paths
  (login flow, pairing UX), OR proactive release
  hardening. Effort: ~3-5h (browser fixture + 4 tests).
  Status: pending.

## Maintenance note

If a future historical import includes non-English notes, keep them in `archive/spanish/*.txt` and update this Markdown tracker in English only.
