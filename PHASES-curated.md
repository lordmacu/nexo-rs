# Phases — curated active scope (2026-05-01)

**Purpose**: single source of truth for what nexo-rs is going to
build vs. what was deliberately dropped or deferred. Use this
file when planning sprints — do not re-derive priorities by
re-reading the full `PHASES.md` / `PHASES-microapps.md` /
`FOLLOWUPS.md` each time.

**Curation principles** (the lens used for every decision below):

1. **Connector framework, not model provider** — nexo-rs connects
   to LLMs (Anthropic / MiniMax / OpenAI / Gemini / DeepSeek /
   xAI / Mistral / future). Anything that assumes nexo *hosts* a
   model is out of scope.
2. **Microapp builder service** — the framework's primary
   consumer is now the microapp author building product features
   (e.g. `agent-creator-microapp`). Features that only serve
   autonomous-agent use cases without a microapp story land
   lower on the queue.
3. **No redundant capability** — if a sub-phase duplicates
   something already shipped (or trivially achievable by chaining
   shipped pieces) it gets dropped.
4. **No scope creep into ecosystems we don't control** — Python
   / TypeScript reference templates, third-party container
   runtimes, push-notification provider integrations all stay
   out unless the microapp framework itself demands them.

---

## Pickup order — read this first

Sub-phases are tagged with one of these labels. Pick from the
top of the list when starting a sprint; do not skip ahead.

| Tag | Meaning |
|-----|---------|
| **P0 — NEXT** | In-flight or the immediate blocker. One sub-phase carries this at any time; finish before pulling another P0. |
| **P1 — CRITICAL PATH** | Blocks shipping the current product (today: `agent-creator-microapp`). Pull as soon as the active P0 lands. |
| **P2 — PARALLEL** | High-leverage hardening / observability that can run alongside P1 without blocking. Pull when you have spare cycles or a separate contributor. |
| **P3 — POST-CRITICAL** | Waits on P1/P2 predecessors. Don't pull early — you'll re-do work. |
| **DEFER** | Real use case but the trigger has not arrived. Don't touch unless the trigger fires. |
| **DROPPED** | Removed from active scope. Don't touch. |

### Suggested pickup order (top → bottom)

1. **P0** — Phase 84.1 coordinator persona system prompt (in flight: brainstorm + spec + plan approved → `/forge ejecutar 84.1`)
2. **P0** — Phase 84.2 → 84.3 → 84.4 → 84.5 (chain of the current phase)
3. **P1** — Phase 82.12 HTTP server hosting (agent-creator can't bind without it)
4. **P1** — Phase 82.11 agent event firehose (agent-creator UI can't reconstruct conversations without it)
5. **P1** — Phase 82.13 agent processing pause + takeover (operator intervention blocks production use)
6. **P1** — Phase 83.15 microapp testing harness (every author needs it; lands DX value early)
7. **P1** — Phase 83.16 microapp error → operator path (operational visibility)
8. **P1** — Phase 83.17 microapp config schema validation (install-time fail-fast)
9. **P1** — Phase 83.5 compliance primitives (gates 83.8 ventas-etb + serves agent-creator)
10. **P1** — Phase 83.6 microapp contract document (gates Python/TS authors using 83.7-Rust as porting source)
11. **P1** — Phase 83.10 second microapp validation (agent-creator production validation)
12. **P1** — Phase 83.12 → 83.13 → 83.14 React UI scaffold + component library + SDK publish (agent-creator UI shell)
13. **P2** — Phase 85.1 reactive 413 recovery (defensive, always-on benefit, ~1 d)
14. **P2** — Phase 86.1 local memory-shape metrics (ops observability, ~1 d)
15. ✅ ~~Phase 81.5 PluginRegistry::discover~~ (shipped 2026-05-02 library + tests; boot wire + CLI deferred to 81.6)
16. **P2** — Phase 81.9 Mode::Run registry sweep (~500 → 30 LOC critical milestone)
17. **P2** — Phase 81.3 / 81.4 / 81.6 / 81.7 / 81.8 (plug-and-play remainder; order after 81.5/9)
18. **P3** — Phase 85.2 cache-aware micro-compaction (multi-tenant cost optimization, ~3-4 d)
19. **P3** — Phase 87.1 LlmJudgeEvaluator (depends on Phase 84 fully shipped)
20. **P3** — Phase 81.10 / 81.11 / 81.12 (plugin hot-load / doctor / migration — after 81.5/9 land)

Anything not in this list is either ✅ shipped, **DEFER**, or
**DROPPED** — see sections below.

---

## ACTIVE — what we will ship next

Order: priority within each phase × phase numerical order.

### ⭐ Phase 84 — Coordinator agent persona + worker continuation   `P0`

**Status**: brainstorm + spec + plan all approved in
conversation. Next action: `/forge ejecutar 84.1`.

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 84.1 — Coordinator persona system prompt | **P0 NEXT** | ⬜ ready | 1.5 d |
| 84.2 — `<task-notification>` envelope | **P0** | ⬜ | 1 d |
| 84.3 — `SendMessageToWorker` continuation tool | **P0** | ⬜ | 2 d |
| 84.4 — Worker persona system prompt | **P0** | ⬜ | 1 d |
| 84.5 — Docs + admin-ui sync | **P0** | ⬜ | 0.5 d |

**Total**: ~6 dev-days. Critical path 84.1 → 84.2 → 84.3.

---

### Phase 83 — Microapp framework foundation (active for the agent-creator critical path)

The agent-creator microapp at `/home/familia/chat/agent-creator-microapp/`
drives this phase. Critical-path rows flagged `P1`.

| Sub-phase | Priority | Status | Notes |
|-----------|----------|--------|-------|
| 83.1 — Per-agent extension config propagation | **P2** | ⬜ | Microapp wants per-agent config maps; not yet a hard blocker |
| 81.6 — Plugin-side agent registration (library + tests) | **P2** | ✅ shipped 2026-05-02 (merge + init loop + report extension; boot wire + CLI deferred follow-up) |
| 81.7 — Plugin-side `skills_dir` contribution (library + tests) | **P2** | ✅ shipped 2026-05-03 (merge + SkillLoader::with_plugin_roots + report extension; boot wire deferred bundle) |
| 81.8 — `ChannelAdapter` trait + registry (library + tests) | **P2** | ✅ shipped 2026-05-03 (trait + registry + PluginInitContext extension + diagnostic variant; boot wire deferred bundle) |
| 81.9 — `wire_plugin_registry` boot helper + boot wire integration | **P2** | ✅ shipped 2026-05-03 (helper + LlmAgentBehavior.plugin_skill_roots + main.rs replaces 81.5.b block) |
| 81.9.b — `nexo agent doctor plugins` CLI subcommand | **P2** | ✅ shipped 2026-05-03 (Mode::DoctorPlugins variant + parser arm + run_doctor_plugins handler + 8-section TTY/JSON render) |
| 81.10 — Plugin hot-load via Phase 18 reload coord | **P3** | ✅ shipped 2026-05-03 (register_plugin_registry_reload_hook helper + boot wire + 3 unit tests; skill_roots rebuild + live discovery_cfg deferred 81.10.b) |
| 81.11 — Plugin doctor + capability inventory integration | **P3** | ✅ shipped 2026-05-03 (capability_aggregator + 3 new diagnostic variants + report extension + wire_plugin_registry signature; doctor_render sections + DoctorCapabilities envelope mode deferred 81.11.b) |
| 81.12.0 — `PluginFactoryRegistry` foundation (no plugin migrations) | **P3** | ✅ shipped 2026-05-03 (factory module + run_plugin_init_loop_with_factory + wire_plugin_registry 6th param) |
| 81.12.a — Browser plugin migration to NexoPlugin | **P3** | ✅ shipped 2026-05-03 (dual-trait + manifest + factory builder; dormant — main.rs untouched) |
| 81.12.b — Telegram plugin migration to NexoPlugin | **P3** | ✅ shipped 2026-05-01 (dual-trait + manifest + factory builder + 5 unit tests; multi-instance pattern verified — manifest.id stays "telegram", per-instance label lives in `registry_name`; dormant — main.rs untouched) |
| 81.12.c — WhatsApp plugin migration to NexoPlugin | **P3** | ✅ shipped 2026-05-01 (dual-trait + manifest + factory builder + 5 unit tests; multi-account pattern verified — manifest.id stays "whatsapp", per-instance label lives in `registry_name`, distinct session_dir per instance keeps Signal keys isolated; dormant — main.rs untouched; WhatsappPairingAdapter + register_whatsapp_tools out of scope) |
| 81.12.d — Email plugin migration to NexoPlugin | **P3** | ✅ shipped 2026-05-01 (dual-trait + manifest + factory builder + 4 unit tests; single-plugin / multi-account-internal model — `manifest().plugin.id` and legacy `name()` both `"email"` at all times, no per-instance divergence; 4-arg factory closes over cfg + creds + google + data_dir; PluginInitContext untouched; dormant — main.rs untouched) |
| 81.12.e — Remove legacy registration block from main.rs | **DEFER → SUPERSEDED-BY-81.17** | ⏸ — once 81.17 (`plugin-browser` extract to standalone repo) ships, the in-tree legacy block becomes obsolete naturally: out-of-tree plugins don't need `Arc<BrowserPlugin>` built from main.rs. Doing 81.12.e now is throwaway work — would require either bundled-manifest discovery search_paths or synthetic factory_registry injection (~1-2 d), and 81.17 deletes the block anyway. Kept as a marker so Phase 81 dual-trait migration (a/b/c/d ✅) reads as 12/13 with e absorbed by 81.17. |
| **81.14 — `SubprocessNexoPlugin` adapter (host-side spawn + stdio JSON-RPC bridge)** | **P3** | ✅ shipped 2026-05-01 (manifest `[plugin.entrypoint]` additive section + `SubprocessNexoPlugin` host-side adapter + `subprocess_plugin_factory` helper + 9 unit tests covering happy path of spawn/handshake plus error paths: missing command, env collision with reserved `NEXO_*`, initialize-reply timeout, manifest id mismatch, shutdown idempotency. JSON-RPC 2.0 newline-delimited over stdio mirrors `extensions/openai-whisper` shape. Broker → child topic bridge wired in 81.14.b. Existing 4 in-tree plugin manifests (browser/telegram/whatsapp/email) verified still parse with new optional `entrypoint` section.) |
| **81.14.b — Broker ↔ child topic bridge** | **P3** | ✅ shipped 2026-05-01 (4 new unit tests covering subscribe pattern derivation from `manifest.channels.register[].kind`, child publish forwarding to broker via `broker.publish` notification, allowlist rejection of publishes to topics outside `plugin.inbound.<kind>[.>]`, and bridge skipped when `broker = None`. Daemon subscribes both exact (`plugin.outbound.<kind>`) and wildcard (`plugin.outbound.<kind>.>`) topics for each declared channel kind — wildcard demands ≥1 trailing segment in the broker's matcher, so both are needed for single-instance + multi-instance coverage. `BridgeContext` struct captured by reader task via `tokio::sync::OnceCell` so the bridge activates only AFTER handshake validates manifest id, preventing the child from racing ahead of its inbound stream. Stdin-bound forwarder tasks use `try_send` (drop-on-full + warn) so a stalled child can't backpressure the daemon's broker. Validation: each child publish topic is matched against the allowlist via `nexo_broker::topic::topic_matches` — child trying to publish to `agent.route.system_critical` (or any non-inbound topic) gets dropped with warn-level log. Defense-in-depth core for community-tier plugins.) |
| **81.15.a — `nexo-microapp-sdk` plugin-mode (`PluginAdapter` child-side helper)** | **P3** | ✅ shipped 2026-05-01 (new `plugin` module behind `plugin` Cargo feature, gated deps on `nexo-plugin-manifest` + `nexo-broker` + `toml`. `PluginAdapter::new(manifest_toml)` parses + caches manifest at construction. Builder API: `.on_broker_event(handler)` + `.on_shutdown(handler)` + `.run_stdio()`. Child-side `BrokerSender` clone-cheap handle for emitting `broker.publish` notifications back to the daemon. Dispatch loop handles `initialize` (replies with cached manifest + server_version), `broker.event` notifications (calls user handler with `BrokerSender` for symmetric publish), `shutdown` request (invokes user handler, replies `{ok:true}`, breaks loop). Unknown methods → -32601, parse errors → -32700. 6 unit tests using `tokio::io::duplex` for stdin/stdout simulation cover all paths.) |
| **81.15.c — SDK child-side RPC helpers (`recall_memory` + `complete_llm`)** | **P3** | ✅ shipped 2026-05-01 (extends `BrokerSender` with `pending: Arc<DashMap>` + `next_id: AtomicU64` for child-side request-response correlation. New low-level `request(method, params, timeout)` + typed wrappers `recall_memory(agent_id, query, limit) -> Result<Vec<MemoryEntry>, RpcError>` and `complete_llm(LlmCompleteParams) -> Result<LlmCompleteResult, RpcError>`. New `RpcError` enum: `Server { code, message }`, `Timeout(Duration)`, `Transport(String)`, `Decode(String)`. New typed structs `LlmCompleteParams` + `LlmCompleteResult` + `TokenCount` exposed via re-exports. Dispatch loop extended to detect response frames (`id` + `result/error`, no `method`) and resolve pending oneshot. **Critical fix**: handler dispatch wrapped in `tokio::spawn` to prevent deadlock — without it, a handler calling `broker.request(...)` blocks the dispatch loop which is the only thing that can resolve the request's oneshot. SDK feature `plugin` adds `nexo-llm` + `nexo-memory` + `dashmap` deps (gated). 4 new unit tests using `tokio::io::duplex` cover round-trip, server error propagation, timeout, typed memory.recall wrapper. 10/10 SDK plugin tests pass.) |
| **81.15.c.b — SDK streaming consumption helper (`complete_llm_stream`)** | **P3** | ✅ shipped 2026-05-01 (pending value type changed to `PendingKind` enum: `Single(oneshot)` for non-streaming + `Streaming { delta_tx: mpsc::UnboundedSender<String>, final_tx: oneshot::Sender }` for streaming. Dispatch loop response path branches on kind: Single resolves oneshot directly, Streaming resolves final_tx with deserialized LlmCompleteResult (delta_tx drops with the enum, closing the chunks channel cleanly). Notification path adds `llm.complete.delta { request_id, chunk }` handling: looks up pending by id, pushes chunk to delta_tx if Streaming. New `BrokerSender::complete_llm_stream(params)` returns `LlmStream` handle with `next_chunk() -> Option<String>` + `await_final() -> Result<LlmCompleteResult, RpcError>` API. `LlmStream.finished` field is `Option<...>` so `await_final` can `take()` ownership despite the Drop impl. Drop impl removes pending entry on early-drop so late deltas/final reply land on missing entry → debug log. 1 new unit test asserts handler reassembles 3 deltas into "hello world" + observes final usage. 11/11 SDK plugin tests pass.) |
| **81.15.b — Rust plugin template (in-workspace draft)** | **P3** | ✅ shipped 2026-05-01 (`extensions/template-plugin-rust/` — Cargo.toml with `nexo-microapp-sdk = { features = ["plugin"] }` + `nexo-broker` path deps, `nexo-plugin.toml` declaring `[[plugin.channels.register]]` + `[plugin.entrypoint]`, `src/main.rs` ~70 LOC echo plugin using `PluginAdapter`, README with copy-rename-build workflow + topic conventions table + handshake smoke test cmd. Workspace member so CI keeps it green; operators copy out of the repo and swap path deps for crates.io versions. Smoke-tested handshake: `echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \| ./target/debug/template-plugin-rust` returns valid manifest reply. Phase 31.6 `nexo plugin new --lang rust` scaffolder will eventually publish this as the `github.com/nexo-rs/plugin-template-rust` external repo. Doubles as 81.17.c-validation: real Rust binary (not bash mock) proves the contract end-to-end against the production daemon path.) |
| **81.16 — `nexo-plugin-contract.md` versioned IPC spec** | **P3** | ✅ shipped 2026-05-01 (workspace root `nexo-plugin-contract.md` ~600 LOC, contract version 1.0.0. Sections: transport, manifest entrypoint, JSON-RPC envelope, lifecycle methods (`initialize`/`shutdown`), broker bridge notifications (`broker.event`/`broker.publish`), topic allowlist semantics, error code table, backpressure, code examples in Rust (using shipped `PluginAdapter`) + Python/TS skeletons for Phase 31.4/31.5, semver compat policy. Thin pointer at `docs/src/plugins/contract.md` plus SUMMARY.md entry; mdbook builds clean. Documents what 81.14/14.b/15.a already implements — single source of truth for cross-language SDK authoring.) |
| **81.17 — Auto-subprocess init-loop fallback (library + tests)** | **P3** | ✅ shipped 2026-05-01 (`run_plugin_init_loop_with_factory` extended with auto-subprocess path: when no in-tree factory is registered for a manifest's id AND the manifest has `[plugin.entrypoint] command = "..."`, the loop builds a `subprocess_plugin_factory(manifest)` inline and uses it. In-tree manifests without entrypoint keep recording `NoHandle` — back-compat with 81.12.a-d partial-migration shape. 3 unit tests added in `init_loop::tests` covering factory-build shape + the negative `NoHandle` case for non-subprocess manifests. Boot wire stays `None` from main.rs because activating it would route through the existing `unreachable!()` ctx_factory in `boot.rs` and panic on any subprocess manifest. **Boot-wire activation deferred to 81.17.b** which extends `wire_plugin_registry` to accept a real broker + shutdown token so it can build a minimal `PluginInitContext` for the subprocess path. End-to-end integration test against a mock script ships with 81.17.b too — needs the boot-wire change to be testable through the public API.) |
| **81.17.b — `wire_plugin_registry` broker/shutdown plumbing + boot wire activation + e2e integration test** | **P3** | ✅ shipped 2026-05-01 (new `SubprocessRuntime { broker, shutdown, config_dir, state_root }` + `wire_plugin_registry_with_runtime(...)` variant + `SubprocessCtxStubs` builds a real-enough `PluginInitContext` for the subprocess path. **Made `wire_plugin_registry` async** — the prior sync-with-`futures::executor::block_on` shape deadlocks tokio when subprocess plugins try to spawn children inside the blocked worker thread; updated all 5 call sites (main.rs ×2, 3 integration tests). **Init loop now retains `Arc<dyn NexoPlugin>` handles** — without retention, `kill_on_drop(true)` on the child Command triggers SIGKILL right after `init()` returns, killing the plugin before it can do any work. New return type `FactoryInitResult { outcomes, handles }` + new field `WirePluginRegistryOutput.plugin_handles`. main.rs activates the path: empty factory_registry + populated SubprocessRuntime → auto-subprocess fallback fires for any manifest with `[plugin.entrypoint] command`. New integration test `crates/core/tests/subprocess_plugin_e2e.rs` drops manifest + mock-plugin.sh in tempdir, asserts InitOutcome::Ok plus broker.publish round-trip from child to test subscriber via the bridge. 2/2 e2e tests pass. 5/5 init_loop tests pass.) |
| **81.17.c — Pilot: extract `plugin-browser` to standalone repo** | **P3** | ⬜ RENUMBERED (was 81.17) | ~3 d. `github.com/nexo-rs/plugin-browser` ships binary; daemon carga via discovery + auto-subprocess fallback. Hot-reload via 81.10 must keep working. Browser stub stays in-tree until 81.18-81.19 + cleanup. Depends on 81.17.b. |
| **81.18 — Extract `plugin-telegram` to standalone repo** | **P3** | ⬜ NEW | ~2 d. Multi-instance pattern probado en 81.12.b se mantiene — operator declara N manifests, daemon spawn N subprocess. |
| **81.19 — Extract `plugin-whatsapp` + `plugin-email` a standalone repos** | **P3** | ⬜ NEW | ~3 d. Email tiene multi-account interno (un solo subprocess maneja N cuentas) — requiere extender contract con per-account credential injection. |
| **81.20.a — Daemon-mediated RPC: `memory.recall`** | **P3** | ✅ shipped 2026-05-01 (host-side dispatch + handler + tests; main.rs threading deferred to 81.20.a.b). Reader detects frame with `id` AND `method` → incoming child request → routes to `handle_child_request`. Today only `memory.recall` is wired (`llm.complete` / `tool.dispatch` ship in 81.20.b/.c). Params validated (`agent_id`, `query` required strings; `limit` u64 default 10, capped at 1000). Errors: -32601 method not found, -32602 invalid params, -32603 memory not configured / backend error. Response shape `{ entries: [<MemoryEntry>] }` serializes the existing `nexo_memory::MemoryEntry`. `BridgeContext` extends with `memory: Option<Arc<LongTermMemory>>`; `SubprocessRuntime` extends with `long_term_memory`. 3 new unit tests: happy path with seeded entry, -32603 when memory None, -32602 on bad params. Wire format documented in contract v1.1.0. 19/19 subprocess + 2/2 e2e tests pass. |
| **81.20.a.b — main.rs threading `memory` → `SubprocessRuntime`** | **P3** | ✅ shipped 2026-05-01 (1 LOC change — turned out the daemon path's `let memory =` binding at main.rs:1731-1821 (Long-term memory section) is already in scope at the wire callsite. The `long_term_memory` reference I'd cited at line 10883 was inside `run_mcp_server` (a separate function). Replaced `long_term_memory: None` with `long_term_memory: memory.clone()`. Subprocess plugins now receive -32603 "memory not configured" only when the operator has explicitly disabled long-term memory in `memory.yaml` (vs always returning that error due to a daemon-side plumbing gap). 19/19 subprocess + 2/2 e2e tests still pass.) |
| **81.20.b — Daemon-mediated RPC: `llm.complete` (non-streaming MVP)** | **P3** | ✅ shipped 2026-05-01 (host-side handler library + 3 unit tests + wire spec at contract v1.2.0; runtime threading deferred to 81.20.b.b). New `LlmServices { registry, config }` bundle. `BridgeContext` extends with `llm: Option<LlmServices>`. `SubprocessRuntime` extends with `llm: Option<LlmServices>`. `handle_child_request` match adds `llm.complete`. Params validated — provider/model/messages required strings/array; messages parsed via serde from JSON-RPC params; max_tokens/temperature/system_prompt optional. Calls `LlmRegistry::build(&cfg, &model_cfg)` then `client.chat(req)`. Response shape `{ content, finish_reason, usage }` — text responses only; tool-call responses return -32601 not_implemented (deferred to a future contract bump that defines the tool_result re-submit shape). Errors: -32602 bad params, -32603 not configured / build failed / chat failed. main.rs llm_registry construction reordered to wrap in Arc earlier so it's clonable into SubprocessRuntime. 3 new unit tests: -32603 when llm None, -32602 on bad params (4 sub-cases), -32603 when provider unknown. 22/22 subprocess + 2/2 e2e tests pass. |
| **81.20.b.b — main.rs threads `LlmServices` into subprocess runtime** | **P3** | ✅ shipped 2026-05-01 (runtime threading half done; streaming deferred to 81.20.b.c). `PluginInitContext` extended with `llm_config: Arc<LlmConfig>` so SubprocessNexoPlugin::init builds `LlmServices { registry: ctx.llm_registry.clone(), config: ctx.llm_config.clone() }` inline and passes to spawn_and_handshake. `SubprocessRuntime.llm: Option<LlmServices>` replaced with two flat fields (`llm_registry: Arc<LlmRegistry>` + `llm_config: Arc<LlmConfig>`) — SubprocessCtxStubs.context_for now passes the runtime's REAL llm_registry instead of the stubs' empty one, so subprocess plugins issuing `llm.complete` reach operator-configured providers. SubprocessCtxStubs no longer carries its own llm_registry stub (ConfigReloadCoordinator gets rt.llm_registry too). main.rs threads `llm_registry.clone()` + `Arc::new(cfg.llm.clone())` through SubprocessRuntime. 22/22 subprocess + 2/2 e2e tests pass. |
| **81.20.b.c — Streaming via `llm.complete.delta` notifications** | **P3** | ✅ shipped 2026-05-01 (`params.stream = true` opt-in switches `handle_llm_complete` from `client.chat` buffered path to `client.stream` streaming path. Each `StreamChunk::TextDelta` becomes a `llm.complete.delta { request_id, chunk }` notification via stdin_tx. `Usage` chunk + final `End { finish_reason }` reassembled into the response. `handle_child_request` extended with `stdin_tx: &mpsc::Sender<Value>` + `request_id: &Value` parameters threaded from reader. Tool-call deltas during streaming are dropped (same scope as non-streaming MVP); pure-tool-call streams return -32601. Final reply matches original `id` but omits `content` field — child reassembled from deltas. Wire docs at contract v1.3.0. 22/22 subprocess tests pass — 6 existing handle_llm_complete callsites updated for new 5-arg signature. SDK-side child-side `BrokerSender::stream_chunks` helper deferred to 81.15.c.) |
| **81.20.c — Daemon-mediated RPC: `tool.dispatch`** | **P3** | ⏸ DEFERRED | Original ~1d estimate was wrong: ToolHandler::call requires a full `AgentContext` (~25 fields, per-running-agent state). Architectural prereq: a new `AgentContextRegistry` that main.rs populates per-spawn + SubprocessRuntime accesses for lookup. ~2-3 d. Defer until path A (proper architecture) is needed. Path B (stub AgentContext with only broker/sessions) is hacky — most tools fail accessing None fields. Honest scoping: 81.20.c needs more than 81.20.a/b cousins. |
| **81.21 — Plugin supervisor: crash detection + broker event (MVP)** | **P3** | ✅ shipped 2026-05-01 (MVP scope: detection + emission only — auto-respawn + resource limits deferred to 81.21.b/.c. Inner.child wraps in `Arc<Mutex<Option<Child>>>` so supervisor task can `try_wait()` every 500ms while shutdown still `take()`s for reaping. New supervisor task spawned alongside writer/stdout-reader/stderr-reader: polls exit status, on detected exit publishes `plugin.lifecycle.<id>.crashed` event with `{ plugin_id, exit_code }` payload + `source = "plugin.supervisor"` (when broker is wired) at warn level, then cancels the plugin's tasks via `cancel.cancel()` to teardown bridge tasks. Helper `kill_handle(&Arc<Mutex<Option<Child>>>)` consolidates the kill-on-error sites in spawn_and_handshake. shutdown() locks the mutex + reaps idempotent with supervisor (whichever observes the child first wins, the other sees None). 1 new unit test `supervisor_publishes_crashed_event_on_child_exit` drops a mock that exits with code 7 after 200ms post-handshake; subscriber on `plugin.lifecycle.test_plugin.crashed` receives the event within 2s with exit_code=7. 3 existing task-count assertions bumped (+1 for supervisor task). 15/15 subprocess tests + 2/2 e2e tests pass.) |
| **81.21.b — Plugin supervisor: stderr tail capture + manifest config** | **P3** | ✅ shipped 2026-05-01 (manifest gains `[plugin.supervisor]` section: `respawn: bool`, `max_attempts: u32`, `backoff_ms: u64`, `stderr_tail_lines: usize` — all defaults so existing manifests parse unchanged. Validation rejects `stderr_tail_lines > 512` (`SUPERVISOR_STDERR_TAIL_MAX`) via new `ManifestError::SupervisorStderrTailExceedsCap`. Stderr reader populates a `VecDeque<String>` ring buffer capped at the manifest's value (drops oldest when full, no append when 0 = disabled). Supervisor on crash drains buffer into the `stderr_tail: [String]` field of the crashed event payload — operators see the LAST N stderr lines without grepping daemon logs. `respawn: true` parses + validates but only logs a one-shot reminder that the actual loop ships in 81.21.b.b (operator must restart daemon to recover). 17/17 subprocess tests pass: existing `supervisor_publishes_crashed_event_on_child_exit` extended to assert 3 diag lines round-trip into payload; new `manifest_validate_rejects_stderr_tail_above_cap` enforces the cap. All 4 in-tree plugin manifests still parse cleanly.) |
| **81.21.b.b — Plugin supervisor: auto-respawn loop** | **P3** | ⬜ DEFERRED | ~2-3 d. The actual respawn behavior — when crash detected AND `manifest.supervisor.respawn = true`, supervisor cancels current bridge tasks + spawns a fresh child + redoes handshake + redoes bridge wiring with exponential backoff up to `max_attempts`. Requires either a higher-level supervisor task that owns SubprocessNexoPlugin lifecycle OR Inner refactor to be partially-replaceable (current `Mutex<Option<Inner>>` is single-shot owned by the adapter). 81.21.b ships the manifest fields so operators can declare intent today; 81.21.b.b wires them. |
| **81.21.c — Plugin resource limits: CPU/mem via cgroup/rlimit** | **P3** | ⬜ DEFERRED | ~3 d. OS-divergent: linux cgroup v2 + rlimit, macOS sandbox-exec resource caps, fallback to monitoring on others. Manifest knobs: `limits.cpu_pct` / `limits.mem_mb` / `limits.startup_timeout_ms`. Required to gate community-tier plugins. |
| **81.22 — Plugin sandbox: network + filesystem allowlist per-plugin via manifest** | **P3** | ⬜ NEW | ~2 d. Gates community tier — untrusted code. Linux: namespaces / seccomp / nftables. macOS: sandbox-exec profile. Manifest declara `sandbox.network.hosts` + `sandbox.fs.read_paths` + `sandbox.fs.write_paths`. |
| **81.23 — Plugin stdio → daemon tracing bridge** | **P3** | ✅ shipped 2026-05-01 (subprocess.rs flips `stderr(Stdio::null())` → `stderr(Stdio::piped())` + new stderr reader task forwards each line as `tracing::info!(target: "plugin.stderr", plugin_id = %id, line = %trimmed)`. Stdout reader's "non-JSON line" path downgraded from `tracing::warn!` (drop frame) to `tracing::info!(target: "plugin.stdout", plugin_id, line)` — child debug output via `eprintln!` / `tracing` no longer disappears, child mixing stderr+stdout for diagnostics gets the same structured visibility. Stderr reader spawned BEFORE handshake send so child boot-time errors land in operator logs. Joined on shutdown via Inner.tasks. 1 new unit test `stderr_is_piped_so_reader_can_construct` + 2 existing task-count assertions updated to account for the new reader task. 14/14 subprocess unit tests + 2/2 e2e tests pass. Operators filter via `RUST_LOG=plugin.stderr=info` or per-plugin via the `plugin_id` field. Structured field extraction from tracing-subscriber JSON output deferred to follow-up 81.23.b.) |
| **81.24 — Remote `ChannelAdapter` wrapper (subprocess-backed)** | **P3** | ⬜ NEW | ~2 d. Permite plugins out-of-tree contribuir **canales nuevos** (Slack, Discord, SMS, Matrix, etc.) registrándose en el `ChannelAdapterRegistry` ya shippeado en 81.8. Daemon translation: trait calls ↔ stdio frames. |
| **81.25 — Remote `LlmClient` provider wrapper** | **P3** | ⬜ NEW | ~2 d. Plugin expone provider LLM custom (Cohere, Mistral, Together, Ollama, llama.cpp local). Daemon registra en `LlmClientRegistry` con CircuitBreaker auto-wrapped + cost tracking integrado. |
| **81.26 — Remote memory backend wrapper (short/long/vector)** | **P3** | ⬜ NEW | ~3 d. Plugin expone storage alternativo (Pinecone, Qdrant, Weaviate, Postgres pgvector). Daemon mete en MemoryStore registry. Config selecciona qué backend usa cada agent. |
| **81.27 — Remote `HookInterceptor` wrapper** | **P3** | ⬜ NEW | ~2 d. Plugin community-tier puede ejecutar compliance/PII-redact/rate-limit checks. Vote-to-block via Phase 83.3 hook protocol. Daemon enforce; plugin solo decide. |
| **81.28 — Manifest `[extends]` section per-registry capability declaration** | **P3** | ⬜ NEW | ~1 d. `[extends.channels] = ["slack"]` / `[extends.llm_providers] = ["cohere"]` / `[extends.memory_backends] = ["pinecone"]` / `[extends.hooks] = ["pii_redact"]`. Daemon usa esto para saber qué registries poblar al subir el plugin. Capability negotiation at handshake. |
| 83.2 — Extension-contributed skills | **P2** | ⬜ | Microapp ships its own skills; opportunistic |
| 83.3 — Hook interceptor (vote-to-block) | **P1** | ⬜ | Compliance primitives plug in here — gates 83.5 + 83.8 |
| 83.4 — `microapp-sdk-rust` reusable helper | **P1** | 🔄 | Core SDK ✅ 2026-04-30; 83.4.b ✅; 83.4.c Phase 82.x helpers pending |
| 83.5 — `compliance-primitives` reusable library | **P1** | ⬜ | Anti-loop / anti-manipulation / opt-out / PII redact / rate-limit / consent. Provider-agnostic, microapp-foundational |
| 83.6 — Microapp contract document | **P1** | ⬜ | Language-agnostic spec — replaces Python/TS reference templates as the portability story |
| 83.7 — Microapp template (Rust only) | **P2** | ⬜ | **Reduced** from 3 stacks to Rust only. Other stacks port from 83.6 contract |
| 83.8 — `ventas-etb` reference microapp | **P2** | ⬜ | First production microapp built on the framework |
| 83.9 — `ana` cutover | **P3** | ⬜ | Migration from yaml-only to extension-based; depends on 83.8 |
| 83.10 — Second microapp validation | **P1** | ⬜ | agent-creator production validation — proves framework reusability |
| 83.11 — Docs + admin-ui sync | **P3** | ⬜ | Final docs sweep |
| 83.12 — Meta-microapp React UI scaffold | **P1** | ⬜ | agent-creator UI shell |
| 83.13 — `microapp-ui-react` component library | **P1** | ⬜ | WhatsApp-inspired chat helper for microapps that need it |
| 83.14 — Publish SDKs (crates.io + npm) | **P1** | ⬜ | Decouples agent-creator from nexo source |
| **83.15 — Microapp testing harness (mock daemon)** | **P1** | ⬜ NEW | Closes a foundational DX gap — every author re-invents mocks today |
| **83.16 — Microapp error → operator path** | **P1** | ⬜ NEW | Operator visibility into microapp boot/handler failures |
| **83.17 — Microapp config schema validation** | **P1** | ⬜ NEW | Shifts validation to install/boot time so misconfig fails fast |

**3 new gap-closing sub-phases added in this curation pass**
(83.15 / 83.16 / 83.17). They were missing from the original
plan — every microapp author would have hit them.

---

### Phase 82 — Multi-tenant SaaS extension enablement

Critical path for agent-creator: **82.11 / 82.12 / 82.13** all
flagged `P1`. Without these the agent-creator UI cannot stream
transcripts, host its HTTP server, or pause agents.

| Sub-phase | Priority | Status |
|-----------|----------|--------|
| 82.1 — `BindingContext` enrichment | — | ✅ |
| 82.2 — Tool registry + manifest parsing | — | ✅ |
| 82.3 — Plugin.toml [outbound_bindings] schema | — | ✅ |
| 82.4 / 82.5 / 82.7 / 82.10 | — | ✅ |
| 82.6 — Per-extension state_root convention | **P2** | ⬜ |
| 82.8 — Multi-tenant audit log filter | **P2** | ⬜ |
| 82.9 — Reference SaaS template | **P3** | ⬜ |
| 82.11 — Agent event firehose + transcripts | **P1** | ⬜ |
| 82.12 — HTTP server hosting | **P1** | ⬜ |
| 82.13 — Agent processing pause + takeover | **P1** | ⬜ |
| 82.14 — `escalate_to_human` tool + notification | **P2** | ⬜ |

---

### Phase 81 — Plug-and-Play Plugin System

| Sub-phase | Priority | Status |
|-----------|----------|--------|
| 81.1 / 81.2 | — | ✅ |
| 81.3 — Tool namespace runtime enforcement | **P2** | ⬜ |
| 81.4 — Plugin-scoped config dir loader | **P2** | ⬜ |
| 81.5 — `PluginRegistry::discover` filesystem walk | **P2** | ✅ shipped 2026-05-02 (library + tests; boot wire + CLI deferred to 81.6) |
| 81.6 — Plugin-side agent registration | **P3** | ⬜ |
| 81.7 — Plugin-side `skills_dir` | **P3** | ⬜ |
| 81.8 — `ChannelAdapter` trait | **P3** | ⬜ |
| 81.9 — `Mode::Run` registry sweep | **P2** | ⬜ critical milestone (~500 → 30 LOC) |
| 81.10 — Plugin hot-load via reload coord | **P3** | ⬜ |
| 81.11 — Plugin doctor + capability inventory | **P3** | ⬜ |
| 81.12 — Existing plugin migration | **P3** | ⬜ |
| 81.13 — Reference plugin template + CLI | **DROPPED → folded into 31.6** | — Replaced by Phase 31.6 multi-lang scaffolder once subprocess infra (81.14-81.23) closes. |

---

### Phase 85 — Compaction hardening

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 85.1 — Reactive 413 recovery | **P2** | ⬜ | ~1 d |
| 85.2 — Cache-aware micro-compaction | **P3** | ⬜ | ~3-4 d |

---

### Phase 86 — Memory observability

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 86.1 — Local memory-shape Prometheus metrics | **P2** | ⬜ | ~1 d |

---

### Phase 87 — LLM-as-judge verifier

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 87.1 — `LlmJudgeEvaluator` impl | **P3** | ⬜ AFTER-PHASE-84 | ~2 d |

---

### Phase 31 — Plugin marketplace + multi-language authoring   `P3`

Promoted from `PHASES.md` legacy backlog 2026-05-01. Activates only
after Phase 81 subprocess infra (81.14-81.23) closes.
**Replaces** the old 81.13 `nexo plugin new` defer (folded into 31.6).

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 31.0 — Registry index format spec + types crate | **P3** | ✅ shipped 2026-05-01 (new `crates/ext-registry/` workspace member with types + parser + validation. `ExtRegistryIndex { schema_version, generated_at, entries }` + `ExtEntry` + `ExtDownload` + `ExtSigning` + `ExtTier` (Verified/Community). Validation enforces id regex, non-empty fields, HTTPS-only URLs, sha256 hex format (64 lowercase chars), non-empty downloads array, and `tier == verified` requires signing block. Schema version `1.0.0`. Bundled sample at `examples/sample-ext-index.json` (slack verified + discord community). 9 unit tests cover parse + validate happy path, id regex rejection, non-HTTPS URLs, bad sha256 (length + uppercase), missing signing for verified, empty downloads, unknown field rejection (deny_unknown_fields), bundled-sample sanity. Phase 31.1 (`nexo ext install`) consumes this types crate; 31.2 CI workflow generates entries; 31.3 cosign verification + trusted_keys.toml uses `signing` field. **Index repo bootstrap** (`github.com/nexo-rs/ext-registry`) — separate external repo, deferred to operator setup since it's a 1-commit init outside this workspace.) |
| 31.1 — Plugin installer library (decentralized GitHub Releases) | **P3** | ✅ shipped 2026-05-03 (Option B chosen — no central catalog, install CLI hits GitHub Releases API directly). New `crates/ext-installer/` workspace member. `PluginCoords::parse("owner/repo@tag")` (default tag = "latest"). `resolve_release()` fetches release JSON, finds `nexo-plugin.toml` asset to extract `plugin.id`, locates target tarball + matching `.sha256` asset. `download_and_verify()` reads expected sha256 from .sha256 asset, stream-downloads tarball + computes sha256 incrementally, rejects + cleans up on mismatch. `install_plugin()` one-shot helper. `current_target_triple()` detects the running daemon's target with NEXO_INSTALL_TARGET env override. 8 unit tests via wiremock simulating GitHub API: coords parsing (4 cases), URL construction, happy round-trip with real sha verify, missing manifest asset rejection, missing target tarball with available list, sha256 mismatch + cleanup. Cosign verification deferred to 31.3; tarball extract deferred to 31.1.b; CLI integration deferred to 31.1.c. |
| 31.1.b — Tarball extraction | **P3** | ✅ shipped 2026-05-03 (new `extract.rs` + `extract_error.rs` modules in `nexo-ext-installer`. `extract_verified_tarball(ExtractInput) -> ExtractedPlugin` lays the verified tarball into `<dest_root>/<id>-<version>/` via staging dir + atomic rename. Per-entry path validation rejects `..`/absolute/Windows-prefix/NUL bytes (mining `research/src/infra/archive.ts` preflight pattern). Entry-type whitelist allows only `Regular` + `Directory`; symlinks/hardlinks/char/block/fifo/GNU long-name/etc. rejected with `DisallowedEntryType`. Size budgets via `ExtractLimits` (`max_tarball_bytes 100 MB`, `max_entries 10_000`, `max_extracted_bytes 250 MB`, `max_entry_bytes 100 MB`). Manifest re-parsed from staging + validated against `expected.id`/`version` (catches tampered tarballs even when sha matched). `bin/<id>` existence verified + chmod 0o755 on Unix. Idempotent re-install: existing `<id>-<version>/` dir with matching manifest short-circuits with `was_already_present: true`. Stale `.staging-*` cleanup at start of every call (crash-recovery). Sync extract under `tokio::task::spawn_blocking`. New `ExtractError` enum with 11 variants. 13 new tests (5 helper-level + 8 public-API including raw-header path-traversal and absolute-path injection that bypass `tar::Builder::set_path` normalization). 21/21 installer tests pass.) |
| 31.1.c — `Mode::PluginInstall` CLI integration | **P3** | ✅ shipped 2026-05-03 (new `src/plugin_install.rs` module + `Mode::PluginInstall` / `Mode::PluginHelp` variants + `[cmd, sub, coords] if cmd == "plugin" && sub == "install"` parse arm. `run_plugin_install` orchestrates: load AppConfig → resolve target (`--target` flag → `NEXO_INSTALL_TARGET` env → autodetect) → resolve dest_root (`--dest` flag → `cfg.plugins.discovery.search_paths[0]` → `nexo_state_dir().join("plugins")` fallback with stderr warn) → reqwest client with optional `NEXO_GITHUB_TOKEN` Bearer + GitHub UA + Accept headers → `PluginCoords::parse` → `resolve_release` → `download_and_verify` → `extract_verified_tarball` → cleanup cached tarball → best-effort `plugin.lifecycle.<id>.installed` broker emit (NATS only, 2s connect timeout, non-fatal failure). Human output: 6-line progress (resolving / resolved with size+sha trunc / downloading / verified / extracting / installed / event emitted). JSON output: `PluginInstallReport { ok, id, version, target, plugin_dir, binary_path, sha256, size_bytes, was_already_present, lifecycle_event_emitted }` single-line. Error path: `PluginInstallErrorReport { ok: false, kind, error, available }` with `kind` mapping the 7 InstallError variants + 11 ExtractError variants to stable strings. Hint blocks for `TargetNotFound` (lists available targets) + GitHub rate-limit (suggests `NEXO_GITHUB_TOKEN`) + 404 (verify coords). 8 unit tests cover error-kind exhaustive mapping (InstallError + ExtractError), report JSON shape, error-report `available` skip-on-none, human-bytes formatting, sha truncation. Workspace builds clean. Ext-installer regression: 21/21 still pass.) |
| 31.2 — Per-plugin CI publish workflow template | **P3** | ✅ shipped 2026-05-03 (new `extensions/template-plugin-rust/.github/workflows/release.yml` (~210 LOC) + `scripts/extract-plugin-meta.sh` + `scripts/pack-tarball.sh`. Tag-driven trigger on `v*`. Four jobs: validate-tag (regex + tag-vs-manifest version match), build (matrix: linux musl x86_64/aarch64 via cargo-zigbuild + macOS x86_64/aarch64 via direct cargo), sign (optional via repo variable `COSIGN_ENABLED == "true"`, keyless cosign sign-blob producing .sig/.pem/.bundle per asset), release (idempotent `gh release create` + `gh release upload --clobber` of all tarballs + sha256 sidecars + signing material + `nexo-plugin.toml`). Asset convention `<id>-<version>-<target>.tar.gz` matches what 31.1 resolver expects + 31.1.b extractor consumes (bin/<id> + nexo-plugin.toml at root, no wrapping dir). Concurrency group keyed on tag prevents duplicate publish. Cosign auto-PR to ext-registry dropped — no central registry under Option B. Template binary renamed `template_plugin_rust` (underscores) to match `plugin.id` (convention: cargo `[[bin]] name == [plugin] id`). New end-to-end Rust integration test `tests/pack_tarball.rs` builds a synthetic binary, runs `pack-tarball.sh`, re-extracts the tarball, asserts canonical layout (bin path + manifest at root, no wrapping) + sha256 sidecar valid + binary 0o755. README + new docs page `docs/src/plugins/publishing.md` document the convention + cosign opt-in + troubleshooting. mdbook builds clean.) |
| 31.3 — Trusted keys config + verified/community tier policy | **P3** | ✅ shipped 2026-05-03 (new `crates/ext-installer/src/{trusted_keys.rs, verify.rs, verify_error.rs}` modules + `<config_dir>/extensions/trusted_keys.toml` config + sample at `config/extensions/trusted_keys.toml.example`. Three trust modes: `ignore` (skip verification), `warn` (verify when `.sig` + `.cert` present, warn-and-proceed when absent — DEFAULT), `require` (reject any unsigned/mismatched install). Per-author `[[authors]]` table binds repo owner → cosign cert SAN regex (`identity_regexp`) + OIDC issuer (defaults to GitHub Actions issuer). Per-author `mode` overrides global default. CLI flags `--require-signature` / `--skip-signature-verify` (mutually exclusive — `FlagsConflict` error at parse time). `cosign verify-blob` shell-out via `tokio::process::Command` with `--signature/--certificate/--certificate-identity-regexp/--certificate-oidc-issuer` + optional `--bundle` (offline Rekor proof when present). `discover_cosign_binary` walks override → $PATH → /usr/local/bin / /opt/homebrew/bin / /usr/bin / ~/go/bin fallbacks; `CosignNotFound { searched }` error lists every probed path. New `VerifyError` enum (7 variants). `PluginInstallReport` extended with `signature_verified` + `signature_identity` + `signature_issuer` + `trust_mode` + `trust_policy_matched` (3 optional, skip-on-none). Verify hook lands between `download_and_verify` (sha256) and `extract_verified_tarball` (extraction). Cleans cached `.sig` / `.cert` (and `.bundle` when present) post-success along with the cached tarball. Hint blocks: `CosignNotFound` → install instructions; `PolicyRequiresSig` → relax mode or ask author to sign; `CosignFailed` → check identity_regexp. New docs page `docs/src/ops/plugin-trust.md` documents trust modes + identity_regexp shape + cosign install + JSON schema + troubleshooting. Template README addendum shows authors what their operators need in `[[authors]]`. 14 new tests: 9 in `trusted_keys.rs::tests` (parse minimal, parse global+authors, reject unknown fields, resolve global default, resolve author override, resolve inherits global when mode unset, load default when missing file, reject invalid regex, reject invalid mode, `as_str` canonical strings) + 6 in `verify.rs::tests` (discover override, discover not found, verify argv shape, verify with bundle, verify without bundle, verify failure propagation, verify io error on unrunnable bin) + 4 in `plugin_install::tests` (`verify_error_kind_maps_all_variants`, `report_includes_signature_fields_when_verified`, `report_omits_signature_identity_when_unverified`, `error_report_serializes_policy_requires_sig`). Ext-installer 21→38 tests; plugin_install 8→12 tests. Workspace builds clean. mdbook clean. NO env knob v1; NO sigstore-rs (defer); NO per-plugin override beyond per-owner; NO TUF / GPG / threshold sigs.) |
| 31.4 — Python SDK (`nexo-plugin-sdk` Python) + template + `noarch` resolver fallback | **P3** | ✅ shipped 2026-05-03 (new `extensions/sdk-python/` Python package: `nexo_plugin_sdk` with `PluginAdapter` + `BrokerSender` + `Event` + `read_manifest` + 3 exception types. Stdlib only (`tomllib` 3.11+ with `tomli` fallback shim). 6 tests using stdlib `unittest` covering handshake, broker.event dispatch, non-blocking reader proof, shutdown lifecycle with in-flight task drain, unknown-method `-32601` reply, manifest validation. New `extensions/template-plugin-python/` plugin template: `nexo-plugin.toml` (entrypoint command points at bash launcher), `src/main.py` (~70 LOC echo handler), `requirements.txt`, `scripts/extract-plugin-meta.sh` (copied from rust template), `scripts/pack-tarball-python.sh` (vendors SDK + deps + plugin source into `lib/`, builds bash launcher in `bin/<id>`, packs `<id>-<version>-noarch.tar.gz` + sha256 sidecar with self-test), `scripts/verify-pure-python.sh` (audits vendor dir for native ext suffixes), `tests/test_pack_tarball.py` (synthetic-SDK end-to-end pack assertion mirroring the Rust template's test), `.github/workflows/release.yml` (Phase 31.2-shaped 4-job workflow with single `noarch` matrix entry + `actions/setup-python@v5` + post-pack pure-python audit gate). Resolver in `crates/ext-installer/src/lib.rs` extended: `resolve_release` falls back to `<id>-<version>-noarch.tar.gz` when no per-target tarball matches; per-target preferred when both present. 2 new resolver tests; ext-installer 38→40. Daemon spawn pipeline UNCHANGED — language-agnostic by design. New docs page `docs/src/plugins/python-sdk.md`; SUMMARY wired. Cross-link in Rust template README. NO `[runtime]` block needed in manifest schema; NO daemon-side changes; NO embedded interpreter (PyO3/Pyodide); NO PyPI publish (defer until 31.5 stabilizes API); NO native-ext per-target Python tarballs (defer 31.4.b).) |
| 31.5.c — PHP SDK (`nexo/plugin-sdk` Composer) + template (Fibers, robusto) | **P3** | ✅ shipped 2026-05-04 (new `extensions/sdk-php/` Composer package with PSR-4 namespace `Nexo\Plugin\Sdk\` + SPDX `MIT OR Apache-2.0` license + `version: 0.1.0` for path-repo resolution. Public API: `PluginAdapter`, `BrokerSender`, `Event`, `Manifest`, `Wire`, `Scheduler`, `StdoutGuard`, `PluginError`/`ManifestError`/`WireError` (one class per file per PSR-4). Single runtime dep `yosymfony/toml: ^1.0` (pure-PHP TOML parser). PHP `^8.1` minimum (Fibers required). Robustness defaults all default-on: `enableStdoutGuard:true` (`ob_start` callback diverts non-JSON `echo`/`print`/`printf`/`var_dump` to stderr tagged `[stdout-guard]`; documented limitation: `fwrite(STDOUT, ...)` direct writes BYPASS — SDK's BrokerSender uses this deliberately for blessed JSON frames), `maxFrameBytes:1048576` (rejects oversized frames with WireError), `handleProcessSignals:true` (`pcntl_async_signals` for SIGTERM/SIGINT → graceful shutdown), in-flight Fiber drain on shutdown via `Scheduler::drain()`. Single-shot `run()` throws PluginError on second call. Fiber-based `Scheduler` runs each `broker.event` handler as cooperative coroutine; reader continues `stream_select`-polling stdin while handlers `Fiber::suspend()` between operations. 14 test cases across 7 test files using stdlib `proc_open`-based runner (no PHPUnit dep): handshake (3), manifest validation (3), dispatch (3 incl. slow-handler proof + drain), stdout-guard (2), wire (1), lifecycle (1), event (1). New `extensions/template-plugin-php/` plugin template: `nexo-plugin.toml` + `src/main.php` echo handler + `composer.json` declaring SDK via path repository (`url: ../sdk-php`, `symlink: false`) + `composer.lock` checked in (reproducibility) + `scripts/{extract-plugin-meta.sh, pack-tarball-php.sh, verify-pure-php.sh}` + `tests/test_pack_tarball.php` end-to-end synthetic-vendor assertion + `.github/workflows/release.yml` (4-job workflow: validate-tag + build with `shivammathur/setup-php@v2` + `composer:v2` + `composer validate --strict` + `composer install --no-dev --optimize-autoloader --classmap-authoritative` + pack + pure-PHP audit, optional sign, release). Bash launcher uses `php -d display_errors=stderr -d log_errors=0` so PHP errors land on stderr (defense-in-depth with the stdout guard). Real handshake smoke verified: `echo '{...,"method":"initialize"}' \| php src/main.php` returns valid JSON-RPC manifest reply. New docs page `docs/src/plugins/php-sdk.md`; SUMMARY wired. Cross-links in Rust + Python + TS template READMEs. Resolver `noarch` fallback (Phase 31.4) reused unchanged; daemon spawn pipeline UNCHANGED. NO PHPUnit (stdlib runner); NO ReactPHP/Amp (Fibers + minimal scheduler); NO embedded interpreter; NO Packagist publish (defer); NO native PHP ext in noarch (defer 31.5.c.b); NO PHP 7.4/8.0 (Fibers 8.1+).) |
| 31.5 — TypeScript SDK (`nexo-plugin-sdk` TS) + template (robusto) | **P3** | ✅ shipped 2026-05-04 (new `extensions/sdk-typescript/` ESM package with strict tsconfig (Node16 module resolution, noUncheckedIndexedAccess, isolatedModules). Public API: `PluginAdapter` + `BrokerSender` + `Event` + `parseManifest` + `installStdoutGuard` + `STDOUT_GUARD_MARKER` + 3 exception classes (`PluginError`/`ManifestError`/`WireError`) + JSON-RPC frame helpers (`buildResponse`/`buildErrorResponse`/`MAX_FRAME_BYTES`). Single runtime dep `smol-toml@^1.4.1` (~5 KB pure-JS TOML parser). Robustness defaults: `enableStdoutGuard:true` (patches `process.stdout.write` to divert non-JSON lines to stderr tagged `[stdout-guard]`), `maxFrameBytes:1<<20` (rejects oversized inbound frames with WireError), `handleProcessSignals:true` (SIGTERM/SIGINT trigger graceful shutdown), in-flight task drain on shutdown via `Promise.allSettled([...inflight])`. Single-shot `run()` (throws PluginError on second call). 13 stdlib `node:test` tests covering: handshake (initialize reply, unknown method `-32601`, unknown notification silently ignored), manifest validation (missing id, invalid TOML, id regex violation), dispatch (handler invocation, non-blocking reader, in-flight drain), stdout guard (idempotent install, console.log diverted to stderr), wire (oversized frame rejected with continued dispatch), lifecycle (double `run()` rejects). Spawn-driven test fixtures (`tests/fixtures/{echo,slow,console-log,lifecycle}-plugin.mjs`). New `extensions/template-plugin-typescript/` plugin template: `nexo-plugin.toml` + `src/main.ts` (~60 LOC echo handler) + `tsconfig.json` (strict) + `package.json` declaring SDK as `file:../sdk-typescript` + `scripts/{extract-plugin-meta.sh, pack-tarball-typescript.sh, verify-pure-js.sh}` + `tests/pack-tarball.test.mjs` (synthetic-SDK end-to-end pack assertion) + `.github/workflows/release.yml` (4-job workflow: validate-tag + build with `actions/setup-node@v4` + `npm ci` + `npm run typecheck` + `tsc` + `npm prune --omit=dev` + pack + pure-JS audit, optional sign, release). Pack script vendors compiled `dist/main.js` to `lib/plugin/`, SDK + scoped/unscoped npm deps to `lib/node_modules/`, ships bash launcher with `NODE_PATH=lib/node_modules` exec'ing `node lib/plugin/main.js`. New docs page `docs/src/plugins/typescript-sdk.md`; SUMMARY wired. Cross-links in Rust + Python template READMEs. Resolver `noarch` fallback (Phase 31.4) reused unchanged; daemon spawn pipeline UNCHANGED. NO CommonJS fallback (ESM-only); NO embedded TS compiler at runtime (pre-compiled at CI); NO Deno-specific entry; NO bundling (esbuild/tsup deferred); NO worker threads; NO npm publish (defer until 31.5.c PHP lands); NO native node addons in `noarch` tarballs (defer 31.5.b). PHP SDK explicitly deferred to 31.5.c per user direction.) |
| 31.6 — `nexo plugin new --lang <rust\|python\|typescript\|php>` scaffolder | **P3** | ✅ shipped 2026-05-04 (new `src/plugin_new.rs` module + `Mode::PluginNew` variant + parse arm. Templates embedded at compile time via `include_dir!` from the four `extensions/template-plugin-*/` directories — binary works after `cargo install` with no runtime FS dependency. Argv: `nexo plugin new <id> --lang <lang> [--dest <path>] [--owner <gh-handle>] [--description <text>] [--git] [--force] [--json]`. Validates `<id>` regex `^[a-z][a-z0-9_]{0,31}$` + `<lang>` ∈ `{rust, python, typescript, php}` before any IO. Substitution is literal byte-replace, longest-pattern-first (covers `template_plugin_<lang>` snake, `template-plugin-<lang>` kebab, `template_echo_<suffix>` channel kind, `Template Plugin (<Lang>)` title, boilerplate description, and the original author string). Text-extension whitelist prevents binary-asset corruption. `--owner alice` injects `alice <alice@users.noreply.github.com>` (privacy-preserving GitHub email). `--git` runs `git init --initial-branch=main` + `git add .` + `git commit -m "chore: scaffold ..."`; gracefully skips when `git` binary missing. `--force` removes existing dest before writing. Unix-only `chmod 0755` on `scripts/*.sh`. `next_steps_for(lang, id, owner)` emits language-specific commands. 11 unit tests: id-validation, lang-validation, title-case, placeholder ordering, scaffold-{rust,python,typescript,php} (4 tests, each verifies key files + manifest substitution + Cargo/package/composer renames), dest-exists-without-force fails, force-flag overwrites, owner-substitution lands. New runtime dep `include_dir = "0.7"`; workspace `regex` + `thiserror` added to root `[dependencies]`. Help text in `print_plugin_help` + `print_usage`. All 4 template READMEs replace the manual `cp -r` + `sed -i` quickstart pipeline with the new scaffolder command. **Replaces deferred 81.13**. ~280 LOC scaffolder, full Phase 31 author-side flow now closes end-to-end. NO interactive TUI wizard, NO custom template URL, NO `gh repo create` integration, NO Windows `chmod`, NO filesystem template lookup.) |
| 31.7 — Local dev loop: `nexo plugin run ./local-plugin` | **P3** | ✅ shipped 2026-05-04 (new `src/plugin_run.rs` module + `Mode::PluginRun` variant + parse arm `[cmd, sub, path] if cmd == "plugin" && sub == "run"`. Argv: `nexo plugin run <path-or-manifest> [--no-daemon-config] [--watch] [--verbose] [--json]`. Implementation pattern: pre-boot validation + side-channel `args.plugin_run_override` + **fall-through** to `Mode::Run` boot path (single source of truth — no duplicated daemon startup). `resolve_local_plugin` canonicalizes the path, branches on file (must end in `nexo-plugin.toml`) vs dir (must contain `nexo-plugin.toml`), parses manifest via `PluginManifest::from_str`, validates `[plugin.entrypoint] command` is non-empty. Errors: `PathNotFound`, `NotAPluginPath`, `ManifestInvalid`, `MissingEntrypoint`, `WatchDeferred` (--watch reserved for 31.7.b), `Io`. `apply_override(&mut cfg, &override)` injected right after `AppConfig::load` — prepends `plugin_root` to `cfg.plugins.discovery.search_paths` (idempotent: skips when already at head); when `--no-daemon-config`, clears `cfg.agents.agents` for standalone plugin inspection. `local > global` precedence — local plugin wins because discovery walker stops at first id-match in search_paths[0]. Reuses Phase 81.17.b subprocess auto-fallback for spawn (no daemon-side changes); reuses Phase 81.10 hot-reload (re-walks search_paths each tick → `cargo build` triggers respawn). 8 unit tests: 6 path-resolution + 2 search-path-prepend (resolves dir with manifest, resolves manifest directly, rejects non-existent, rejects dir without manifest, rejects invalid TOML, rejects missing entrypoint, prepend inserts at head, prepend idempotent for head-match). Tests use a private `apply_search_path_prepend` helper that operates on `Vec<PathBuf>` directly to avoid `AppConfig::default()` (no Default impl). New help text in `print_plugin_help` + main `print_usage`. CliArgs gained `plugin_run_override: Option<PluginRunOverride>` field; `parse_args` updated across all 9 construction sites. NO `--watch` (deferred 31.7.b), NO stdio-only no-broker mode (deferred 31.7.c), NO multi-plugin injection, NO custom port. Workspace builds clean; 40/40 ext-installer + 12/12 plugin_install + 11/11 plugin_new + 8/8 plugin_run.) |
| 31.8 — Operator UI: `nexo ext list` / `upgrade` / `remove` | **P3** | ⬜ NEW | ~2 d. CRUD operacional de plugins instalados. `upgrade` re-resuelve contra index respetando semver constraints. `remove` cleanup atómico. |
| 31.9 — Docs: plugin authoring guide per language + contract reference + signing how-to | **P3** | ⬜ NEW | ~2 d. `docs/src/plugin-authoring/{rust,python,typescript}.md` + `docs/src/plugin-authoring/contract-reference.md` (auto-generado del 81.16 spec) + `docs/src/plugin-authoring/signing-and-publishing.md`. |

**Total Phase 31**: ~20 dev-days. Critical path 31.0 → 31.1 → 31.2.
Lenguajes (31.4 + 31.5) son paralelos. 31.7 (local dev loop) es
el feature que hace la DX viable — sin él autores externos sufren
el round-trip publish-instalar-debug.

**Total roadmap completo (81.14 → 31.9)**: ~42 dev-days desde el
cierre de 81.12.e hasta "tercero con Python publica plugin
firmado, otro operator hace `nexo ext install`, plugin corre
con todos los recursos del framework (memory + LLM + tools +
broker + circuit breaker) accesibles vía SDK".

---

## DROPPED ❌ — explicit no-go

These will not ship. Removed from the active sub-phase tally.

| Phase | Reason |
|-------|--------|
| **80.13** — KAIROS_PUSH_NOTIFICATION (APN/FCM/WebPush tool) | Provider-specific mobile push channel. Generic webhook receiver (Phase 80.12 ✅) covers the use case. Adding APN/FCM/WebPush ties nexo to provider-specific creds + lifecycles for marginal benefit. Microapps that need push wire it themselves. |
| **86.2** — `nexo agent debug break-cache` CLI | Debug-only framework-internal tool. Microapps don't consume it. The automatic cache-break detector (Phase 77.4 ✅) already surfaces the events. Manual force-miss can be added ad-hoc when a real bug demands it, not pre-emptively. |
| **ANTI_DISTILLATION** (was eyed in Phase 87 prior-art batch) | Provider-side defense against model distillation. Nexo is a model **consumer**, has nothing to protect against distillation. Fake-tool injection would only confuse our own agent. Permanent skip. |

---

## DEFERRED ⏸ — gated on a specific trigger

These have a real use case but the trigger has not arrived.
Listed here so the design pointer is not lost.

| Phase | Trigger (when to revisit) |
|-------|----------------------------|
| **80.7** — Cron scheduler per-cwd lock owner (multi-instance) | Phase 32 (multi-host orchestration) becoming active. Single-daemon deploys do not need it. |
| ~~**81.13**~~ — folded into Phase 31.6 (`nexo plugin new --lang <rust\|python\|ts>`). |
| **87.2** — Container runtime dispatcher (BYOC) | **Either** Phase 32 multi-host **or** Phase 82 multi-tenant SaaS hardening demanding stronger-than-worktree isolation. Until then, the existing `WorkspaceManager` git-worktree boundary is sufficient. |

---

## Phase 80 — autonomous assistant mode (mostly ✅, residual)

22 sub-phases in original plan. 20 ✅ shipped. 1 DEFER (80.7
above), 1 DROPPED (80.13 above). **Phase 80 is effectively
closed at MVP** for the autonomous-agent core; remaining items
are not gating microapp work.

Open follow-ups against shipped Phase 80 items live in
`FOLLOWUPS.md` § Phase 36.2 + § Audit 2026-04-30 — these are
tactical hardening completions, not promotion-worthy
sub-phases.

---

## Curation pass — what was promoted from FOLLOWUPS.md

Reviewed the open `⬜` and `🟡` items in `FOLLOWUPS.md`. None
warranted promotion to a top-level sub-phase. Reasoning:

- **Phase 36.2 compactions tail** — tiny slice (`CompactionStore`
  schema decision); stays in followups.
- **C4.b.b YAML config bash safety schema** / **C4.c.b
  notify_origin wire** — surgical wiring tasks; stays.
- **Audit 2026-04-30 M-series (M1–M10)** — most are partial /
  shipped slices with tail items. Tail work is still tactical.
  Stays.
- **Phase 67.A–H residuals (PT-1 / PT-2 / PT-3 / PT-6 / PT-7 /
  PT-8)** — these *together* would be a sub-phase-sized effort
  (driver-binary unification + dispatch-telemetry wire-up +
  multi-agent integration test). Flagged here for future
  promotion **if** the user wants to formally schedule it.
  Currently fragmented across followup notes.
- **Phase 79.M MCP server follow-ups** / **Phase 19 V2 pollers**
  / **Phase 21 link / 25 web-search / 26 pairing** — domain-
  specific tactical hardening; stays.

Recommendation: leave followups alone. The signal-to-noise of
the open items is fine where they are. Promote only if a
specific item starts blocking microapp work.

---

## Effort summary

| Bucket | Active dev-days |
|--------|------------------|
| Phase 84 (coordinator persona) | ~6 |
| Phase 83 — agent-creator critical path (★ rows: 82.11/12/13 + 83.10/12/13/14 + 83.15/16/17) | sized in PHASES-microapps.md, ~30 d aggregate |
| Phase 83 non-critical (83.1–83.9 + 83.11) | sized in PHASES-microapps.md |
| Phase 81 plug-and-play (excluding 81.13 DEFER) | unestimated, ~10-15 d |
| Phase 85 compaction hardening | ~5 |
| Phase 86 memory observability (86.1 only) | ~1 |
| Phase 87 LLM-as-judge (87.1 only, after 84) | ~2 |
| **Active total (excl. Phase 83 detail)** | ~14 + Phase 83 critical path |

DEFER pile (~14-22 d if all activated) and DROPPED items are
not counted.

---

## Update protocol

1. When a sub-phase ships, mark it ✅ in the source `PHASES.md`
   / `PHASES-microapps.md` AND update the corresponding row
   here in the same commit.
2. When a new sub-phase is added (after a `/forge brainstorm`
   approval), record it in source AND add a row here under
   the right phase, with a one-line rationale tying it to the
   curation principles above.
3. When a sub-phase is dropped or deferred, move its row from
   ACTIVE to DROPPED ❌ or DEFERRED ⏸ here AND apply the
   marker in the source file.
4. Do not let this file drift — `CLAUDE.md` cites it as the
   single source of truth for active scope, so a stale view
   here mis-leads sprint planning.
