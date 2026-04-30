# Agent Framework — Project Guide

## What this is

Rust multi-agent framework with microservices architecture. Event-driven via NATS message broker. LLM-powered agents (primary: MiniMax M2.5). Full design in `proyecto/design-agent-framework.md`.

## Workspace layout

```
mi-agente/
├── crates/
│   ├── core/          # Agent runtime, EventBus, SessionMgr, CircuitBreaker, Heartbeat
│   ├── plugins/
│   │   ├── browser/   # CDP client → Chrome DevTools Protocol
│   │   ├── whatsapp/  # Wrapper over ../whatsapp-rs crate
│   │   ├── telegram/
│   │   └── email/
│   ├── llm/           # LLM clients — minimax.rs is primary
│   ├── memory/        # short_term.rs, long_term.rs (SQLite), vector.rs (sqlite-vec)
│   ├── broker/        # async-nats client + local fallback
│   └── config/        # YAML loading + env var resolution
├── config/
│   ├── agents.yaml
│   ├── broker.yaml
│   ├── llm.yaml       # API keys via ${ENV_VAR} — never hardcoded
│   ├── memory.yaml
│   └── plugins/
└── secrets/           # gitignored — plaintext key files for Docker secrets
```

## Key decisions

- Broker: **NATS** via `async-nats = "0.35"` (not `natsio` — that crate doesn't exist)
- Primary LLM: **MiniMax M2.5** — implement `crates/llm/src/minimax.rs` first
- Vector search: **sqlite-vec** — zero extra infrastructure
- WhatsApp: `../whatsapp-rs` wraps Signal Protocol + QR pairing — Phase 6 plugs it in
- Secrets: env vars + Docker secrets (`/run/secrets/`), never in YAML values

## Agent-to-agent comms

Topic: `agent.route.{target_id}` — use `correlation_id` to match responses.

## Heartbeat

Agents with `heartbeat.enabled: true` fire `on_heartbeat()` on interval. Used for proactive messages, reminders, external state sync.

## Fault tolerance

Every external call goes through `CircuitBreaker`. NATS offline → fallback to local `tokio::mpsc` + disk queue, drain on reconnect.

## Retry policy

| Component | Max attempts | Backoff |
|-----------|-------------|---------|
| LLM 429 | 5 | 1s → 60s exponential |
| LLM 5xx | 3 | 2s → 30s exponential |
| NATS publish | 3 | 100ms fixed |
| CDP command | 2 | 500ms fixed |

## Implementation phases

Full detail with sub-phases and done criteria: `proyecto/PHASES.md`

| Phase | Name | Sub-phases | Status |
|-------|------|-----------|--------|
| 1 | Core Runtime | 1.1 scaffold, 1.2 config, 1.3 local bus, 1.4 session, 1.5 agent types+trait, 1.6 plugin interface, 1.7 agent runtime | 7/7 ✅ |
| 2 | NATS Broker | 2.1 client, 2.2 abstraction, 2.3 disk queue, 2.4 DLQ, 2.5 circuit breaker, 2.6 backpressure | 6/6 ✅ |
| 3 | LLM Integration | 3.1 trait, 3.2 minimax, 3.3 rate limiter, 3.4 openai-compat, 3.5 tool registry, 3.6 agent loop | 6/6 ✅ |
| 4 | Browser CDP | 4.1 cdp client, 4.2 chrome launch, 4.3 element refs, 4.4 commands, 4.5 event loop, 4.6 session | 6/6 ✅ |
| 5 | Memory | 5.1 short-term, 5.2 sqlite, 5.3 long-term, 5.4 vector, 5.5 memory tool | 5/5 ✅ |
| 6 | WhatsApp Plugin | 6.1 audit+ADR, 6.2 config+bootstrap, 6.3 inbound bridge, 6.4 outbound dispatch, 6.5 media, 6.6 lifecycle+health, 6.7 transcriber, 6.8 e2e, 6.9 qr friendly | 9/9 ✅ |
| 7 | Heartbeat | 7.1 runtime, 7.2 behaviors, 7.3 reminder tool | 3/3 ✅ |
| 8 | Agent-to-Agent | 8.1 protocol, 8.2 routing, 8.3 delegation tool | 3/3 ✅ |
| 9 | Polish | 9.1 logging, 9.2 metrics, 9.3 health, 9.4 shutdown, 9.5 docker, 9.6 integration tests | 6/6 ✅ |
| 10 | Soul, Identity & Learning | 10.1 identity, 10.2 SOUL.md, 10.3 MEMORY.md, 10.4 transcripts, 10.5 recall signals, 10.6 dreaming, 10.7 vocabulary, 10.8 self-report, 10.9 git-backed memory | 9/9 ✅ |
| 11 | Extension System | 11.1 manifest, 11.2 discovery, 11.3 stdio runtime, 11.4 NATS runtime, 11.5 tool registration, 11.6 lifecycle hooks, 11.7 CLI commands, 11.8 templates | 8/8 ✅ |
| 12 | MCP Support | 12.1 client stdio, 12.2 client HTTP, 12.3 tool catalog, 12.4 session runtime, 12.5 resources, 12.6 agent as MCP server, 12.7 MCP in extensions, 12.8 tools/list_changed hot-reload | 8/8 ✅ |
| 13 | Skills (OpenClaw + Google + infra) | 13.1–13.18 (skills + google) + 13.19 anthropic/gemini LLM providers + 13.20 brave-search + 13.21 wolfram-alpha + 13.22 docker-api + 13.23 proxmox | 22/22 ✅ |
| 14 | TaskFlow runtime | 14.1 schema+FlowStore, 14.2 state machine, 14.3 FlowManager, 14.4 wait/resume, 14.5 agent tools, 14.6 mirrored+CLI, 14.7 e2e+docs | 7/7 ✅ |
| 15 | Claude subscription auth | 15.1 config schema, 15.2 anthropic_auth module, 15.3 CLI credentials reader, 15.4 AnthropicClient wiring, 15.5 setup wizard, 15.6 error classification, 15.7 docs, 15.8 OAuth browser PKCE flow, 15.9 Claude-Code request shape (Bearer-only headers + spoof system block) | 9/9 ✅ |
| 16 | Per-binding capability override | 16.1 schema, 16.2 EffectiveBindingPolicy, 16.3 boot validation, 16.4 AgentContext + registry cache, 16.5 runtime intake + rate limiter, 16.6 LLM/prompt/skills/outbound/delegation, 16.7 YAML example + e2e tests | 7/7 ✅ |
| 17 | Per-agent credentials (WA/TG/Google) | 17.1 nexo-auth scaffold, 17.2 boot gauntlet, 17.3 per-channel stores, 17.4 resolver, 17.5 telemetry, 17.6 config schemas, 17.7 `--check-config`, 17.8 runtime integration, 17.9 plugin tool migration, 17.10 google tool store lookup, 17.11 e2e + fingerprint stability | 11/11 ✅ |
| 18 | Config hot-reload | 18.1 deps+schema, 18.2 RuntimeSnapshot+ArcSwap, 18.3 ReloadCommand channel, 18.4 file watcher, 18.5 coordinator, 18.6 intake migration, 18.7 telemetry, 18.8 CLI+boot wiring, 18.9 tests | 9/9 ✅ |
| 20 | `agent_turn` poller | 20.1 PollContext.llm_*, 20.2 with_llm builder, 20.3 agent_turn builtin (cron-style scheduled LLM turn → channel), 20.4 tests + example YAML | 4/4 ✅ |
| 70 | Pairing/Dispatch DX cleanup | 70.1 cody-prompt no-hallucination, 70.2 has_any_override fix, 70.3 `pair list --all`, 70.4 `[intake]`/`[dispatch]` error prefixes, 70.5 pair-start loopback fallback, 70.6 `setup doctor` pairing audit, 70.7 reload flushes gate caches, 70.8 docs sync | 8/8 ✅ |
| 71 | Agent registry persistence + shutdown drain | 71.1 wire `SqliteAgentRegistryStore`, 71.2 boot reattach (`MarkedLost` + notify_origin), 71.3 SIGTERM drain via `drain_running_goals` helper, 71.4 unit tests in `shutdown_drain` + reattach (full SIGTERM e2e deferred), 71.5 docs sync | 5/5 ✅ |
| 72 | Turn-level audit log | 72.1 `TurnLogStore` + `SqliteTurnLogStore`, 72.2 `EventForwarder.with_turn_log` writes per `AttemptResult`, 72.3 `agent_turns_tail` tool, 72.4 9 unit tests across the three modules, 72.5 docs sync | 5/5 ✅ |
| 73 | Claude Code 2.1 MCP wire fixes | 73.1 `--verbose` w/ stream-json, 73.2 `--strict-mcp-config`, 73.3 absolute paths in `.nexo-mcp.json`, 73.4 protocol-version negotiation (incl. 2025-11-25), 73.5 drop `nextCursor: null`, 73.6 `serverInfo.name` matches config-key, 73.7 `permission_prompt_tool` namespace, 73.8 `updatedInput` always-record on allow | 8/8 ✅ |
| 74 | Claude Code 2.1 MCP conformance | 74.1 `claude_2_1_conformance.rs` fixture (6 tests), 74.2 `McpTool.output_schema` + `permission_prompt` declares oneOf union, 74.3 `McpToolResult.structured_content` alongside text | 3/3 ✅ |
| 75 | Acceptance autodetect | 75.1 `default_acceptance()` branches on Cargo/pyproject/setup.py/package.json/CMakeLists, 75.2 7 unit tests with PATH-stubbed tools, 75.3 docs sync | 3/3 ✅ |
| 76 | MCP server hardening | 76.1 HTTP+SSE transport ✅, 76.2 `McpTransport` trait + stdio refactor ✅, 76.3 pluggable auth (StaticToken/BearerJwt/MutualTls) ✅, 76.4 multi-tenant isolation ✅, 76.5 per-principal rate-limit ✅, 76.6 backpressure+concurrency caps ✅, 76.7 server-side notifications+streaming ✅, 76.8 durable sessions+reconnect ✅, 76.9 `McpServerBuilder` ✅ (core; `#[mcp_tool]` macro deferred), 76.10 server observability+health ✅, 76.11 per-call audit log ✅, 76.12 conformance+fuzz suite ✅, 76.13 TLS+reverse-proxy guidance ✅, 76.14 `nexo mcp-server` CLI ops ✅, 76.15 docs+extension template ✅, 76.16 expose_tools whitelist ✅ | 16/16 ✅ |
| 77 | Claude Code parity sweep (claude-code-leak) | 77.1 microCompact ✅, 77.2 autoCompact ✅, 77.3 sessionMemoryCompact ✅, 77.4 promptCacheBreakDetection ✅, 77.5 extractMemories ✅, 77.6 memdir findRelevantMemories ✅, 77.7 memdir secretScanner ✅, 77.8 bashSecurity destructive-command warning ✅, 77.9 bashSecurity sed-in-place + path validation ✅, 77.10 bashSecurity shouldUseSandbox heuristic ✅, 77.11 claudeAiLimits + rateLimitMessages UX ✅, 77.12 Skill `loop` ✅, 77.13 Skill `stuck` ✅, 77.14 Skill `simplify` ✅, 77.15 Skill `verify` ✅, 77.16 AskUserQuestion mid-turn elicitation tool ✅, 77.17 versioned schema migrations system ✅, 77.18 coordinator/worker mode pattern ✅, 77.19 docs + admin-ui sync ✅, 77.20 proactive mode + adaptive Sleep tool ✅ | 20/20 ✅ |
| 79 | Tool surface parity sweep (claude-code-leak tools) | 79.1 EnterPlanMode/ExitPlanMode ✅, 79.2 ToolSearchTool ✅ (MVP — provider filtering deferred), 79.3 SyntheticOutputTool ✅, 79.4 TodoWriteTool ✅, 79.5 LSPTool ✅, 79.6 TeamCreate/TeamDelete ✅ (MVP — spawn-as-teammate deferred to 79.6.b), 79.7 ScheduleCronTool ✅ (MVP — runtime firing deferred), 79.8 RemoteTriggerTool ✅, 79.9 BriefTool (terse-mode toggle — re-spec needed; leak's Brief is SendUserMessage gate, not a terse toggle), 79.10 ConfigTool ✅, 79.11 ListMcpResources + ReadMcpResource ✅ (McpAuth deferred — trait lacks refresh), 79.12 REPLTool ✅, 79.13 NotebookEditTool ✅, 79.M MCP exposure parity sweep ✅ (MVP — Lsp/Team*/Config wiring deferred to 79.M.b/c/d), 79.14 docs + admin-ui sync ✅ | 14/14 ✅ |
| 80 | KAIROS autonomous assistant mode parity (claude-code-leak) | 80.0 surface inventory + design appendix ✅ (`proyecto/design-kairos-port.md` — 7 sections, decisions D-1..D-10, brainstorm-citation checklist), 80.1 autoDream fork-style consolidation ✅ MVP (`crates/dream/` foundation: config + error + ConsolidationLock + ConsolidationPromptBuilder + DreamProgressWatcher + AutoDreamRunner control flow with 7 gates + force bypass + lock acquire/rollback + post-fork escape audit + tracing events; 48 unit tests verde including gate ordering + force bypass + fork timeout + fork error + lock blocked + completed audit row; `crates/dream/` standalone — provider-agnostic per memory rule). 80.1.b ✅ MVP driver-loop post-turn hook integration (AutoDreamHook trait + AutoDreamOutcomeKind + DreamContextLite in nexo-driver-types upstream of cycle; orchestrator gains auto_dream field + builder + invocation site adjacent to Phase 77.5; nexo-dream impl AutoDreamHook for AutoDreamRunner; AutoDreamConfig moved to nexo-config::types::dream cycle-free; DriverEvent::AutoDreamOutcome variant; tests across 4 crates verde). 80.1.b.b ✅ partial (AgentConfig::auto_dream field + 47-fixture workspace sweep via perl multi-line replace + 3 YAML round-trip tests). 80.1.b.b.b ✅ MVP (`nexo_dream::boot::build_runner` helper + BootDeps struct + default_memory_dir/default_dream_db_path path helpers; 7 tests covering disabled short-circuit / validate / mkdir / override / state_root parent / happy path; ~270 LOC; main.rs hookup snippet documented for user-side application when their pre-existing dirty state resolves). 80.1.c ✅ MVP (`crates/dream/src/tools.rs` ~250 LOC + 9 unit tests; `DreamNowTool` + `register_dream_now_tool` + `outcome_to_json` mapper for 6 RunOutcome variants; defensive reason extraction collapsing empty/missing/non-string to `"no reason given"`; required `ctx.session_id` so forced runs anchor to a goal; calls `runner.run_forced(&ctx)` bypassing kairos+remote+time+session gates while keeping lock gate; mirror leak `autoDream.ts:102-179` `runAutoDream` manual trigger + Phase 77.20 Sleep tool shape; capability gate INVENTORY entry split as 80.1.c.b). 80.1.c.b ✅ MVP (`crates/setup/src/capabilities.rs::INVENTORY` appends `extension: "dream", env_var: "NEXO_DREAM_NOW_ENABLED", kind: Boolean, risk: Medium`; `register_dream_now_tool` short-circuits with `tracing::info!` when env unset/falsy; two-layer gate composes with Phase 16 `allowed_tools` per-binding; 4 new nexo-dream env-gate tests + 3 new nexo-setup INVENTORY asserts; `anyhow` promoted from dev-deps to `[dependencies]` in nexo-dream/Cargo.toml fixing pre-existing drift). 80.1.d ✅ MVP (`src/main.rs::Mode::AgentDream(AgentDreamSubcommand)` + 3 verbs `tail|status|kill` + 4 parser arms + dispatch + 3 async run fns + `resolve_dream_db_path` helper with 3-tier resolution `--db` > `NEXO_STATE_ROOT` env > XDG default; opens `dream_runs.db?mode=ro` for tail/status, RW for kill; kill flips `Running`→`Killed` with `--force`, finalises `ended_at`, rolls back `ConsolidationLock` when `--memory-dir` provided; `DreamRunRow` derives `Serialize + Deserialize` so `--json` path renders cleanly; workspace Cargo.toml gained `nexo-dream` + `nexo-driver-types` + `dirs = "5"` + `tempfile = "3"` deps fixing M8.a blocker; 11 inline tests verde; mirror leak `BackgroundTasksDialog.tsx:281,315-317` `DreamTask.kill` semantics, CLI not Ink keyboard). 80.1.g ✅ MVP (`nexo_driver_types::MemoryCheckpointer` trait upstream + `nexo_core::agent::MemoryGitCheckpointer` newtype adapter wrapping `Arc<MemoryGitRepo>` with `spawn_blocking`; `AutoDreamRunner.git_checkpointer` field + `with_git_checkpointer` builder + `has_git_checkpointer` accessor; runner llama `ckpt.checkpoint(subject, body)` después del `audit.update_status(Completed)` SOLO cuando `progress.touched.is_empty() == false`; helper `build_checkpoint_body(run_id, files)` con format `audit_run_id: <uuid>\n\n- path...` para cross-link al audit row; failure → `tracing::warn!` sin downgrade del outcome; BootDeps gana `git_checkpointer: Option`; mirror Phase 10.9 git-backed memory extendido al deep pass — innovación nexo-específica, leak no tiene autoDream→git wiring; provider-agnostic; defense-in-depth preservada AutoMemFilter ∧ secret guard ∧ MAX_COMMIT_FILE_BYTES ∧ Mutex<Repository>; 7 tests nuevos verde — 1 driver-types + 2 nexo-core workspace_git + 4 nexo-dream auto_dream + RecordingCheckpointer mock; 67 nexo-dream tests totales verde; main.rs hookup documentado en doc-comment del builder, espera resolución de dirty state). 80.1.e ✅ MVP (PIVOTED del buffer pattern original al SKIP pattern alineado con leak `extractMemories.ts:121-148` `hasMemoryWritesSince`; nuevo trait `nexo_driver_types::ConsolidationLockProbe` upstream cycle-break; impl sync en `nexo_dream::ConsolidationLock` con fail-open semantics; `DreamReport.deferred_for_fork: bool` field nuevo; `DreamEngine.with_consolidation_probe(probe)` builder; `run_sweep` chequea probe AL INICIO y retorna early con `deferred_for_fork: true` si `is_live_holder() == true` — sin promotions, sin MEMORY.md write, sin SQLite ledger update; trade-off: promotions diferidas al siguiente turno, recoverables porque memorias hot scorean igual de high; 5 probe tests en nexo-dream + 3 dreaming tests en nexo-core + 1 trait test en nexo-driver-types verde; main.rs hookup documentado en doc-comment del builder, diferido hasta resolución de dirty state). 80.1.f ✅ MVP (docs sweep — extendido `docs/src/soul/dreaming.md` con 7 nuevas secciones ~370 LOC: two-tier consolidation tabla comparativa + deep pass via fork con 7 gates + ConsolidationLock + 4-phase prompt + AutoMemFilter + escape audit + skip pattern coordination + audit trail con dream_runs schema + git commits + operator CLI tail/status/kill ejemplos + LLM tool dream_now + capability gate two-layer + configuration yaml ejemplo; mdbook build smoke verde; admin-ui panel reservado para 80.21). Remaining 80.1.d.b (live NATS abort needs 80.11), 80.1.d.c (`agent dream now` operator force needs daemon plumbing), 80.2 cron jitter 6-knob hot-reload config ⬜, 80.3 cron task-id-derived deterministic jitter ⬜, 80.4 cron one-shot vs recurring jitter modes ⬜, 80.5 cron `permanent` flag + `recurringMaxAgeMs` exemption ⬜, 80.6 cron killswitch + missed-task surfacing ⬜, 80.7 cron per-cwd lock owner (DEFERRED until Phase 32) ⬜, 80.8 brief mode + SendUserMessage tool (re-spec of 79.9) ⬜, 80.9 KAIROS_CHANNELS — MCP channels routing + 7-step gate ⬜, 80.10 ✅ MVP SessionKind + BG sessions — `SessionKind` enum 4 variants (`Interactive` default / `Bg` / `Daemon` / `DaemonWorker`) en `nexo-agent-registry::types`; `AgentHandle.kind` field con `#[serde(default)]` backward-compat; schema migration v5 idempotent + index; `list_by_kind` + `reattach_running_kind_aware` helpers; workspace fixture sweep 14+ struct literals; CLI `Mode::AgentRun { prompt, bg, db, json }` + `Mode::AgentPs { kind, all, db, json }` + 2 parser arms + 2 dispatch arms + helper `resolve_agent_db_path` (3-tier `--db` > `NEXO_STATE_ROOT` > XDG default); 13 tests verde (8 agent-registry + 5 bin); CLI smoke manual confirmed. **Deferred follow-ups**: 80.10.b attach (=80.16), 80.10.c daemon supervisor, 80.10.d kill, 80.10.e logs, 80.10.f schema-migration sys integration, 80.10.g daemon-side pickup of queued goals (rows insertan en DB pero el daemon worker pickup automatic no shippea hasta 80.10.g + 80.16 attach), 80.11 ✅ MVP agent inbox subject + ListPeers + SendToPeer (publisher-only); nuevo `crates/core/src/agent/inbox.rs` con `inbox_subject(goal_id)` helper rendering `agent.inbox.<goal_id>` + `InboxMessage` payload (from_agent_id/from_goal_id/to_agent_id/body/sent_at/correlation_id) + size constants; `list_peers_tool.rs` returns JSON peer list excluding self con reachability via `allowed_delegates`; `send_to_peer_tool.rs` con `PeerGoalLookup` closure injected (no agent-registry dep); 6 validation gates (empty/self-send/missing args/oversize/unknown agent_id/no live goals); per-goal fan-out vía broker publish; 15 nuevos tests verde (3 inbox + 1 list_peers + 11 send_to_peer incl. correlation_id wire round-trip via subscribe). `PeerDirectory.peers()` slice accessor added. **80.11.b ✅ MVP** receive side router + per-goal buffer + render helper (`crates/core/src/agent/inbox_router.rs` ~280 LOC + 17 tests verde): `InboxRouter` mirrors Phase 79.6 TeamMessageRouter pattern (single broker subscriber on `agent.inbox.>` wildcard + dashmap per-goal buffers), `InboxBuffer` con MAX_QUEUE=64 + FIFO eviction, buffer-on-demand para race-safety, idempotent `register()`, `parse_goal_from_subject` defensive parser, pure-fn `render_peer_messages_block` returns Option<String> con `<peer-message from=... sent_at=... correlation_id?=...>` shape; cancel via CancellationToken; e2e test verifica round-trip via real broker pubsub. **Deferred**: 80.11.b.b llm_behavior per-turn drain+render injection (blocked dirty-state), 80.11.b.c main.rs router spawn + lifecycle hooks, 80.11.c broadcast `*`, 80.11.d cross-machine cluster, 80.11.e bridge protocol responses, 80.11.f main.rs tool registration wire, 80.12 generic webhook receiver (provider-agnostic — re-scoped 2026-04-30 from "KAIROS_GITHUB_WEBHOOKS" per Cristian; sólo HTTP receiver detrás de tunnel + signature verify configurable HMAC-SHA256/SHA1/raw-token + event-kind extraction header/body + publish a NATS subject; sin GitHub event router, sin `github_subscribe` tool, sin `crates/plugins/github/` — providers se configuran 100% en YAML; channels + pollers cubren el resto del inbound) ⬜, 80.13 KAIROS_PUSH_NOTIFICATION — APN/FCM/WebPush provider trait + tool ⬜, 80.14 ✅ MVP AWAY_SUMMARY — template-based slim MVP; nuevo `AwaySummaryConfig` schema (enabled/threshold_hours/max_events) en `nexo-config::types::away_summary` + `AgentConfig.away_summary: Option` con `#[serde(default)]` + 49 fixture sweep; `TurnLogStore::tail_since(since, limit)` trait method con default-empty fallback + Sqlite override; nuevo `nexo-dispatch-tools/src/away_summary.rs` con `try_compose_away_digest()` pure-async fn (4 gates ordenados: enabled/last_seen/threshold/non-empty) + `build_digest()` pure-fn renderer markdown (counters completed/aborted/failed/other + truncation suffix); 11 tests verde + 6 config tests verde; LLM-summarised version deferida 80.14.b, last_seen pairing-store tracking 80.14.c, per-channel rendering 80.14.d, main.rs hookup 80.14.g, 80.15 assistant module — kairos-active flag + system addendum + initial team ⬜, 80.16 ✅ MVP `nexo agent attach` + `nexo agent discover` CLI — DB-only viewer slim MVP; `Mode::AgentAttach` + `Mode::AgentDiscover` + 2 parser arms + 2 dispatch arms + 2 run fns; **attach** valida UUID upfront, fetch handle, render markdown con kind/status/phase/started/finished/last_progress/diff/turn/event, hint diferenciado Running vs terminal; **discover** filtra a BG/Daemon/DaemonWorker Running por defecto, broadens con `--include-interactive`; reusa `list_by_kind` + `get` + `resolve_agent_db_path` helpers; 8 nuevos tests verde + CLI smoke manual confirmed. **Deferred follow-ups**: 80.16.b live NATS event streaming (requires nexo-broker connect), 80.16.c user input piping (depends on Phase 80.11 inbox), 80.16.d interactive TUI, 80.17 ✅ MVP `auto_approve` mode — renombrado de `kairos_remote_control` para mantener nombre descriptivo sin attribution; nuevo `crates/driver-permission/src/auto_approve.rs` (~280 LOC + 27 tests verde) con `is_curated_auto_approve(tool_name, args, auto_approve_on, workspace_path) -> bool` decision table 25 entries (read-only/info-gathering siempre auto; Bash conditional pasa solo si `is_read_only` AND no destructive AND no sed-in-place — Phase 77.8/77.9 SIEMPRE vetan; FileEdit/Write solo bajo `workspace_path` canonical con symlink-escape defense + parent-canonicalize fallback para archivos nuevos; notifications + memory + coordination siempre auto; ConfigTool/REPL/remote_trigger/schedule_cron NEVER auto; mcp_/ext_ prefix default-ask; default arm `_ => false`). `AgentConfig.auto_approve: bool` + `InboundBinding.auto_approve: Option<bool>` con `#[serde(default)]`. `EffectiveBindingPolicy.auto_approve` + `workspace_path` resuelto via override > agent default. 49+14 fixture sites swept. **Deferred follow-ups**: 80.17.b hook into approval gate at `mcp.rs` (helper existe, falta integration), 80.17.c setup doctor warn `assistant_mode + !auto_approve`, 80.17.d audit log, 80.17.e CLI override, 80.17.f customisable allowlist, 80.18 DreamTask audit-log row ✅ (`crates/agent-registry::dream_run` 26 tests, `DreamRunStore` trait + `SqliteDreamRunStore` impl, schema migration v4 idempotent + 3 indexes, MAX_TURNS=30 server-side cap, TAIL_HARD_CAP=1000 defends tail-OOM, JSON columns for files_touched + turns avoid join tables, `prior_mtime_ms: Option<i64>` distinguishes Some(0) from None for autoDream lock semantics, fork_label flexible String supports auto_dream/away_summary/eval; reattach_running hook ready, integration in 80.18.b follow-up), 80.19 forked subagent infra (cache-safe + skipTranscript) ✅ (`crates/fork/` 9 modules + 42 unit tests, `CacheSafeParams::from_parent_request` preserves partial tool_use per leak `:522-525`, `DelegateMode::{Sync,ForkAndForget}`, `ForkHandle::take_completion` + Drop-cancels-abort, `tracing` span `fork.subagent` + `WARN fork.cache_break_detected` Phase 77.4 hook; `delegation_tool.rs` refactor + `agent_handles` row write deferred as 80.19.b/80.10 follow-ups) ⬜, 80.20 auto-mem `can_use_tool` whitelist for forked dream ✅ (`crates/fork::AutoMemFilter` 24 tests + `nexo_driver_permission::bash_destructive::is_read_only` 19 tests, composes Phase 77.8/77.9 classifiers + whitelist of ~45 read-only utilities, fail-fast missing dir + canonicalize-at-construction + canonicalize-per-call symlink/traversal defense, provider-agnostic flat-args contract verified by 3 transversality tests), 80.21 ✅ MVP docs + admin-ui sync (5 new pages en `docs/src/`: assistant-mode.md / auto-approve.md / away-summary.md / multi-agent-coordination.md / cli/agent-bg.md; SUMMARY.md reorganized con nueva sección Assistant mode; mdbook build smoke verde sin broken links; 6 new tech-debt entries en `admin-ui/PHASES.md` cubriendo assistant_mode + auto_approve + BG sessions + AWAY_SUMMARY + multi-agent inbox + AutoDream operator UI panels), 80.12 ✅ MVP generic webhook receiver (nuevo `crates/webhook-receiver/` ~700 LOC + 33 tests verde; `WebhookSourceConfig` YAML schema con SignatureSpec con HmacSha256/HmacSha1/RawToken algorithms + EventKindSource header.NAME o body.json-path; pure-fn `verify_signature` constant-time via subtle::ConstantTimeEq + `extract_event_kind` + `render_publish_topic`; `WebhookHandler::handle` orquesta 4 gates ordenados — body cap → signature → event kind → subject safety; `RejectReason` typed con 7 variantes; provider-agnostic decision-table data-driven via YAML; HTTP listener integration deferred 80.12.b — operator wires via axum/hyper/health-server route) | 19/22 (+80.17.b ✅ MVP `AutoApproveDecider` decorator hooks `is_curated_auto_approve` into any `PermissionDecider` chain via `request.metadata.{auto_approve, workspace_path}`; 6 decorator tests verde; main.rs boot wire + caller-side metadata population deferred as 80.17.b.b/c) |

| 81 | Plug-and-Play Plugin System | 81.2 ✅ MVP `NexoPlugin` trait + `PluginInitContext` en `nexo-core::agent::plugin_host` (~470 LOC + 8 tests verde): async `NexoPlugin` con `manifest()` + `init(ctx) -> Result<(), PluginInitError>` + `shutdown() -> Result<(), PluginShutdownError>` (default Ok), 11-field `PluginInitContext<'a>` exposing handles a `ToolRegistry` + `Arc<RwLock<AdvisorRegistry>>` + `HookRegistry` + `AnyBroker` + `LlmRegistry` + `ConfigReloadCoordinator` + `SessionManager` + `Option<Arc<LongTermMemory>>` + config_dir/state_root paths + `CancellationToken`, helpers `plugin_config_dir(id)` / `plugin_state_dir(id)`. `PluginInitError` 5 variants (`MissingNexoCapability`/`UnregisteredTool`/`Config`/`ToolRegistration`/`Other`) + `PluginShutdownError` 2 variants (`Timeout`/`Other`) — todos thiserror-typed con `plugin_id` field. `DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT = 5s` const. Compile-time dyn-safety check via `static _OBJECT_SAFE_CHECK: OnceLock<Arc<dyn NexoPlugin>>`. Re-exported en `crates/core/src/agent/mod.rs`. `nexo-core` Cargo.toml ganó deps `nexo-plugin-manifest` + `nexo-driver-permission`. Distinct del existing Channel `Plugin` trait (`crates/core/src/agent/plugin.rs`) — diferente file + diferente trait name. IRROMPIBLE refs: claude-code-leak `src/tools/*` absence (no plugin trait); `research/src/plugins/runtime/types-channel.ts:56-71` register/dispose pattern adapted via Rust Drop; `research/src/plugins/activation-context.ts:27-44` activation metadata. Provider-agnostic by construction. 81.1 ✅ MVP `nexo-plugin-manifest` crate (~860 LOC + 25 tests verde): `PluginManifest` struct con 14 sub-sections (`plugin.{capabilities,tools,advisors,agents,channels,skills,config,requires,capability_gates,ui,contracts,meta}`) + 3 enums (`Capability` 10 variants, `GateKind` 3, `GateRisk` 4) + `ChannelDecl` + `CapabilityGateDecl` + `UiHint` mirror OpenClaw `PluginConfigUiHint`. `PluginManifest::{from_str, from_path, validate, id, version}` API. 4-tier defensive validator: syntactic (`#[serde(deny_unknown_fields)]` everywhere) + field-level (id regex `^[a-z][a-z0-9_]{0,31}$` + semver + path security `..` + absolute-path rejection) + cross-field (tool namespace `<id>_` prefix + deferred ⊆ expose + capability_impl + duplicate gate env_var) + runtime (`min_nexo_version` `VersionReq.matches(current)`). `validate()` collects ALL errors into `Vec<ManifestError>` (no bail-on-first). `ManifestError` enum 13 variants thiserror-typed con operator-actionable messages. Reference manifest `examples/marketing-example.toml` documenta cada sección. IRROMPIBLE refs: `research/src/plugins/manifest-types.ts:1-20` mirror, `manifest.ts:17,54-60` precedent + `extensions/firecrawl/openclaw.plugin.json` example; claude-code-leak absent (hardcoded tools). **Deferred follow-ups**: 81.3 namespace runtime enforcement, 81.4 plugin config loader, 81.5 PluginRegistry discovery, 81.6 agent contribution, 81.7 skills_dir contribution, 81.8 ChannelAdapter trait, 81.9 main.rs registry sweep, 81.10 hot-load Phase 18 integration, 81.11 plugin doctor, 81.12 existing plugin migration, 81.13 reference template + docs | 2/13 |

**Progress: 265 implemented sub-phases / 2 deferred sub-phases tracked in `PHASES.md` (26.aa pair_approve tool [security-review-gated], 19.x pollers V2 backlog). Phase 67 (Claude Code self-driving agent) — 67.0–67.9 plus 67.A.1–67.H.6 shipped (foundation, spawn, binding, permission, loop, acceptance, git-worktree, memory, replay-policy, compact-policy; project-tracker, multi-agent registry, async dispatch, capability gate, completion hooks, query / control / admin tools, CLI espejo, NATS subjects, hot-reload-aware tool filter, docs); 67.10–67.13 backlogged (the original 67.10 'Escalación a WhatsApp/Telegram' is largely subsumed by the 67.F notify_origin / notify_channel hooks). Phase 68 (Local LLM tier — llama.cpp) — 15 sub-phases backlogged (transversal tier-0 inference for PII / embeddings / poller pre-filter / classifiers / fallback; **model-agnostic GGUF loader** — gemma3, qwen2.5, llama3.2, phi3, smolLM, etc. all swappable per-job via config — defaults gemma3-270m + bge-small via `llama-cpp-2`, target Termux ARM CPU + desktop CPU/GPU; complementary to Phase 46 which treats local LLM as primary agent provider). TaskFlow integration points threaded through 19.x (P-4 batch polls), 26.ac (companion-tui pairing), 68.15 (tier-0 batch / chunked / async patterns). Phase 21 (link understanding), Phase 25 (web_search), and Phase 26 (pairing + per-channel reply adapters) shipped. Phases 22–24 backlogged in `PHASES.md` (Slack/Discord plugins, realtime voice, image generation). `web_fetch` deferred — Phase 21 covers user-shared URLs. Pairing companion-tui deferred and direct in-process `Session::send_text` delivery for the challenge is deferred (adapters currently publish via broker on `plugin.outbound.{channel}`); store + gate + CLI + WA/TG `PairingChannelAdapter` impls + adapter registry all live. Detailed status of every deferred item lives in `proyecto/FOLLOWUPS.md`. Phase 69 (Setup wizard agent-centric submenu) shipped — per-agent dashboard inside `nexo setup → Configurar agente`, helpers in `yaml_patch` (read/upsert/remove/append-idempotent/remove-by-predicate), hot-reload trigger after every mutation. Phase 72 (Turn-level audit log) shipped — `nexo-agent-registry` gains a `TurnLogStore` trait + `SqliteTurnLogStore` (table `goal_turns`, idempotent on `(goal_id, turn_index)`, tail capped at 1000); `EventForwarder.with_turn_log(store)` appends a row per `AttemptResult` best-effort (warns on failure, never blocks the loop); new read tool `agent_turns_tail goal_id=<uuid> [n=20]` registered in `READ_TOOL_NAMES` returns a markdown table with one row per turn for post-mortem debugging that survives daemon restart. Phase 71 (Agent registry persistence + shutdown drain) shipped — `src/main.rs` now reads `agent_registry.store` and opens `SqliteAgentRegistryStore` (env placeholders resolved); boot honours `reattach_on_boot: true` to flip prior-run Running rows to `LostOnRestart` and fire `notify_origin` once per goal so the originating chat sees a clean `[abandoned]` closure; SIGTERM runs `nexo_dispatch_tools::drain_running_goals` BEFORE plugin teardown so `[shutdown]` notify_origin actually leaves the channel; per-hook 2 s timeout keeps stuck publishers from hanging shutdown; `DispatchToolContext` gains `hook_dispatcher: Option<Arc<dyn HookDispatcher>>`. Phase 27.4 (Debian + RPM packages — Tier 1 + Tier 3) shipped — `release.yml` gains 4 jobs (build-debian matrix x2 amd64/arm64, build-rpm matrix x2 x86_64/aarch64) that reuse the musl static binary already built by `build-musl` (zero recompile) plus 2 install-test matrix jobs (`install-test-deb` on debian:12+ubuntu:22.04+ubuntu:24.04, `install-test-rpm` on fedora:40+rockylinux:9). Smoke = `apt install ./*.deb` / `dnf install ./*.rpm` + `nexo --version` regex match + `nexo --help`. `packaging/debian/build.sh` control cleaned up: `Pre-Depends: adduser`, `Depends: ca-certificates` only (musl-static bundles libsqlite/libssl), `Recommends:` ampliated with cloudflared/yt-dlp/python3. `packaging/rpm/nexo-rs.spec` drops `Requires: sqlite-libs` + `Requires: openssl-libs`, mirrors Recommends. Broken VERSION awk regex (`gsub(/.*"|".*/)` greedy) fixed in all 3 packaging scripts to `grep -m1 '^version' | cut -d'"' -f2`. `packaging/rpm/build.sh` cp of systemd unit fixed to read from `packaging/debian/` (single source of truth). `docs/src/getting-started/install-deb.md` + `install-rpm.md` (NEW, registered in SUMMARY.md) document quick-install + sha256/cosign verify + 27.4.b deferral. Tier 2 (signed apt/yum repos in GH Pages, GPG key management, `apt-ftparchive` + `createrepo_c`, bootstrap one-liner) split out as new sub-phase 27.4.b ⬜ open. Phase 27.2 (GH Actions release workflow) shipped — `.github/workflows/release.yml` rewritten end-to-end. Triggers on `push.tags: ["nexo-rs-v*"]` (matches release-plz `git_tag_name`); `name: release` preserved so the `workflow_run` chain in `sign-artifacts.yml` (27.3) + `sbom.yml` (27.9) keeps firing. 5 jobs — `validate-tag` (regex + `gh release view` precondition), `build-musl` matrix x2 (x86_64 + aarch64, ubuntu-latest, fail-fast: false), `build-termux` (aarch64-linux-android via `packaging/termux/build.sh`, ubuntu-latest, separate compile because Termux is bionic-libc not musl), `publish` (`gh release upload --clobber`), `smoke-test` (extracts host musl tarball, verifies short `nexo --version` + verbose provenance stamps `channel:tarball-x86_64-unknown-linux-musl` + `target:x86_64-unknown-linux-musl`). Toolchain pins: zig 0.13.0 via `goto-bus-stop/setup-zig@v2`, cargo-zigbuild 0.22.3, cargo-dist 0.31.0; Swatinem/rust-cache@v2 keyed per-target+Cargo.lock. `NEXO_BUILD_CHANNEL` injected per runner closes the 27.1 provenance deferral. **Scope reduced**: Apple (`x86_64`/`aarch64-apple-darwin`) and Windows (`x86_64-pc-windows-msvc`) targets dropped from the matrix; Phase 27.6 (Homebrew) parked; `dist-workspace.toml::targets` shrunk to 2 musl entries; `installers = ["shell"]` only; `scripts/release-check.sh` reduced + Termux `.deb` glob check added; `Makefile::HOST_TARGET` defaults to `x86_64-unknown-linux-musl`; `packaging/termux/build.sh` emits `<deb>.sha256` sidecar. Phase 27.1 (cargo-dist baseline) shipped — `dist-workspace.toml` declares the host-fallback + 5-target shippable matrix (`x86_64-unknown-linux-gnu` + `x86_64`/`aarch64-unknown-linux-musl` + `x86_64`/`aarch64-apple-darwin` + `x86_64-pc-windows-msvc`), `precise-builds = true` with `[package.metadata.dist] dist = false` opt-out on every internal bin-bearing crate (driver-permission, driver-loop, dispatch-tools, companion-tui, mcp's mock_mcp_server) and dev programs (`browser-test`, `integration-browser-check`, `llm_smoke`) reshaped as `[[example]]` under `examples/` so cargo-dist excludes them; `build.rs` injects four compile-time stamps (`NEXO_BUILD_GIT_SHA`, `NEXO_BUILD_TARGET_TRIPLE`, `NEXO_BUILD_CHANNEL`, `NEXO_BUILD_TIMESTAMP`) consumed by the new `Mode::Version { verbose }` (`nexo version` and `nexo --version --verbose` print provenance, `nexo --version` keeps the short form); `make dist-check` (= `dist build --target $(HOST_TARGET) && scripts/release-check.sh`) is the local smoke gate that verifies tarball contents + sha256 + host-native `--version` and emits WARN (not FAIL) for matrix members the local toolchain can't build. release-plz remains the source of truth for version bumps + crates.io publish + per-crate CHANGELOG; cargo-dist owns binary tarballs only; full musl/darwin/msvc matrix lands on Phase 27.2 CI runners. Operator + contributor docs in `packaging/README.md` + `docs/src/contributing/release.md`. Phase 70 (Pairing/Dispatch DX cleanup) shipped — Cody prompt forbids hallucinated tool denials; `binding_validate::has_any_override` now sees `dispatch_policy` / `pairing_policy` / `language` / `link_understanding` / `web_search` so dispatch-only bindings stop printing the no-overrides warn; `PairingStore::list_allow` + `nexo pair list --all [--include-revoked]` make seeded senders visible; every `DispatchDenied` and runtime pairing log is prefixed with `[dispatch]` / `[intake]` so the origin of a "trusted" denial is unambiguous; `nexo pair start` loopback fallback prints ready-to-run `pair seed` commands per configured plugin instance; `nexo setup doctor` audits `(channel, account_id)` tuples whose binding has `auto_challenge: true` but an empty allowlist; `ConfigReloadCoordinator::register_post_hook` flushes the `PairingGate` decision cache after every successful reload so newly-seeded senders take effect without a daemon restart. Phase 15.9 (Anthropic OAuth Claude-Code request shape) shipped — Bearer-auth requests (SetupToken / OAuth bundle / Claude Code CLI import) failed silently for Opus 4.x and Sonnet 4.x because the request was missing the Claude-Code identity claim Anthropic uses to gate non-Haiku models, and the resulting 4xx body was hidden behind a generic "no quota" `LlmError::Other`. Fix mirrors OpenClaw `research/src/agents/anthropic-transport-stream.ts:558-641`: `AnthropicAuth::subscription_betas()` returns `&["claude-code-20250219","oauth-2025-04-20","fine-grained-tool-streaming-2025-05-14"]`, `AuthHeaders.extra` carries `User-Agent: claude-cli/<version>`, `x-app: cli`, and `anthropic-dangerous-direct-browser-access: true` only on Bearer paths, and `build_body` now prepends the canonical `"You are Claude Code, Anthropic's official CLI for Claude."` text block at `system[0]` whenever `AnthropicAuth::is_subscription()` is true (legacy `system_prompt` strings are promoted to a 2-element array; existing `system_blocks` get the spoof inserted at index 0). Operators can override the CLI version stamp via `NEXO_CLAUDE_CLI_VERSION` env (default `2.1.75`) without rebuilding when Anthropic bumps the accepted version. `classify_response` now `tracing::warn!`s the truncated body before collapsing generic 4xxs into `LlmError::Other` so the next "no quota" surprise leaves a real reason in logs. `crates/llm/src/text_sanitize.rs` ports OpenClaw's surrogate-stripping helper as a defensive parity guard. API-key path is unchanged — none of the spoof headers or system block leak into static `x-api-key` requests. Phase 77 (Claude Code parity sweep — claude-code-leak) opened with 19 sub-phases backlogged: 77.1 microCompact + 77.2 autoCompact + 77.3 sessionMemoryCompact/postCompactCleanup (multi-tier context compression to keep long-running goals under the model context window without losing the audit trail), 77.4 promptCacheBreakDetection (anthropic.rs logs cache_read_input_tokens drops > 50 % so cache-miss surprises leave a diagnosable line), 77.5 extractMemories (post-turn LLM extraction, complements Phase 10.6 dreaming with an inline path), 77.6 memdir findRelevantMemories + age-decay scorer (relevance × recency × access), 77.7 memdir secretScanner + teamMemSecretGuard (regex set blocks Anthropic/OpenAI/GitHub/AWS/Stripe/Google/JWT shapes before any memory commit), 77.8–77.10 bashSecurity (destructive-command warning, sed-in-place + path validation, shouldUseSandbox heuristic with bwrap/firejail probe), 77.11 claudeAiLimits + rateLimitMessages UX (`LlmError::QuotaExceeded { retry_after, plan_hint }` surfaced in `setup doctor` + notify_origin), 77.12–77.15 four bundled skills (`loop`, `stuck`, `simplify`, `verify` — verify pairs with Phase 75 acceptance autodetect), 77.16 AskUserQuestion mid-turn elicitation tool (TaskFlow wait/resume hook, survives daemon restart, default 3600 s timeout), 77.17 versioned schema migrations system (`crates/config/migrations/`, `nexo setup migrate --dry-run|--apply`, Phase 18 hot-reload re-validates post-migration snapshot), 77.18 coordinator/worker mode pattern (binding-level `role: coordinator|worker` with curated tool subset for workers + graceful mode-mismatch on session resume), 77.19 docs + admin-ui sync, 77.20 proactive mode + adaptive Sleep tool (mirror of Claude Code's `--proactive`/KAIROS feature flag — agent receives periodic `<tick>` injections from driver-loop, decides whether to act or call a new `Sleep { duration_ms, reason }` tool that pauses the goal and schedules a wake-up; cache-aware scheduler prefers waits under 270 s to keep the Anthropic prompt cache warm or ≥ 1200 s when amortising a cache miss; activated per binding via `proactive: { enabled: true, tick_interval_secs, jitter_pct, max_idle_secs }`; mutually exclusive with `role: coordinator`; built on Phase 20 `agent_turn` poller + Phase 67 driver-loop). Voice/STT, Ink UI, IDE bridge, GrowthBook analytics explicitly out of scope. Phase 79 (Tool surface parity sweep — claude-code-leak tools) opened with 14 sub-phases backlogged: 79.1 EnterPlanMode/ExitPlanMode (read-only planning mode toggleable mid-turn, pairs with Phase 75 acceptance autodetect + Phase 67 self-driving), 79.2 ToolSearchTool (deferred-schema discovery — tools advertise name+description in the system prompt and the schema body loads on demand via `ToolSearch(select:Foo)`, cuts MCP token cost when the surface is wide), 79.3 SyntheticOutputTool (typed/structured output forcing — model fills a JSON schema instead of free prose, direct input for Phase 19/20 pollers + Phase 51 eval harness), 79.4 TodoWriteTool (intra-turn scratch list, ephemeral, distinct from TaskFlow's cross-session persistent tasks, helps long Phase 67 driver-loop turns coordinate sub-steps without spawning sub-goals), 79.5 LSPTool (rust-analyzer + pylsp + tsserver in-process — go-to-def, hover, references, diagnostics as a tool, big multiplier for Phase 67 dev workflows), 79.6 TeamCreateTool/TeamDeleteTool (N parallel coordinated agents with shared scratchpad — distinct from AgentTool's 1-to-1 delegate, suited to research fan-out + massive refactors), 79.7 ScheduleCronTool (agent schedules its own cron entry from inside a turn — complements Phase 7 Heartbeat which is config-time only), 79.8 RemoteTriggerTool (LLM-time webhook / NATS publish — wraps existing tunnel + broker), 79.9 BriefTool (terse-mode toggle for pairing companion-tui), 79.10 ConfigTool (gated self-config — agent reads/writes its own YAML through Phase 18 hot-reload, requires capability gate strong enough for `nexo setup` conversational flow), 79.11 McpAuthTool + ListMcpResourcesTool + ReadMcpResourceTool (LLM-driven MCP introspection — agent navigates resources autonomously, Phase 12.5 follow-up), 79.12 REPLTool (Python/Node stateful sandbox preserving variables across turns — needs sandbox infra), 79.13 NotebookEditTool (Jupyter cell-level edits with output preservation), 79.14 docs + admin-ui sync. Phase 79 references the leak's `src/tools/` dir directly per sub-phase. Phase 80 (KAIROS autonomous assistant mode parity — claude-code-leak) opened with 22 sub-phases backlogged covering the gaps identified after deep-mining `claude-code-leak/src/services/autoDream/` + `src/utils/{forkedAgent,cronScheduler,cronTasks,cronJitterConfig,concurrentSessions,conversationRecovery}.ts` + `src/services/mcp/channelNotification.ts` + `src/tools/{BriefTool,ScheduleCronTool,SubscribePRTool,PushNotificationTool,SleepTool}/` + `src/tasks/DreamTask/DreamTask.ts` + `src/main.tsx` KAIROS integration points (lines 559, 685, 1058-1088, 1075, 2197-2208, 2518, 2916, 3035, 3259-3340, 3832-3845, 4334, 4612-4625): 80.0 surface inventory + design appendix, 80.1 autoDream fork-style consolidation (PID-mtime `.consolidate-lock`, time-gate 24 h + session-gate ≥ 5 transcripts + scan throttle 10 min, forked subagent with `skipTranscript:true` + 4-phase consolidation prompt + auto-mem `can_use_tool` whitelist; complements existing scoring-based `crates/core/src/agent/dreaming.rs` rather than replaces it — scoring stays as light pass, fork is the deep pass), 80.2 cron jitter 6-knob hot-reload config (replaces `with_jitter_pct(pct)` with `recurring_frac/recurring_cap_ms/one_shot_max_ms/one_shot_floor_ms/one_shot_minute_mod/recurring_max_age_ms` refreshed via Phase 18 watcher), 80.3 cron task-id-derived deterministic jitter (`jitter_frac(entry_id) = u32::from_str_radix(&entry_id[..8],16) as f64 / u32::MAX as f64` so retries don't move target), 80.4 cron one-shot vs recurring jitter modes (forward `t1 + min(frac*(t2-t1), cap)` for recurring, backward lead `max(t1 - lead, fromMs)` for one-shot only when minute % mod == 0), 80.5 cron `permanent` flag exempting built-ins from `recurringMaxAgeMs` auto-expiry, 80.6 cron killswitch (per-tick `enabled` poll + per-binding override + missed-task surfacing on boot atomically setting `next_fire = i64::MAX` to prevent double-fire), 80.7 cron per-cwd `.scheduler-lock` lock owner pattern (DEFERRED until Phase 32 multi-host orchestration), 80.8 brief mode + `SendUserMessage` tool (re-spec of Phase 79.9 BriefTool — leak's BriefTool is a *gating mechanism* on a SendUserMessage tool: when brief on, free-text output is hidden and only `SendUserMessage` calls render; activation `(kairos_active OR user_msg_opt_in) AND entitled`, 5-min refresh, `/brief` slash command + system reminder injection, Phase 26 channel adapters honour `brief_only`), 80.9 KAIROS_CHANNELS — MCP channels routing (servers declare `capabilities.experimental['claude/channel']`, inbound wrapped in `<channel source="...">` XML, structured `ChannelPermissionRequestParams` outbound permission flow, 7-step gate: capability → runtime → OAuth → org policy → session `--channels` allowlist → plugin marketplace → `allowed_channel_plugins` allowlist), 80.10 SessionKind enum + BG sessions (`Interactive | Bg | Daemon | DaemonWorker` on `AgentHandle`, `nexo agent run --bg <prompt>` detached spawn, `nexo agent ps [--all] [--kind=bg]` listing, schema migration v3 via Phase 77.17 system, Phase 71 reattach honours kind), 80.11 agent inbox subject + `ListPeers` + `SendToPeer` LLM tools (`agent.inbox.<goal_id>` NATS subject contract supersedes leak's UDS_INBOX), 80.12 KAIROS_GITHUB_WEBHOOKS — github plugin + receiver (HTTP receiver behind tunnel with `X-Hub-Signature-256` HMAC verify, event router for `pull_request|issue_comment|push|workflow_run`, `github_subscribe` LLM tool, `${GITHUB_WEBHOOK_SECRET}` config), 80.13 KAIROS_PUSH_NOTIFICATION — `PushProvider` trait + APN (token-based p8 key) + FCM (HTTP v1 service account JSON) + WebPush (VAPID) + `notify_push` LLM tool distinct from `notify_origin`, 80.14 AWAY_SUMMARY — re-connection digest (track `last_seen_at` per (channel, account_id) in pairing store, on inbound after `> threshold` h gap spawn forked goal that summarises Goals/aborts/notify_origins/cron-fires from Phase 72 turn-log, deliver before user inbound is processed, off by default), 80.15 ✅ MVP assistant module — nuevo crate `nexo-assistant` con `AssistantConfig` + `ResolvedAssistant::resolve()` + `DEFAULT_ADDENDUM` provider-agnostic; `AgentConfig.assistant_mode: Option` field con backward-compat sweep de 49 fixtures; `AgentContext.assistant: ResolvedAssistant` field default disabled; system-prompt injection wired en `llm_behavior.rs` post proactive/coordinator hints con cache-friendly ordering. 13 nuevos tests verde. **Deferred follow-ups**: 80.15.b initial_team auto-spawn (needs 80.10), 80.15.c cron flip (needs 80.6), 80.15.d brief flip (needs 80.8), 80.15.f doctor reporter, 80.15.g main.rs boot wiring (1-line snippet en doc-comment, espera dirty-state user). Original "replaces leak's runtime-computed `kairosEnabled`", 80.16 `nexo agent attach <goal_id>` + `nexo agent discover` CLI (re-attach a TTY to a running BG/Daemon goal via Phase 67 NATS subjects, mirror of leak's `_pendingAssistantChat = { sessionId, discover }`), 80.17 `kairos_remote_control` mode (auto-approve curated tool subset within Phase 16 capability gate — gate stays authoritative, mode only flips auto-approve dial; `setup doctor` warns when `assistant_mode: true` AND `kairos_remote_control: false`), 80.18 DreamTask audit-log row (new `dream_runs` SQLite table joined to `goal_id` with `status/phase/sessions_reviewing/files_touched(JSON)/prior_mtime/started_at/ended_at`, `dream_runs_tail` LLM tool, `dream.kill <run_id>` admin CLI), 80.19 forked subagent infra (generalise `delegation_tool` with `mode: { Sync | ForkAndForget }`, `runtime::fork_subagent` with cache-safe params — system_prompt/user_context/system_context/tool_use_context/fork_context_messages all five must match parent for cache hit — and `skip_transcript: true` semantics), 80.20 auto-mem `can_use_tool` whitelist for forked dream (`FileRead/Glob/Grep/REPL` unrestricted, `Bash` only when `bash_security::is_read_only`, `FileEdit/FileWrite` only when `file_path.starts_with(memory_dir)`, structured denial message), 80.21 docs + admin-ui sync (new `docs/src/concepts/kairos-mode.md` + `docs/src/operations/cron-jitter.md` pages, admin-ui A-N "Assistant mode" panel, `crates/setup/src/capabilities.rs::INVENTORY` registers `${GITHUB_WEBHOOK_SECRET}` + push provider env toggles + `NEXO_KAIROS_REMOTE_CONTROL`). Effort estimate: 80.1 ~3-4 d, 80.9 ~2-3 d, 80.12 ~1.5-2 d (re-scoped to generic webhook receiver, was ~3 d), 80.13 ~3 d (3 providers, 2 share most code), 80.14 ~2 d, 80.10 ~2 d, 80.19 ~3 d, others ≤ 1 d each → ~23-28 dev-days for full parity. Out of scope vs leak: GrowthBook itself (use Phase 18 hot-reload + binding policy), UDS sockets (NATS subjects supersede), cloud session-history API (Phase 72 turn log is local + offline + already shipped), Anthropic-internal `tengu_*` flag namespace (we expose the same knobs through `agents.yaml` + per-binding override).**

### Progress tracking rule

**After every sub-phase is done:** update `proyecto/PHASES.md` — mark the sub-phase `[x]`, update the count in CLAUDE.md table and global total.

Format in PHASES.md:
```
### 1.1 — Workspace scaffold   ✅
### 1.2 — Config loading       🔄  (in progress)
### 1.3 — Local event bus      ⬜
```

## Commands

```bash
cargo build --workspace
cargo test --workspace
cargo run --bin agent -- --config config/agents.yaml
```

## Language rules

- **All code comments in English**
- **All code (variables, functions, types, modules) in English**
- **All repository Markdown/docs in English** (except proper names/legal text)

## MANDATORY: Before every sub-phase

**Before writing any code for any sub-phase, always run `/forge brainstorm <topic>` first.**

No exceptions. Even if the sub-phase seems obvious. Brainstorm:
1. Mines OpenClaw (`../research/` if available, or the repo at `/home/familia/chat/research/`) for patterns, pitfalls, and decisions already made
2. **Also mines `claude-code-leak/` (`/home/familia/claude-code-leak/`)** — leaked Anthropic Claude Code CLI source (TypeScript, ~1,900 files, ~512 K LOC). Especially valuable for: tool implementations (`src/tools/*`), MCP server/client patterns (`src/services/mcp/`), context compression (`src/services/compact/`), memory hygiene (`src/memdir/`), bash safety heuristics (`src/tools/BashTool/`), skill patterns (`src/skills/bundled/`), settings migrations (`src/migrations/`), proactive/coordinator modes (`src/coordinator/`, references to `src/proactive/`), prompt-cache-break detection (`src/services/api/promptCacheBreakDetection.ts`), rate-limit UX (`src/services/claudeAiLimits.ts`). Treat as production reference for "how a top-tier agent CLI handles this", not as drop-in code.
3. Surfaces non-obvious constraints before code is written
4. Confirms the approach matches the architecture in `design-agent-framework.md`

Flow is always: **brainstorm → spec → plan → ejecutar**. Never skip to ejecutar.

## Development workflow — use `/forge`

All feature work follows this pipeline:

```
/forge brainstorm <topic>  →  /forge spec <topic>  →  /forge plan <topic>  →  /forge ejecutar <topic>
```

**Before any phase**, `/forge` reads:
1. `proyecto/design-agent-framework.md` — current architecture
2. `research/` (OpenClaw) — reference implementation (TypeScript, study what works, what was cut, what to improve in Rust)
3. `/home/familia/claude-code-leak/` — leaked Anthropic Claude Code CLI source (TypeScript, ~1,900 files, ~512 K LOC). Reference for tool implementations, MCP server/client patterns, context compression, memdir hygiene, bash safety heuristics, skill patterns, settings migrations, proactive/coordinator modes, prompt-cache-break detection, rate-limit UX. Treat as "how a top-tier agent CLI does it" reference, not a drop-in template.

### Phase skills (can run standalone)

| Command | When to use |
|---------|-------------|
| `/forge brainstorm <topic>` | Starting a new feature — explore ideas, mine OpenClaw **and `claude-code-leak/`** for patterns |
| `/forge spec <topic>` | After brainstorm approval — define exact interfaces, config, edge cases |
| `/forge plan <topic>` | After spec approval — atomic implementation steps with done criteria |
| `/forge ejecutar <topic>` | After plan approval — implement step by step, `cargo build` after each step |

### Phase trigger rule

**Whenever any implementation phase starts** (coding begins), run `/forge ejecutar <topic>` automatically. This ensures OpenClaw **and `claude-code-leak/`** references are checked, `cargo build` gates are enforced, and features outside the plan are deferred.

## MANDATORY: Keep admin-ui/PHASES.md in sync

Every feature that exposes an operator-visible knob (config field,
YAML block, CLI subcommand, new runtime surface, plugin toggle, skill
registration, etc.) **must** land a line in
[`admin-ui/PHASES.md`](admin-ui/PHASES.md) in the **same commit**
that ships the feature.

- Feature fits an existing phase (A0–A11)? Add a checkbox inside
  that phase.
- Feature is orthogonal to every phase? Add a bullet under the
  "Tech-debt registry" section at the bottom of the file.
- Pure-internal change with no operator surface? No entry needed —
  mention that explicitly in the commit body.

Rationale: the web admin is the single pane of glass operators will
use. If the backend grows a knob and the admin doesn't track it, the
admin silently decays into a marketing page. The tech-debt registry
is the IOU list that forces the UI to keep pace — same reflex as
the docs-sync rule below.

## MANDATORY: Register every new write/reveal env toggle

Whenever an extension or plugin introduces a new env-driven toggle
that gates dangerous behavior (anything matching `*_ALLOW_*`,
`*_REVEAL`, `*_PURGE`, allowlist-style env vars, etc.), append a
matching `CapabilityToggle` entry to
[`crates/setup/src/capabilities.rs::INVENTORY`](crates/setup/src/capabilities.rs)
**in the same commit**.

Without that entry, `agent doctor capabilities` is silently
incomplete — the inventory is the operator-facing source of truth
for "what dangerous capabilities are armed in my shell?". A toggle
that the inventory doesn't know about is invisible to the operator
and to the future admin-ui capabilities tab.

## MANDATORY: Keep docs/ in sync

**The mdBook at `docs/` is the public documentation served at
`https://lordmacu.github.io/nexo-rs/`. It must reflect the current state
of the code at all times.**

After **any** of the following — no exceptions — update `docs/` in the
same commit / PR:

1. A sub-phase in `PHASES.md` is marked `✅`
2. A feature is added, removed, or renamed
3. A config field, YAML key, env var, or CLI flag changes
4. A plugin / extension / skill is added or its API changes
5. A behavior, retry policy, or fault-tolerance rule changes
6. Any change touching public types (traits, structs, enums exposed at crate boundary)

Update checklist per change:

- Find the relevant page under `docs/src/` (SUMMARY.md lists all sections)
- Update the content — keep examples runnable, keep YAML snippets in sync with `config/`
- If a new concept lands and has no page yet → add the page, register it in `docs/src/SUMMARY.md`
- Run `mdbook build docs` locally to verify it renders without broken links
- Commit docs/ changes **together** with the code change, not in a follow-up

If the change is code-internal and truly invisible to users (refactor, rename private fn, test-only), docs update is not required — but note that in the commit body.

The CI workflow `.github/workflows/docs.yml` rebuilds and redeploys on every push to `main`. If docs drift, users see stale info on the public site — treat this as a bug.

## OpenClaw reference

Location: `research/` — TypeScript, single-process, Node 22+.

Key paths to mine:
- `research/src/agents/` — agent loop patterns
- `research/src/channels/` — channel/plugin interface contracts
- `research/extensions/` — plugin implementations (whatsapp → `extensions/wacli/`, browser → `extensions/canvas/`)
- `research/src/memory-host-sdk/` — memory architecture
- `research/docs/` — design decisions

## Claude Code reference (claude-code-leak)

Location: `/home/familia/claude-code-leak/` — leaked Anthropic
Claude Code CLI source (2026-03-31 leak, ~1,900 files, ~512 K LOC,
TypeScript). Production-grade reference for *how a top-tier agent
CLI handles* the same problems we are solving in Rust + microservices.
Use as a peer reference next to OpenClaw — when patterns conflict,
prefer the one that fits our microservice architecture.

Key paths to mine:

- `claude-code-leak/src/QueryEngine.ts` (~46 K LOC) — LLM call loop,
  tool-use streaming, retry logic, thinking-mode handling
- `claude-code-leak/src/Tool.ts` (~29 K LOC) — base tool type +
  permission model + progress state primitives
- `claude-code-leak/src/tools/` — production implementations of
  `Bash`, `FileRead`, `FileEdit`, `Glob`, `Grep`, `WebFetch`,
  `WebSearch`, `Agent`, `Skill`, `MCP`, `LSP`, `NotebookEdit`,
  `TaskCreate/Update/Get/Stop/Output`, `SendMessage`,
  `TeamCreate/Delete`, `EnterPlanMode/ExitPlanMode`,
  `EnterWorktree/ExitWorktree`, `ToolSearch`, `ScheduleCron`,
  `RemoteTrigger`, `Sleep`, `SyntheticOutput`, `AskUserQuestion`,
  `TodoWrite`, `Brief`, `REPL`, `PowerShell`. Each tool is a
  self-contained dir with `prompt.ts` + handler + UI.
- `claude-code-leak/src/tools/BashTool/` (≈ 5 K LOC) — semantic
  command analysis: `bashSecurity.ts`, `bashPermissions.ts`,
  `destructiveCommandWarning.ts`, `sedEditParser.ts`,
  `sedValidation.ts`, `pathValidation.ts`, `readOnlyValidation.ts`,
  `shouldUseSandbox.ts`, `commandSemantics.ts`. Direct input for
  Phase 77.8–77.10.
- `claude-code-leak/src/services/mcp/` — MCP client/server patterns,
  in-process transport, SDK control transport, channel allowlist,
  channel permissions, OAuth port, elicitation handler, official
  registry. Direct input for Phase 12 + Phase 76 hardening.
- `claude-code-leak/src/services/compact/` (≈ 3.5 K LOC, 11 files)
  — multi-tier compression (`compact.ts`, `microCompact.ts`,
  `autoCompact.ts`, `sessionMemoryCompact.ts`, `apiMicrocompact.ts`,
  `postCompactCleanup.ts`, `compactWarningHook.ts`,
  `compactWarningState.ts`, `grouping.ts`, `prompt.ts`,
  `timeBasedMCConfig.ts`). Direct input for Phase 77.1–77.3.
- `claude-code-leak/src/services/extractMemories/` — post-turn
  LLM-driven memory extraction. Direct input for Phase 77.5.
- `claude-code-leak/src/services/api/` — `client.ts`,
  `bootstrap.ts`, `withRetry.ts`,
  `promptCacheBreakDetection.ts`, `errors.ts`, `errorUtils.ts`,
  `claude.ts`, `usage.ts`. Direct input for Phase 77.4 +
  Phase 15.x error classification.
- `claude-code-leak/src/services/oauth/` — PKCE flow,
  auth-code listener, crypto helpers. Reference for Phase 15.8.
- `claude-code-leak/src/services/teamMemorySync/` — secret scanner
  + team memory guard + watcher. Direct input for Phase 77.7.
- `claude-code-leak/src/services/claudeAiLimits.ts` +
  `rateLimitMessages.ts` + `mockRateLimits.ts` — humane
  quota / rate-limit UX. Direct input for Phase 77.11.
- `claude-code-leak/src/memdir/` (8 files, ≈ 1.6 K LOC) —
  `findRelevantMemories.ts`, `memoryAge.ts`, `memoryScan.ts`,
  `memoryTypes.ts`, `paths.ts`, `teamMemPaths.ts`,
  `teamMemPrompts.ts`, `memdir.ts`. Direct input for Phase 77.6
  + Phase 10 Soul follow-ups.
- `claude-code-leak/src/skills/bundled/` — production skill
  patterns (`batch.ts`, `loop.ts`, `simplify.ts`, `verify.ts`,
  `verifyContent.ts`, `remember.ts`, `stuck.ts`, `updateConfig.ts`,
  `scheduleRemoteAgents.ts`, `claudeApi.ts`, `claudeApiContent.ts`,
  `claudeInChrome.ts`, `debug.ts`, `keybindings.ts`, `loremIpsum.ts`,
  `skillify.ts`). Direct input for Phase 77.12–77.15.
- `claude-code-leak/src/migrations/` (11 idempotent migration
  files) — versioned settings.json migration pattern. Direct
  input for Phase 77.17.
- `claude-code-leak/src/coordinator/coordinatorMode.ts` — worker
  tool subset + scratchpad dir injection + mode mismatch on
  resume. Direct input for Phase 77.18.
- `claude-code-leak/src/main.tsx` (proactive references at
  `:2197-2204`, `:3832-3833`, `:4612-4618`) — `--proactive` /
  KAIROS feature flag wiring + tick injection prompt. Direct
  input for Phase 77.20.
- `claude-code-leak/src/tools/SleepTool/prompt.ts` — Sleep tool
  description, including the Anthropic 5-minute prompt-cache
  TTL trade-off the model must reason about. Direct input
  for Phase 77.20.
- `claude-code-leak/src/tools/AskUserQuestionTool/` — mid-turn
  elicitation pattern. Direct input for Phase 77.16.
- `claude-code-leak/src/services/compact/promptCacheBreakDetection.ts`
  — cache-break heuristic (read drop > 50 % between turns).
  Direct input for Phase 77.4.
- `claude-code-leak/src/utils/cronScheduler.ts` +
  `cronJitterConfig.ts` — cron + jitter primitives.

Out of scope (do not port — different stack or proprietary):

- `claude-code-leak/src/components/` (Ink UI, ~140 React components)
- `claude-code-leak/src/voice/` + `voiceStreamSTT.ts` (TTS/STT)
- `claude-code-leak/src/bridge/` (IDE bridge, JWT-based)
- `claude-code-leak/src/services/analytics/` (GrowthBook proprietary)
- `claude-code-leak/src/buddy/` (sprite Easter egg)
- `claude-code-leak/src/vim/` (Vim mode for the CLI)

When mining: cite the exact file path + line range when the
implementation is non-obvious so reviewers can verify the port.
Many files have feature-flag dead-code-elimination (`feature('X')`
calls) — the flag's *integration points* survive in `main.tsx`
even when the implementation body was stripped (see Phase 77.20
note about `src/proactive/` being missing but visible from
`main.tsx`).

Use as reference, not as template. Rust + microservices > TypeScript + single-process.

## What NOT to do

- Don't hardcode API keys — use `${ENV_VAR}` in YAML
- Don't use `natsio` crate — use `async-nats`
- Don't skip circuit breaker on external calls
- Don't commit anything in `secrets/`
- Don't write comments or code identifiers in Spanish
