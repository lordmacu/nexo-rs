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

**Progress: 113 / 113 sub-phases done (0 deferred). All phases complete.**

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
- Conversations with Cristian in Spanish — code artifacts always English

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
