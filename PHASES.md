# Implementation Phases

Ordered execution plan. Each phase depends on the previous. Each sub-phase is a unit of work with clear done criteria.

Reference: `design-agent-framework.md` for architecture decisions.  
OpenClaw reference: `../research/` — study patterns, do not copy TypeScript directly.

## Progress

**Global: 0 / 68 sub-phases done**

| Phase | Done | Total |
|-------|------|-------|
| 1 — Core Runtime | 0 | 6 |
| 2 — NATS Broker | 0 | 6 |
| 3 — LLM Integration | 0 | 6 |
| 4 — Browser CDP | 0 | 6 |
| 5 — Memory | 0 | 5 |
| 6 — WhatsApp Plugin | 0 | 4 |
| 7 — Heartbeat | 0 | 3 |
| 8 — Agent-to-Agent | 0 | 3 |
| 9 — Polish | 0 | 6 |
| 10 — Soul, Identity & Learning | 0 | 8 |
| 11 — Extension System | 0 | 8 |
| 12 — MCP Support | 0 | 7 |

> After each sub-phase: mark ✅ here, update count in this table and in `../CLAUDE.md`.

---

## Phase 1 — Core Runtime

**Goal:** Agent process boots, loads config, receives a message, runs a no-op agent loop.

### 1.1 — Workspace scaffold ⬜
- Create `Cargo.toml` workspace with all crates declared
- Create each crate with empty `lib.rs` + `Cargo.toml`
- `cargo build --workspace` passes clean
- **Done:** no compile errors, all crates visible in workspace

### 1.2 — Config loading (`crates/config`) ⬜
- Load `config/agents.yaml`, `config/broker.yaml`, `config/llm.yaml`, `config/memory.yaml`
- Resolve `${ENV_VAR}` placeholders at load time
- Error clearly if required env var is missing
- Structs: `AgentConfig`, `BrokerConfig`, `LLMConfig`, `MemoryConfig`
- **Done:** `Config::load("config/")` returns populated structs; missing env var returns descriptive error

### 1.3 — Local event bus (`crates/broker/src/local.rs`) ⬜
- `tokio::mpsc`-based in-memory bus, no external dependency
- Implements `BrokerHandle` trait (same interface as NATS broker)
- `subscribe(topic, handler)`, `publish(topic, event)`, `request(topic, msg) -> Response`
- Topic pattern matching: `agent.events.*` wildcards
- **Done:** two tasks can pub/sub through local bus in integration test

### 1.4 — Session manager (`crates/core/src/session/`) ⬜
- `Session { id, agent_id, history, context, created_at, last_access }`
- `SessionManager`: create, get, update, expire (TTL from config)
- In-memory store for Phase 1 (SQLite persistence comes in Phase 5)
- **Done:** session created, updated, retrieved, expired after TTL in unit tests

### 1.5 — Agent skeleton (`crates/core/src/agent/`) ⬜
- `Agent` struct with `id`, `model: ModelConfig`, `plugins: Vec<Box<dyn Plugin>>`
- `AgentBehavior` trait: `on_message`, `on_event`, `on_heartbeat`, `decide`
- `AgentRuntime`: boots agent, subscribes to `plugin.inbound.{plugin}` topics, calls `on_message`
- No-op default implementation (echoes message back)
- **Done:** agent boots from YAML config, receives test message, calls `on_message`, no panic

### 1.6 — Plugin interface (`crates/core/src/agent/plugin.rs`) ⬜
- `Plugin` trait: `name()`, `start(broker)`, `stop()`, `send_command(cmd) -> Response`
- `Command` and `Response` enums (extensible)
- `PluginRegistry`: register by name, lookup, start all, stop all
- **Done:** mock plugin registers, starts, receives command, returns response

---

## Phase 2 — NATS Message Broker

**Goal:** Replace local bus with real NATS. Survive NATS restart without losing messages.

### 2.1 — NATS client (`crates/broker/src/nats.rs`) ⬜
- Connect to NATS via `async-nats`
- Implements same `BrokerHandle` trait as local bus — zero changes to callers
- Auth via nkey file (path from config)
- **Done:** pub/sub works end-to-end through real NATS server in integration test

### 2.2 — Broker abstraction (`crates/broker/src/lib.rs`) ⬜
- `BrokerHandle` enum: `Local(LocalBroker)` | `Nats(NatsBroker)`
- Selected at boot from `config/broker.yaml` `type` field
- **Done:** switching config between `local` and `nats` changes broker with no code change

### 2.3 — Persistent disk queue (`crates/broker/src/queue.rs`) ⬜
- Append-only write-ahead log at `./data/queue/`
- On publish: write to disk first, then forward to NATS
- On NATS reconnect: drain disk queue → publish pending → delete entries
- **Done:** kill NATS mid-run, restart, all pending events delivered

### 2.4 — Dead letter queue ⬜
- After `max_attempts` failures, move message to `agent.dlq.{topic}`
- DLQ persisted to SQLite (`./data/dlq.db`)
- CLI command to inspect/replay DLQ entries
- **Done:** 3 failed deliveries move message to DLQ; replay command redelivers

### 2.5 — Circuit breaker (`crates/core/src/circuit_breaker.rs`) ⬜
- States: `Closed` → `Open` → `HalfOpen` → `Closed`
- `failure_threshold`, `recovery_timeout` configurable per use site
- Wrap every external call site: NATS publish, LLM call, CDP command
- **Done:** inject failures in test, breaker opens, rejects, recovers after timeout

### 2.6 — Backpressure ⬜
- `EventBus` tracks pending-per-topic counter
- When `max_pending` exceeded: slow producers via `tokio::time::sleep` backoff
- **Done:** fast publisher + slow subscriber → publisher slows, no memory explosion

---

## Phase 3 — LLM Integration

**Goal:** Agent calls MiniMax, gets completion, parses tool calls, executes via ToolRegistry.

### 3.1 — LLM client trait (`crates/llm/src/client.rs`) ⬜
- `LLMClient` trait: `complete(prompt) -> Completion`, `stream(prompt, cb)`, `embed(text) -> Vec<f32>`
- `Prompt { system, messages, tools, max_tokens }`
- `Completion { content, tool_calls, usage: TokenUsage }`
- **Done:** trait defined, mock impl passes all method calls

### 3.2 — MiniMax client (`crates/llm/src/minimax.rs`) ← **start here** ⬜
- HTTP client via `reqwest` to `https://api.minimax.chat/v1`
- Auth: `Authorization: Bearer ${MINIMAX_API_KEY}` + `GroupId: ${MINIMAX_GROUP_ID}`
- `complete()` and `stream()` implementations
- Map MiniMax response format → `Completion` struct
- **Done:** real API call returns completion; token usage populated

### 3.3 — Rate limiter (`crates/llm/src/rate_limiter.rs`) ⬜
- Token bucket: `requests_per_second` from config
- On 429: exponential backoff (1s initial, 2x multiplier, 60s max)
- `QuotaTracker`: alert log when remaining tokens < threshold
- Wraps any `LLMClient` transparently
- **Done:** 10 rapid requests → rate limiter spaces them; 429 → backoff logged; quota alert fires

### 3.4 — OpenAI-compatible client (`crates/llm/src/openai.rs`) ⬜
- Covers OpenAI + Ollama (same API shape)
- Base URL configurable → points to `http://localhost:11434/v1` for Ollama
- **Done:** works against Ollama local instance with `llama3`

### 3.5 — Tool registry (`crates/core/src/tool_registry.rs`) ⬜
- `Tool` trait: `name()`, `description()`, `schema() -> JsonSchema`, `execute(args) -> Value`
- `ToolRegistry`: register, list (for LLM prompt injection), dispatch by name
- **Done:** register 2 tools, inject schemas into prompt, parse tool_call from completion, dispatch

### 3.6 — Agent LLM loop (`crates/core/src/agent/loop.rs`) ⬜
- On `on_message`: build prompt from session history + system prompt + tools
- Call LLM → parse completion
- If tool_calls: execute via ToolRegistry → append results → call LLM again
- Loop until no more tool_calls or max_iterations reached
- Append final response to session history
- **Done:** agent receives "what time is it?" → calls `time` tool → responds with time

---

## Phase 4 — Browser CDP Plugin

**Goal:** Agent can navigate, click, fill, screenshot via Chrome DevTools Protocol.

### 4.1 — CDP WebSocket client (`crates/plugins/browser/src/cdp/client.rs`) ⬜
- Connect to `http://127.0.0.1:9222/json` → discover targets
- Open WebSocket to target → send/receive CDP JSON-RPC
- Async: send command, await matching `id` in response stream
- **Done:** `Page.navigate` to URL, receive `frameStoppedLoading` event

### 4.2 — Chrome auto-discovery / launch ⬜
- Try connect to `cdp_url` from config
- If unreachable: spawn `google-chrome --remote-debugging-port=9222 --headless`
- Retry connect up to 10s
- **Done:** no Chrome running → plugin launches it → connects

### 4.3 — Element reference system ⬜
- After `DOM.getDocument`: assign stable refs `@e1`, `@e2` to interactive elements
- `ElementRegistry`: maps ref → `nodeId`, tracks lifecycle (removed on navigation)
- **Done:** snapshot returns elements with refs; click `@e2` resolves to correct nodeId

### 4.4 — Command implementations ⬜
- `Navigate { url }` → `Page.navigate`
- `Click { selector }` → `DOM.querySelector` + `Input.dispatchMouseEvent`
- `Fill { selector, value }` → focus + `Input.insertText`
- `Screenshot` → `Page.captureScreenshot` → base64 bytes
- `Snapshot` → `DOM.getDocument` + element refs → structured tree
- `Evaluate { script }` → `Runtime.evaluate`
- `Wait { condition }` → poll or CDP event subscription
- **Done:** all commands tested against real Chrome in integration test

### 4.5 — Browser plugin event loop ⬜
- Subscribe to `plugin.outbound.browser` → execute commands
- Publish CDP events to `agent.events.{agent_id}.{event_type}`
- **Done:** agent publishes click command → plugin executes → agent receives result event

### 4.6 — Session persistence ⬜
- Save/restore Chrome profile from `user_data_dir`
- Multiple profiles (default / development) switchable via config
- **Done:** login to site in one run → restart → still logged in

---

## Phase 5 — Memory System

**Goal:** Agent remembers conversation history across restarts. Semantic recall works.

### 5.1 — Short-term memory (`crates/memory/src/short_term.rs`) ⬜
- `RollingWindow<Interaction>` capped at `max_history_turns` from config
- Per-session, in-process
- `context(session_id, max_turns)` → `Vec<Interaction>` for prompt building
- **Done:** 100 turns added, only last 50 returned; session isolated from other sessions

### 5.2 — SQLite store (`crates/memory/src/store.rs`) ⬜
- `sqlx` + SQLite at `./data/memory.db`
- Tables: `sessions`, `interactions`, `agent_facts`
- `store(session_id, interaction)` → insert
- `history(session_id, limit)` → ordered interactions
- **Done:** restart process, history survives; 1000 interactions inserted in <500ms

### 5.3 — Long-term memory (`crates/memory/src/long_term.rs`) ⬜
- `remember(agent_id, fact: &str)` → insert into `agent_facts`
- `recall(agent_id, query, limit)` → keyword search (full-text via SQLite FTS5)
- **Done:** store 20 facts, recall with partial keyword returns relevant ones

### 5.4 — Vector index (`crates/memory/src/vector.rs`) ⬜
- `sqlite-vec` extension loaded into same SQLite connection
- On `remember`: embed fact via LLM `embed()` → store vector
- `recall_semantic(query, limit)` → embed query → cosine similarity search
- **Done:** store "user likes jazz", recall with "music preferences" returns it top-1

### 5.5 — Memory tool ⬜
- Register `MemoryTool` in `ToolRegistry`
- Tools: `remember(fact)`, `recall(query)`, `list_facts()`
- Agent uses via tool calling loop
- **Done:** agent receives "remember I like jazz" → calls `remember` tool → persisted; next session recall works

---

## Phase 6 — WhatsApp Plugin

**Goal:** Agent receives and sends WhatsApp messages via `whatsapp-rs`.

### 6.1 — Audit `whatsapp-rs` public API ⬜
- Read `../whatsapp-rs/src/lib.rs` — what is `pub`
- Read `../whatsapp-rs/src/messages/send.rs` and `recv.rs`
- Read `../whatsapp-rs/src/auth/` — session lifecycle
- Document: send function signatures, recv event types, session init flow
- **Done:** list of pub functions + event types written in `docs/whatsapp-rs-api.md`

### 6.2 — WhatsApp plugin wrapper (`crates/plugins/whatsapp/src/lib.rs`) ⬜
- Wrap `whatsapp-rs` as `Plugin` trait implementation
- `start()`: init `whatsapp-rs` session (load credentials or QR pair)
- Recv loop: translate `whatsapp-rs` events → publish to `plugin.inbound.whatsapp`
- `send_command(SendText { to, text })` → call `whatsapp-rs` send function
- **Done:** real WhatsApp message received → event on broker; agent response → sent via WhatsApp

### 6.3 — Session persistence ⬜
- Credentials stored in path from `config/plugins/whatsapp.yaml` `session_dir`
- On restart: load existing credentials, skip QR
- On credential expiry: re-trigger QR flow, notify agent
- **Done:** restart plugin → reconnects without QR; expired session → QR event published

### 6.4 — Media support ⬜
- `SendMedia { to, path, caption }` → `whatsapp-rs` media send
- Inbound media: download to `./data/media/`, publish path in event
- **Done:** send image, receive image with local path

---

## Phase 7 — Heartbeat Scheduler

**Goal:** Agents fire `on_heartbeat()` on interval. Proactive behavior works.

### 7.1 — Heartbeat runtime (`crates/core/src/heartbeat.rs`) ⬜
- Per-agent `tokio::interval` from `heartbeat.interval` config
- On tick: publish `agent.events.{agent_id}.heartbeat`
- `AgentRuntime` subscribes, calls `on_heartbeat(ctx)`
- **Done:** agent configured with 5s interval → `on_heartbeat` called every 5s

### 7.2 — Default heartbeat behaviors ⬜
- Check pending reminders in memory → send proactive message if due
- Log heartbeat (debug level) with agent id + timestamp
- **Done:** store reminder at T+10s → heartbeat fires → message sent at T+10s

### 7.3 — Heartbeat tool ⬜
- `schedule_reminder(at: DateTime, message: &str)` tool
- Stored in `agent_facts` with type `reminder`
- Heartbeat checks and fires
- **Done:** agent called with "remind me in 1 minute to drink water" → fires correctly

---

## Phase 8 — Agent-to-Agent Routing

**Goal:** Agent A can delegate tasks to Agent B and receive results.

### 8.1 — Routing protocol ⬜
- Topic: `agent.route.{target_id}`
- Message: `AgentMessage { from, to, correlation_id, payload: AgentPayload }`
- `AgentPayload`: `Delegate { task, context }` | `Result { task_id, output }` | `Broadcast { event, data }`
- **Done:** struct definitions + serde round-trip test

### 8.2 — Routing in AgentRuntime ⬜
- Subscribe to `agent.route.{self.id}` on boot
- On `Delegate`: run agent loop with delegated task, publish `Result` back
- On `Result`: match by `correlation_id`, resume waiting caller
- **Done:** agent A delegates to agent B in integration test, receives result

### 8.3 — Delegation tool ⬜
- `delegate(agent_id: &str, task: &str) -> Value` tool
- Registered in ToolRegistry, callable by LLM via tool calling
- Waits for result with configurable timeout
- **Done:** LLM calls delegate tool → routes to agent B → returns result to LLM

---

## Phase 9 — Observability & Polish

**Goal:** Production-ready: logs, metrics, health checks, graceful shutdown, Docker.

### 9.1 — Structured logging ⬜
- `tracing` + `tracing-subscriber` with JSON formatter in production
- Log levels: ERROR (panics/unrecoverable), WARN (retry/circuit breaker), INFO (lifecycle), DEBUG (message flow)
- Span per agent message: `agent_id`, `session_id`, `message_id`
- **Done:** all log lines have structured fields; `RUST_LOG=info` shows clean output

### 9.2 — Metrics (Prometheus) ⬜
- `metrics` + `metrics-exporter-prometheus` crates
- Track: `llm_requests_total`, `llm_latency_ms`, `messages_processed_total`, `circuit_breaker_state`
- Expose at `http://0.0.0.0:9090/metrics`
- **Done:** `/metrics` returns valid Prometheus text format with all counters

### 9.3 — Health check endpoints ⬜
- HTTP server (minimal, `axum`) on port 8080
- `GET /health` → 200 if process alive
- `GET /ready` → 200 if broker connected + at least one agent running
- **Done:** Docker HEALTHCHECK passes; `/ready` returns 503 when NATS is down

### 9.4 — Graceful shutdown ⬜
- Handle `SIGTERM` and `SIGINT`
- On signal: stop accepting new messages → drain in-flight → flush memory store → stop plugins → exit 0
- Timeout: 30s max drain before force exit
- **Done:** `kill -TERM <pid>` → no messages lost; exits within 30s

### 9.5 — Docker Compose ⬜
- Services: `nats`, `agent`, `chrome` (browserless)
- Agent image: multi-stage build (builder → runtime)
- Secrets via Docker secrets files (not env vars in compose file)
- Health checks on all services
- Volume mounts: `./config`, `./data`, `./secrets`
- **Done:** `docker compose up` → all services healthy; `docker compose down && up` → state persists

### 9.6 — Integration test suite ⬜
- Test: full message flow WhatsApp → agent LLM loop → WhatsApp reply
- Test: browser navigate + screenshot
- Test: NATS restart recovery
- Test: circuit breaker open/recover
- Test: agent-to-agent delegation
- **Done:** `cargo test --workspace` green; `docker compose -f docker-compose.test.yml up` green

---

## Phase 10 — Soul, Identity & Continuous Learning

**Goal:** Agent has a persistent personality, feels human, and genuinely learns over time from interactions — not just stores facts but synthesizes them into durable knowledge like OpenClaw's dreaming system.

OpenClaw reference:
- `research/docs/concepts/soul.md` — SOUL.md philosophy and pitfalls
- `research/docs/reference/templates/SOUL.md` — canonical template
- `research/extensions/memory-core/src/dreaming.ts` — dreaming sweep engine
- `research/extensions/memory-core/src/dreaming-phases.ts` — Light Sleep / REM phases
- `research/extensions/memory-core/src/short-term-promotion.ts` — recall-signal-based promotion
- `research/src/agents/identity.ts` — name, emoji, ack reaction, per-channel prefix
- `research/src/agents/identity.human-delay.test.ts` — human delay config and merge logic

### 10.1 — Agent identity system ⬜

Each agent has a persistent identity beyond just a system prompt.

```rust
pub struct AgentIdentity {
    pub name: String,
    pub emoji: Option<String>,
    pub ack_reaction: String,        // reaction sent on message receipt (e.g. "👀")
    pub message_prefix: Option<String>, // e.g. "[kate]" in multi-agent channels
    pub human_delay: Option<HumanDelayConfig>,
}

pub struct HumanDelayConfig {
    pub mode: HumanDelayMode,
    pub min_ms: u64,
    pub max_ms: u64,
}

pub enum HumanDelayMode {
    Natural,   // random within [min, max], scaled by message length
    Fixed,     // always min_ms
    Off,
}
```

Config (YAML):
```yaml
agent:
  id: "kate"
  identity:
    name: "Kate"
    emoji: "🐱"
    ack_reaction: "👀"
    message_prefix: null     # null = auto from name in multi-agent channels
    human_delay:
      mode: "natural"
      min_ms: 800
      max_ms: 3000
```

- On message receipt: send `ack_reaction` immediately before processing
- On response: wait `human_delay` before sending (scaled by char count if `natural`)
- Per-channel prefix resolution: channel config → agent config → name fallback
- **Done:** kate receives message → 👀 reaction appears → delay fires → response sent with natural timing

### 10.2 — SOUL.md — agent personality file ⬜

Each agent workspace has a `SOUL.md` injected at session start as high-priority system context.

**File location:** `agents/{agent_id}/SOUL.md`

**What goes in SOUL.md:**
- Tone (blunt, warm, dry humor, etc.)
- Opinions and preferences (strong > hedgy)
- Brevity rules
- Boundaries
- What the agent never says (no "Great question!", no corporate filler)
- One-liners that define character

**What does NOT go in SOUL.md:**
- Life story or backstory narrative
- Operational rules (those go in `AGENTS.md` or system prompt)
- Security policies
- Vague vibes with no behavioral effect

**Injection:** `AgentRuntime` prepends `SOUL.md` content to system prompt at session start. SOUL.md is re-read on each session — changes take effect immediately.

**Agent can evolve its own SOUL.md:**
- Tool: `update_soul(section: &str, new_content: &str)` — agent can rewrite sections
- On change: log diff, notify user (e.g. "I updated my personality — here's what changed")
- Versioned via git if workspace is a repo

- **Done:** kate's SOUL.md injected → responses match defined tone; `update_soul` tool rewrites section; restart preserves change

### 10.3 — MEMORY.md — persistent self-knowledge ⬜

`MEMORY.md` is the agent's durable long-term memory, distinct from conversation history.

**File location:** `agents/{agent_id}/MEMORY.md`

**Content:** synthesized facts the agent has learned about the user, context, preferences, recurring patterns — NOT raw conversation logs.

**Format (structured sections):**
```markdown
# Memory

## About Cristian
- Prefers concise answers, no filler
- Works late nights, active ~10pm–2am
- Uses MiniMax as primary LLM

## Recurring topics
- WhatsApp integration questions (high frequency)
- Bible video automation

## Preferences
- Rust over TypeScript for performance work
- Docker for all deployments

## Last updated: 2026-04-22
```

**Injection:** `AgentRuntime` injects `MEMORY.md` into system prompt context on session start (after SOUL.md, before conversation history).

**Agent can write to MEMORY.md:**
- Tool: `update_memory(section: &str, fact: &str)` — appends or updates fact in section
- Agent decides when to write (after learning something new and durable)
- **Done:** kate told "I prefer dark mode" → writes to MEMORY.md → next session recalls it without being told

### 10.4 — Session transcripts ⬜

All conversations stored as structured JSONL for dreaming input.

```rust
// agents/{agent_id}/sessions/{session_id}.jsonl
pub enum TranscriptEntry {
    Session { id: Uuid, timestamp: DateTime },
    Message { role: Role, timestamp: DateTime, content: String },
    ToolCall { name: String, args: Value, result: Value },
}
```

- Written on every message exchange (append-only)
- Retention: configurable days (default 30)
- Used as input for dreaming sweep (Phase 10.6)
- **Done:** full conversation written to JSONL; file readable and parseable after restart

### 10.5 — Recall signal tracking ⬜

Tracks which facts are recalled frequently — foundation for dreaming promotion.

```rust
pub struct RecallSignal {
    pub fact_id: Uuid,
    pub query: String,
    pub recalled_at: DateTime,
    pub session_id: Uuid,
}

pub struct RecallStats {
    pub fact_id: Uuid,
    pub recall_count: u32,
    pub unique_queries: u32,
    pub last_recalled: DateTime,
    pub score: f32,   // weighted: recency + frequency + query diversity
}
```

- Every `recall(query)` call logs a `RecallSignal`
- `score = frequency_weight * recency_decay * unique_query_bonus`
- Facts with `score > threshold` are candidates for dreaming promotion
- **Done:** recall same fact 3x with different queries → score above threshold → appears in promotion candidates

### 10.6 — Dreaming — memory consolidation sweep ⬜

Inspired directly by OpenClaw's dreaming system. A scheduled LLM sweep that synthesizes recent sessions and high-recall facts into durable `MEMORY.md` entries.

**Phases (matching OpenClaw's Light Sleep → REM model):**

**Light Sleep** — fast pattern extraction:
- Input: recent session transcripts + recall signals from last N days
- LLM prompt: "What patterns, preferences, or facts appeared repeatedly? List them."
- Output: `agents/{agent_id}/dreaming/light/{date}.md` — raw candidates
- Threshold: `min_recall_count = 3`, `min_unique_queries = 2`

**REM Sleep** — deep synthesis:
- Input: Light Sleep report + existing MEMORY.md
- LLM prompt: "Which of these candidates are genuinely durable knowledge? Synthesize into MEMORY.md format."
- Output: `agents/{agent_id}/dreaming/rem/{date}.md` — promoted entries
- Threshold: `min_score = 0.6`

**Promotion:**
- Apply REM report → merge into `MEMORY.md`
- Deduplicate against existing entries
- Log what was promoted

```rust
pub struct DreamingConfig {
    pub enabled: bool,
    pub cron: String,              // e.g. "0 3 * * *" — 3am daily
    pub timezone: String,
    pub light_sleep: LightSleepConfig,
    pub rem_sleep: RemSleepConfig,
}

pub struct LightSleepConfig {
    pub min_recall_count: u32,     // default: 3
    pub min_unique_queries: u32,   // default: 2
    pub lookback_days: u32,        // default: 7
}

pub struct RemSleepConfig {
    pub min_score: f32,            // default: 0.6
    pub max_promotions_per_run: u32, // default: 10
    pub recency_half_life_days: f32, // default: 14.0 — older signals decay
}
```

Config (YAML):
```yaml
agent:
  id: "kate"
  dreaming:
    enabled: true
    cron: "0 3 * * *"
    timezone: "America/Bogota"
    light_sleep:
      min_recall_count: 3
      min_unique_queries: 2
      lookback_days: 7
    rem_sleep:
      min_score: 0.6
      max_promotions_per_run: 10
      recency_half_life_days: 14.0
```

Scheduled via the Heartbeat cron system (Phase 7). Uses the LLM client (Phase 3). Reads session transcripts (Phase 10.4) and recall signals (Phase 10.5).

- **Done:** run dreaming sweep → Light Sleep report written → REM report written → fact promoted to MEMORY.md → next session agent recalls fact without being reminded

### 10.7 — Concept vocabulary ⬜

Inspired by `research/extensions/memory-core/src/concept-vocabulary.ts`.

Extracts a personal vocabulary of recurring concepts from MEMORY.md and session transcripts — improves semantic search quality.

```rust
pub struct ConceptVocabulary {
    concepts: HashMap<String, ConceptEntry>,
}

pub struct ConceptEntry {
    pub term: String,
    pub aliases: Vec<String>,      // e.g. "WhatsApp" → ["wa", "whatsapp", "wapp"]
    pub frequency: u32,
    pub last_seen: DateTime,
}
```

- Rebuilt after each dreaming sweep
- Injected into vector search at recall time: expand query with known aliases
- **Done:** "wa integration" query finds results tagged "WhatsApp" via alias expansion

### 10.8 — Agent self-report ⬜

Agent can describe its own state on demand.

Tools:
- `who_am_i()` → returns SOUL.md summary + identity fields
- `what_do_i_know()` → returns MEMORY.md sections
- `my_stats()` → session count, facts stored, last dream date, recall signals this week

- **Done:** user asks "what do you remember about me?" → agent reads MEMORY.md → responds with structured summary

---

---

## Phase 11 — Extension System (Plug-and-Play)

**Goal:** Third-party extensions can be added without recompiling the agent. Any language. Drop-in manifest, auto-discovered, registered at runtime.

OpenClaw reference:
- `research/src/plugins/manifest.ts` — manifest schema + capability types
- `research/src/plugins/discovery.ts` — filesystem scan + candidate detection
- `research/src/plugins/tools.ts` — tool registration with plugin metadata
- `research/extensions/AGENTS.md` — boundary rules between core and extensions
- `research/src/plugin-sdk/` — public contract between core and plugins

### Architecture

Two-tier plugin model (learned from OpenClaw's evolution):

| Tier | Where | Language | Loaded | Use for |
|------|-------|----------|--------|---------|
| **Native** | `crates/plugins/` | Rust | Compiled in | Core integrations (WhatsApp, Browser, Telegram) |
| **Extension** | `extensions/` | Any | Runtime, external process | Community plugins, custom tools, LLM providers |

Extensions communicate with the agent via **NATS** (already in the stack) or **stdio JSON-RPC** (fallback for simple cases). No ABI issues, no dynamic `.so` linking, any language works.

### 11.1 — Extension manifest (`plugin.toml`) ⬜

Each extension declares its identity and capabilities in a `plugin.toml`:

```toml
[plugin]
id = "my-weather"
name = "Weather Tool"
version = "0.1.0"
description = "Adds real-time weather lookup to agents"
min_agent_version = "0.1.0"

[capabilities]
tools = ["get_weather", "get_forecast"]
hooks = ["before_message"]        # optional lifecycle hooks
channels = []                     # if it adds a new messaging channel
providers = []                    # if it adds an LLM provider

[transport]
kind = "stdio"                    # "stdio" | "nats" | "http"
command = "./my-weather"          # executable to spawn (stdio mode)
# -- OR --
# kind = "nats"
# subject_prefix = "ext.my-weather"   # NATS mode: agent publishes here

[meta]
author = "Cristian García"
license = "MIT"
```

- **Done:** `plugin.toml` parses into `ExtensionManifest` struct; invalid fields return descriptive error

### 11.2 — Extension discovery ⬜

Agent scans `./extensions/` on boot for subdirectories containing `plugin.toml`.

```rust
pub struct ExtensionDiscovery {
    pub search_paths: Vec<PathBuf>,   // default: ["./extensions"]
    pub ignore_dirs: Vec<String>,     // ["target", ".git", "node_modules"]
}

pub struct ExtensionCandidate {
    pub manifest: ExtensionManifest,
    pub root_dir: PathBuf,
    pub origin: ExtensionOrigin,      // Local | Installed
}

pub enum ExtensionOrigin {
    Local,                            // ./extensions/my-plugin/
    Installed { registry: String },   // future: downloaded from registry
}
```

Config (`config/extensions.yaml`):
```yaml
extensions:
  search_paths:
    - "./extensions"
  disabled:
    - "my-broken-plugin"    # disable without deleting
  allowlist: []             # empty = allow all discovered
```

- **Done:** drop `plugin.toml` into `./extensions/my-weather/` → agent discovers it on boot; disabled entry skipped

### 11.3 — Extension runtime (stdio transport) ⬜

Simplest transport: agent spawns extension as child process, communicates via stdin/stdout JSON-RPC.

**Protocol (JSON-RPC 2.0):**

```json
// Agent → Extension: handshake
{ "jsonrpc": "2.0", "method": "initialize", "params": { "agent_version": "0.1.0" }, "id": 1 }

// Extension → Agent: capabilities
{ "jsonrpc": "2.0", "result": { "tools": [...], "hooks": [...] }, "id": 1 }

// Agent → Extension: tool call
{ "jsonrpc": "2.0", "method": "tools/call", "params": { "name": "get_weather", "arguments": { "city": "Bogotá" } }, "id": 2 }

// Extension → Agent: result
{ "jsonrpc": "2.0", "result": { "content": "22°C, sunny" }, "id": 2 }
```

```rust
pub struct StdioExtensionRuntime {
    process: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    pending: HashMap<u64, oneshot::Sender<Value>>,
}
```

- Extension process kept alive for agent lifetime
- On crash: restart with backoff (circuit breaker from Phase 2)
- **Done:** spawn echo extension via stdio → `tools/call` roundtrip works; crash → restart logged

### 11.4 — Extension runtime (NATS transport) ⬜

For extensions that run as independent services (not spawned by agent).

```
Extension boots → subscribes to `ext.{id}.call.*`
Agent → publishes to `ext.{id}.call.get_weather` → extension handles → publishes result to reply subject
```

```rust
pub struct NatsExtensionRuntime {
    broker: BrokerHandle,
    subject_prefix: String,
    catalog: ExtensionToolCatalog,
}
```

- Extension registers itself by publishing to `ext.registry.announce` on boot
- Agent subscribes to `ext.registry.*` and adds extensions dynamically (hot-plug)
- **Done:** extension process started independently → agent discovers it via NATS announce → tools available

### 11.5 — Extension tool registration ⬜

Extensions expose tools that get merged into the agent's `ToolRegistry`.

```rust
pub struct ExtensionTool {
    pub name: String,
    pub description: String,
    pub input_schema: JsonSchema,
    pub plugin_id: String,          // for attribution in logs
    pub optional: bool,             // can agent run without this tool?
}

// ExtensionTool implements Tool trait — routes calls to extension runtime
impl Tool for ExtensionTool {
    async fn execute(&self, args: Value) -> Result<Value> {
        self.runtime.call(&self.name, args).await
    }
}
```

- Tools from extensions appear in LLM tool list alongside native tools
- Prefixed: `ext_{plugin_id}_{tool_name}` to avoid collision
- **Done:** weather extension tool appears in LLM prompt; LLM calls it; result returned

### 11.6 — Lifecycle hooks ⬜

Extensions can hook into agent lifecycle events, not just expose tools.

```rust
pub enum HookPoint {
    BeforeMessage,          // before agent processes incoming message
    AfterMessage,           // after agent sends response
    BeforeToolCall,         // before any tool executes
    AfterToolCall,          // after tool result
    OnHeartbeat,            // on each heartbeat tick
    OnSessionStart,
    OnSessionEnd,
}

pub struct HookRegistry {
    hooks: HashMap<HookPoint, Vec<ExtensionHookHandler>>,
}
```

Config (in `plugin.toml`):
```toml
[capabilities]
hooks = ["before_message", "after_tool_call"]
```

- Hooks called in registration order; any hook can modify context or abort
- **Done:** logging extension hooks `before_message` → every message logged to file; abort hook stops processing

### 11.7 — Extension CLI commands ⬜

Extensions can add CLI subcommands to the agent binary.

```
agent ext install ./extensions/my-weather
agent ext list
agent ext disable my-weather
agent ext enable my-weather
agent ext info my-weather
```

```rust
pub struct ExtensionCli {
    discovery: ExtensionDiscovery,
    config_path: PathBuf,
}
```

- `install`: copies/links extension dir, validates manifest, adds to config
- `list`: shows all discovered extensions with status (enabled/disabled/error)
- `disable`/`enable`: toggles in `config/extensions.yaml` without deleting
- **Done:** `agent ext list` shows table with id, version, status, tool count

### 11.8 — Extension template ⬜

Starter template for building a new extension, in Rust and in Python (two most likely languages).

**Rust template** (`extensions/template-rust/`):
```
plugin.toml
Cargo.toml
src/
  main.rs      # stdio JSON-RPC server loop
  tools.rs     # tool definitions
```

**Python template** (`extensions/template-python/`):
```
plugin.toml
main.py        # stdio JSON-RPC server loop
tools.py       # tool definitions
requirements.txt
```

- `agent ext new my-tool --lang rust` scaffolds from template
- Template includes: handshake, tool schema declaration, call handler, graceful shutdown
- **Done:** scaffold command creates working extension; `agent ext install` picks it up; tool callable from agent

---

## Phase 12 — MCP Support (Model Context Protocol)

**Goal:** Agent can connect to any MCP server as a tool/resource source, and optionally expose itself as an MCP server so other MCP clients can use it.

OpenClaw reference:
- `research/src/agents/mcp-transport-config.ts` — stdio + HTTP transport resolution
- `research/src/agents/mcp-transport.ts` — connection lifecycle
- `research/src/agents/mcp-http.ts` — HTTP/SSE transport
- `research/src/agents/pi-bundle-mcp-types.ts` — catalog types (`McpToolCatalog`, `SessionMcpRuntime`)
- `research/src/agents/pi-bundle-mcp-runtime.ts` — session-scoped MCP runtime manager
- `research/src/plugins/bundle-mcp.ts` — how plugins declare MCP servers

MCP crate: `rmcp` (official Rust SDK from modelcontextprotocol) or `mcp-client` crate.

### 12.1 — MCP client (stdio transport) ⬜

Connect to any MCP server that runs as a local process (most common case).

```rust
pub struct McpStdioClient {
    process: Child,
    transport: McpTransport,
    server_info: McpServerInfo,
}

pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub connection_timeout_ms: u64,   // default 30_000 (learned from OpenClaw)
}
```

Config (`config/mcp.yaml`):
```yaml
mcp:
  servers:
    filesystem:
      transport: stdio
      command: "npx"
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/home/familia"]
    postgres:
      transport: stdio
      command: "npx"
      args: ["-y", "@modelcontextprotocol/server-postgres"]
      env:
        POSTGRES_URL: "${POSTGRES_URL}"
    brave-search:
      transport: stdio
      command: "npx"
      args: ["-y", "@modelcontextprotocol/server-brave-search"]
      env:
        BRAVE_API_KEY: "${BRAVE_API_KEY}"
```

- **Done:** connect to `@modelcontextprotocol/server-filesystem` → list tools → agent can call `read_file`

### 12.2 — MCP client (HTTP/SSE transport) ⬜

Connect to remote MCP servers over HTTP (Streamable HTTP or legacy SSE).

```rust
pub struct McpHttpClient {
    url: Url,
    headers: HashMap<String, String>,
    transport_type: HttpTransportType,
}

pub enum HttpTransportType {
    StreamableHttp,   // modern: POST to /mcp, SSE response
    Sse,              // legacy: GET /sse + POST /messages
}
```

Config:
```yaml
mcp:
  servers:
    remote-agent:
      transport: http
      url: "https://my-mcp-server.com/mcp"
      headers:
        Authorization: "Bearer ${MCP_TOKEN}"
```

- Auto-detect transport type from server capabilities on connect
- **Done:** connect to remote HTTP MCP server → tools available → roundtrip call works

### 12.3 — MCP tool catalog ⬜

Each connected MCP server exposes tools. Catalog tracks all of them.

```rust
pub struct McpToolCatalog {
    pub version: u64,
    pub generated_at: DateTime,
    pub servers: HashMap<String, McpServerSummary>,
    pub tools: Vec<McpCatalogTool>,
}

pub struct McpCatalogTool {
    pub server_name: String,
    pub tool_name: String,
    pub description: Option<String>,
    pub input_schema: JsonSchema,
    pub prefixed_name: String,      // "mcp_{server}_{tool}" — avoids ToolRegistry collision
}
```

- Catalog built on connect, refreshed on reconnect
- Tools registered in `ToolRegistry` as `McpProxyTool` (routes calls to MCP server)
- **Done:** `filesystem` server connected → `mcp_filesystem_read_file` appears in agent tool list; LLM calls it

### 12.4 — Session-scoped MCP runtime ⬜

MCP connections are scoped per session (learned from OpenClaw's `SessionMcpRuntime`).

```rust
pub struct SessionMcpRuntime {
    pub session_id: Uuid,
    pub config_fingerprint: String,   // detect config changes → reconnect
    pub created_at: DateTime,
    pub last_used_at: DateTime,
    servers: HashMap<String, McpClientHandle>,
}

pub struct McpRuntimeManager {
    sessions: HashMap<Uuid, SessionMcpRuntime>,
}

impl McpRuntimeManager {
    pub async fn get_or_create(&self, session_id: Uuid) -> &SessionMcpRuntime;
    pub async fn call_tool(&self, session_id: Uuid, server: &str, tool: &str, input: Value) -> Result<Value>;
    pub async fn dispose(&self, session_id: Uuid);
}
```

- Config fingerprint: if `mcp.yaml` changes mid-session, runtime reconnects transparently
- **Done:** two sessions use same MCP server concurrently without interference; config reload reconnects

### 12.5 — MCP resources ⬜

MCP servers can expose Resources (files, DB rows, live data) in addition to tools.

```rust
pub struct McpResource {
    pub uri: String,             // e.g. "file:///home/familia/notes.md"
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}
```

- Agent can `list_resources(server)` and `read_resource(server, uri)`
- Resources injected into prompt context when relevant (via Memory system)
- **Done:** `filesystem` server resources listed; agent reads a file resource and uses content in response

### 12.6 — Agent as MCP server (optional) ⬜

Expose the agent itself as an MCP server so Claude Desktop, Cursor, or other MCP clients can use it.

```rust
pub struct AgentMcpServer {
    pub bind: String,           // "0.0.0.0:3000"
    pub transport: McpServerTransport,
    pub exposed_tools: Vec<String>,  // which agent tools to expose
}
```

Config:
```yaml
mcp_server:
  enabled: true
  bind: "127.0.0.1:3000"
  transport: streamable_http
  expose_tools:
    - "browser_navigate"
    - "browser_screenshot"
    - "memory_recall"
```

- Claude Desktop can connect to the agent and use its browser/memory tools
- **Done:** add agent as MCP server in Claude Desktop → `browser_screenshot` callable from Claude

### 12.7 — MCP in extension manifests ⬜

Extensions can declare MCP servers they bundle (learned from `bundle-mcp.ts`).

In `plugin.toml`:
```toml
[mcp_servers]
[mcp_servers.brave]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-brave-search"]
env = { BRAVE_API_KEY = "${BRAVE_API_KEY}" }
```

- Agent auto-starts declared MCP servers when extension is loaded
- MCP tools appear namespaced: `ext_{plugin_id}_mcp_{server}_{tool}`
- **Done:** install extension with bundled MCP server → MCP server starts → tools available with no extra config

---

## Phase dependencies summary

```
1 (Core) → 2 (NATS) → 3 (LLM) → 4 (Browser)
                    ↘           ↘
                     5 (Memory) → 6 (WhatsApp)
                                ↘
                                 7 (Heartbeat) → 8 (Agent-to-agent)
                                                ↘
                                                 9 (Polish)
                                                ↘
                 10.1–10.3 (Soul/Identity/MEMORY.md) ← needs Phase 3 + 5
                 10.4–10.6 (Transcripts + Dreaming)  ← needs Phase 7 (cron)
                 10.7–10.8 (Vocabulary + Self-report) ← needs Phase 10.6
                                                ↘
                 11 (Extensions) ← needs Phase 2 (NATS) + Phase 3 (ToolRegistry)
                                                ↘
                 12 (MCP) ← needs Phase 11 (ExtensionRuntime) + Phase 3 (ToolRegistry)
```

Phase 11 can start after Phase 3. Phase 12 builds on top of Phase 11's runtime infrastructure.
Sub-phases 12.1–12.3 (MCP client) can start independently of Phase 11 — they only need Phase 3.
