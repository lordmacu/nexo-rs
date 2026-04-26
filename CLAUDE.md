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
| 15 | Claude subscription auth | 15.1 config schema, 15.2 anthropic_auth module, 15.3 CLI credentials reader, 15.4 AnthropicClient wiring, 15.5 setup wizard, 15.6 error classification, 15.7 docs, 15.8 OAuth browser PKCE flow | 8/8 ✅ |
| 16 | Per-binding capability override | 16.1 schema, 16.2 EffectiveBindingPolicy, 16.3 boot validation, 16.4 AgentContext + registry cache, 16.5 runtime intake + rate limiter, 16.6 LLM/prompt/skills/outbound/delegation, 16.7 YAML example + e2e tests | 7/7 ✅ |
| 17 | Per-agent credentials (WA/TG/Google) | 17.1 nexo-auth scaffold, 17.2 boot gauntlet, 17.3 per-channel stores, 17.4 resolver, 17.5 telemetry, 17.6 config schemas, 17.7 `--check-config`, 17.8 runtime integration, 17.9 plugin tool migration, 17.10 google tool store lookup, 17.11 e2e + fingerprint stability | 11/11 ✅ |
| 18 | Config hot-reload | 18.1 deps+schema, 18.2 RuntimeSnapshot+ArcSwap, 18.3 ReloadCommand channel, 18.4 file watcher, 18.5 coordinator, 18.6 intake migration, 18.7 telemetry, 18.8 CLI+boot wiring, 18.9 tests | 9/9 ✅ |
| 20 | `agent_turn` poller | 20.1 PollContext.llm_*, 20.2 with_llm builder, 20.3 agent_turn builtin (cron-style scheduled LLM turn → channel), 20.4 tests + example YAML | 4/4 ✅ |
| 70 | Pairing/Dispatch DX cleanup | 70.1 cody-prompt no-hallucination, 70.2 has_any_override fix, 70.3 `pair list --all`, 70.4 `[intake]`/`[dispatch]` error prefixes, 70.5 pair-start loopback fallback, 70.6 `setup doctor` pairing audit, 70.7 reload flushes gate caches, 70.8 docs sync | 8/8 ✅ |
| 71 | Agent registry persistence + shutdown drain | 71.1 wire `SqliteAgentRegistryStore`, 71.2 boot reattach (`MarkedLost` + notify_origin), 71.3 SIGTERM drain via `drain_running_goals` helper, 71.4 unit tests in `shutdown_drain` + reattach (full SIGTERM e2e deferred), 71.5 docs sync | 5/5 ✅ |
| 72 | Turn-level audit log | 72.1 `TurnLogStore` + `SqliteTurnLogStore`, 72.2 `EventForwarder.with_turn_log` writes per `AttemptResult`, 72.3 `agent_turns_tail` tool, 72.4 9 unit tests across the three modules, 72.5 docs sync | 5/5 ✅ |

**Progress: 210 implemented sub-phases / 5 deferred sub-phases tracked in `PHASES.md` (26.z tunnel.url, 26.aa pair_approve tool [security-review-gated], 26.ab pairing.yaml loader, 19.x pollers V2 backlog, 38.x flakes & races). Phase 67 (Claude Code self-driving agent) — 67.0–67.9 plus 67.A.1–67.H.6 shipped (foundation, spawn, binding, permission, loop, acceptance, git-worktree, memory, replay-policy, compact-policy; project-tracker, multi-agent registry, async dispatch, capability gate, completion hooks, query / control / admin tools, CLI espejo, NATS subjects, hot-reload-aware tool filter, docs); 67.10–67.13 backlogged (the original 67.10 'Escalación a WhatsApp/Telegram' is largely subsumed by the 67.F notify_origin / notify_channel hooks). Phase 68 (Local LLM tier — llama.cpp) — 15 sub-phases backlogged (transversal tier-0 inference for PII / embeddings / poller pre-filter / classifiers / fallback; **model-agnostic GGUF loader** — gemma3, qwen2.5, llama3.2, phi3, smolLM, etc. all swappable per-job via config — defaults gemma3-270m + bge-small via `llama-cpp-2`, target Termux ARM CPU + desktop CPU/GPU; complementary to Phase 46 which treats local LLM as primary agent provider). TaskFlow integration points threaded through 19.x (P-4 batch polls), 26.ac (companion-tui pairing), 68.15 (tier-0 batch / chunked / async patterns). Phase 21 (link understanding), Phase 25 (web_search), and Phase 26 (pairing + per-channel reply adapters) shipped. Phases 22–24 backlogged in `PHASES.md` (Slack/Discord plugins, realtime voice, image generation). `web_fetch` deferred — Phase 21 covers user-shared URLs. Pairing companion-tui deferred and direct in-process `Session::send_text` delivery for the challenge is deferred (adapters currently publish via broker on `plugin.outbound.{channel}`); store + gate + CLI + WA/TG `PairingChannelAdapter` impls + adapter registry all live. Detailed status of every deferred item lives in `proyecto/FOLLOWUPS.md`. Phase 69 (Setup wizard agent-centric submenu) shipped — per-agent dashboard inside `nexo setup → Configurar agente`, helpers in `yaml_patch` (read/upsert/remove/append-idempotent/remove-by-predicate), hot-reload trigger after every mutation. Phase 72 (Turn-level audit log) shipped — `nexo-agent-registry` gains a `TurnLogStore` trait + `SqliteTurnLogStore` (table `goal_turns`, idempotent on `(goal_id, turn_index)`, tail capped at 1000); `EventForwarder.with_turn_log(store)` appends a row per `AttemptResult` best-effort (warns on failure, never blocks the loop); new read tool `agent_turns_tail goal_id=<uuid> [n=20]` registered in `READ_TOOL_NAMES` returns a markdown table with one row per turn for post-mortem debugging that survives daemon restart. Phase 71 (Agent registry persistence + shutdown drain) shipped — `src/main.rs` now reads `agent_registry.store` and opens `SqliteAgentRegistryStore` (env placeholders resolved); boot honours `reattach_on_boot: true` to flip prior-run Running rows to `LostOnRestart` and fire `notify_origin` once per goal so the originating chat sees a clean `[abandoned]` closure; SIGTERM runs `nexo_dispatch_tools::drain_running_goals` BEFORE plugin teardown so `[shutdown]` notify_origin actually leaves the channel; per-hook 2 s timeout keeps stuck publishers from hanging shutdown; `DispatchToolContext` gains `hook_dispatcher: Option<Arc<dyn HookDispatcher>>`. Phase 70 (Pairing/Dispatch DX cleanup) shipped — Cody prompt forbids hallucinated tool denials; `binding_validate::has_any_override` now sees `dispatch_policy` / `pairing_policy` / `language` / `link_understanding` / `web_search` so dispatch-only bindings stop printing the no-overrides warn; `PairingStore::list_allow` + `nexo pair list --all [--include-revoked]` make seeded senders visible; every `DispatchDenied` and runtime pairing log is prefixed with `[dispatch]` / `[intake]` so the origin of a "trusted" denial is unambiguous; `nexo pair start` loopback fallback prints ready-to-run `pair seed` commands per configured plugin instance; `nexo setup doctor` audits `(channel, account_id)` tuples whose binding has `auto_challenge: true` but an empty allowlist; `ConfigReloadCoordinator::register_post_hook` flushes the `PairingGate` decision cache after every successful reload so newly-seeded senders take effect without a daemon restart.**

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
2. Surfaces non-obvious constraints before code is written
3. Confirms the approach matches the architecture in `design-agent-framework.md`

Flow is always: **brainstorm → spec → plan → ejecutar**. Never skip to ejecutar.

## Development workflow — use `/forge`

All feature work follows this pipeline:

```
/forge brainstorm <topic>  →  /forge spec <topic>  →  /forge plan <topic>  →  /forge ejecutar <topic>
```

**Before any phase**, `/forge` reads:
1. `proyecto/design-agent-framework.md` — current architecture
2. `research/` (OpenClaw) — reference implementation (TypeScript, study what works, what was cut, what to improve in Rust)

### Phase skills (can run standalone)

| Command | When to use |
|---------|-------------|
| `/forge brainstorm <topic>` | Starting a new feature — explore ideas, mine OpenClaw for patterns |
| `/forge spec <topic>` | After brainstorm approval — define exact interfaces, config, edge cases |
| `/forge plan <topic>` | After spec approval — atomic implementation steps with done criteria |
| `/forge ejecutar <topic>` | After plan approval — implement step by step, `cargo build` after each step |

### Phase trigger rule

**Whenever any implementation phase starts** (coding begins), run `/forge ejecutar <topic>` automatically. This ensures OpenClaw reference is checked, `cargo build` gates are enforced, and features outside the plan are deferred.

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

Use as reference, not as template. Rust + microservices > TypeScript + single-process.

## What NOT to do

- Don't hardcode API keys — use `${ENV_VAR}` in YAML
- Don't use `natsio` crate — use `async-nats`
- Don't skip circuit breaker on external calls
- Don't commit anything in `secrets/`
- Don't write comments or code identifiers in Spanish
