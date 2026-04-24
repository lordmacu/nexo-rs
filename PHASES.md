# Implementation Phases

Ordered execution plan. Each phase depends on the previous. Each sub-phase is a unit of work with clear done criteria.

Reference: `design-agent-framework.md` for architecture decisions.  
OpenClaw reference: `../research/` — study patterns, do not copy TypeScript directly.

## Progress

**Global: 77 / 81 sub-phases done**

| Phase | Done | Total |
|-------|------|-------|
| 1 — Core Runtime | 7 | 7 |
| 2 — NATS Broker | 6 | 6 |
| 3 — LLM Integration | 6 | 6 |
| 4 — Browser CDP | 6 | 6 |
| 5 — Memory | 5 | 5 |
| 6 — WhatsApp Plugin | 0 | 4 |
| 7 — Heartbeat | 3 | 3 |
| 8 — Agent-to-Agent | 3 | 3 |
| 9 — Polish | 6 | 6 |
| 10 — Soul, Identity & Learning | 9 | 9 |
| 11 — Extension System | 8 | 8 |
| 12 — MCP Support | 8 | 8 |

> After each sub-phase: mark ✅ here, update count in this table and in `../CLAUDE.md`.

---

## Phase 1 — Core Runtime

**Goal:** Agent process boots, loads config, receives a message, runs a no-op agent loop.

### 1.1 — Workspace scaffold ✅
- Create `Cargo.toml` workspace with all crates declared
- Create each crate with empty `lib.rs` + `Cargo.toml`
- `cargo build --workspace` passes clean
- **Done:** no compile errors, all crates visible in workspace

### 1.2 — Config loading (`crates/config`) ✅
- Load `config/agents.yaml`, `config/broker.yaml`, `config/llm.yaml`, `config/memory.yaml`
- Resolve `${ENV_VAR}` placeholders at load time
- Error clearly if required env var is missing
- Structs: `AgentConfig`, `BrokerConfig`, `LLMConfig`, `MemoryConfig`
- **Done:** `Config::load("config/")` returns populated structs; missing env var returns descriptive error

### 1.3 — Local event bus (`crates/broker/src/local.rs`) ✅
- `tokio::mpsc`-based in-memory bus, no external dependency
- Implements `BrokerHandle` trait (same interface as NATS broker)
- `subscribe(topic, handler)`, `publish(topic, event)`, `request(topic, msg) -> Response`
- Topic pattern matching: `agent.events.*` wildcards
- **Done:** two tasks can pub/sub through local bus in integration test

### 1.4 — Session manager (`crates/core/src/session/`) ✅
- `Session { id, agent_id, history, context, created_at, last_access }`
- `SessionManager`: create, get, update, expire (TTL from config)
- In-memory store for Phase 1 (SQLite persistence comes in Phase 5)
- **Done:** session created, updated, retrieved, expired after TTL in unit tests

### 1.5 — Agent types + behavior trait (`crates/core/src/agent/`) ✅
- `RunTrigger` enum: `User | Heartbeat | Manual`
- `InboundMessage` struct: `id`, `session_id`, `agent_id`, `sender_id`, `text`, `trigger`, `timestamp`
- `AgentContext` struct: `broker`, `sessions`, `agent_id`, `config`
- `AgentBehavior` trait: `on_message`, `on_event`, `on_heartbeat`, `decide` (all default no-op)
- `Agent` struct: `id`, `config: Arc<AgentConfig>`, `behavior: Arc<dyn AgentBehavior>`
- `NoOpAgent`: logs on `on_message`, no-op elsewhere
- **Done:** `NoOpAgent` receives `InboundMessage`, logs it, unit test passes

### 1.6 — Plugin interface (`crates/core/src/agent/plugin.rs`) ✅
- `Plugin` trait: `name()`, `start(broker)`, `stop()`, `send_command(cmd) -> Response`
- `Command` and `Response` enums (extensible)
- `PluginRegistry`: register by name, lookup, start all, stop all
- **Done:** mock plugin registers, starts, receives command, returns response

### 1.7 — Agent runtime: boot + debounce + queue + dispatch ✅
- `AgentRuntime`: takes `Agent`, `LocalBroker`, `Arc<SessionManager>`
- `start()` spawns tokio task: subscribe `plugin.inbound.{plugin}` per plugin in config
- Inbound debounce: `debounce_ms` from `AgentRuntimeConfig` — emits last event after idle window
- Per-session message queue: `mpsc(queue_cap)` per `session_id`, messages serialized per session
- Dispatch: `behavior.on_message(ctx, msg).await`; errors logged, runtime continues
- **Done:** two tasks send events to agent via broker → debounce collapses → `on_message` called exactly once per debounce window

---

## Phase 2 — NATS Message Broker

**Goal:** Replace local bus with real NATS. Survive NATS restart without losing messages.

### 2.1 — NATS client (`crates/broker/src/nats.rs`) ✅
- Connect to NATS via `async-nats`
- Implements same `BrokerHandle` trait as local bus — zero changes to callers
- Auth via nkey file (path from config)
- **Done:** pub/sub works end-to-end through real NATS server in integration test

### 2.2 — Broker abstraction (`crates/broker/src/lib.rs`) ✅
- `BrokerHandle` enum: `Local(LocalBroker)` | `Nats(NatsBroker)`
- Selected at boot from `config/broker.yaml` `type` field
- **Done:** switching config between `local` and `nats` changes broker with no code change

### 2.3 — Persistent disk queue (`crates/broker/src/disk_queue.rs`) ✅
- Append-only write-ahead log at `./data/queue/`
- On publish: write to disk first, then forward to NATS
- On NATS reconnect: drain disk queue → publish pending → delete entries
- **Done:** kill NATS mid-run, restart, all pending events delivered

### 2.4 — Dead letter queue ✅
- After `max_attempts` failures, move message to `agent.dlq.{topic}`
- DLQ persisted to SQLite (`./data/dlq.db`)
- CLI command to inspect/replay DLQ entries
- **Done:** 3 failed deliveries move message to DLQ; replay command redelivers

### 2.5 — Circuit breaker (inside `NatsBroker`) ✅
- States: `Closed` → `Open` → `HalfOpen` → `Closed`
- `failure_threshold`, `recovery_timeout` configurable per use site
- Wrap every external call site: NATS publish, LLM call, CDP command
- **Done:** inject failures in test, breaker opens, rejects, recovers after timeout

### 2.6 — Backpressure ✅
- `EventBus` tracks pending-per-topic counter
- When `max_pending` exceeded: slow producers via `tokio::time::sleep` backoff
- **Done:** fast publisher + slow subscriber → publisher slows, no memory explosion

---

## Phase 3 — LLM Integration

**Goal:** Agent calls MiniMax, gets completion, parses tool calls, executes via ToolRegistry.

### 3.1 — LLM client trait (`crates/llm/src/client.rs`) ✅
- `LlmClient` trait: `chat(ChatRequest) -> ChatResponse`
- Types: `ChatMessage`, `ChatRole`, `ChatRequest`, `ChatResponse`, `ResponseContent`, `ToolCall`, `ToolDef`, `TokenUsage`, `FinishReason`
- **Done:** trait defined with full type set

### 3.2 — MiniMax client (`crates/llm/src/minimax.rs`) ✅
- POST to `{base_url}/text/chatcompletion_v2`
- `X-MiniMax-Group-Id` header when `group_id` present
- Handles `Text` and `Parts` content formats
- **Done:** full wire type deserialization, maps to `ChatResponse`

### 3.3 — Rate limiter (`crates/llm/src/rate_limiter.rs`) ✅
- Token bucket: `requests_per_second` from config
- `acquire()` sleeps until next allowed slot
- **Done:** two rapid acquires space >= 90ms at 10 rps

### 3.4 — OpenAI-compatible client (`crates/llm/src/openai_compat.rs`) ✅
- POST `{base_url}/chat/completions`
- Reads `retry-after` header on 429
- Covers OpenAI + Ollama (same API shape)
- **Done:** compiles, maps response to `ChatResponse`

### 3.5 — Tool registry (`crates/core/src/agent/tool_registry.rs`) ✅
- `ToolHandler` trait: `call(ctx, args) -> Value`
- `ToolRegistry`: `register`, `get`, `to_tool_defs`
- **Done:** register tool, retrieve by name, list all defs

### 3.6 — Agent LLM loop (`crates/core/src/agent/llm_behavior.rs`) ✅
- `LlmAgentBehavior`: `on_message` → build history → chat loop → execute tools → publish outbound
- `source_plugin` from `InboundMessage` → routes reply to `plugin.outbound.{plugin}`
- Max 10 tool iterations; warns if exhausted
- `AnyLlmClient::stub()` for tests
- **Done:** 3 tests pass — text reply published, tool call then text, session history persists

---

## Phase 4 — Browser CDP Plugin

**Goal:** Agent can navigate, click, fill, screenshot via Chrome DevTools Protocol.

### 4.1 — CDP WebSocket client (`crates/plugins/browser/src/cdp/client.rs`) ✅
- `CdpClient::connect(ws_url)` — tokio-tungstenite, writer+reader tasks, DashMap pending
- `send(method, params)` — atomic id, oneshot channel correlation
- `discover_ws_url(http_url)` — GET /json/version

### 4.2 — Chrome launcher (`crates/plugins/browser/src/chrome.rs`) ✅
- `find_chrome_executable()` — searches PATH + known Linux paths
- `launch(config)` — spawn with `--remote-debugging-port=0`, read WS URL from stderr
- `connect_existing(cdp_url)` — GET /json/version → WS URL
- `RunningChrome::drop` — kills child process

### 4.3 — Element refs system ✅
- `CdpSession::new` — Target.attachToTarget, stores sessionId
- `snapshot()` — JS query all interactive elements, assigns @e1..@eN via data-agent-ref

### 4.4 — Commands CDP ✅
- `navigate`, `click`, `fill`, `screenshot`, `evaluate`, `snapshot`, `scroll_to`
- All use sessionId in CDP messages, timeout from config

### 4.5 — Plugin event loop ✅
- `BrowserPlugin::start(broker)` — subscribes plugin.inbound.browser, dispatches BrowserCmd, publishes to plugin.outbound.browser
- `BrowserCmd` serde tag enum, `BrowserResult` with ok/error/data/snapshot fields

### 4.6 — Session management ✅
- `ensure_session()` — lazy init on first command
- `Arc<Mutex<Option<CdpSession>>>` — reset on stop
- Per-command timeout wrapper

---

## Phase 5 — Memory System

**Goal:** Agent remembers conversation history across restarts. Semantic recall works.

### 5.1 — Short-term memory ✅
- Implemented in Phase 1.4 as `SessionManager` with rolling `history` capped at `max_history_turns`
- No separate `short_term.rs` needed — session is the short-term layer

### 5.2 — SQLite store + schema ✅
- `LongTermMemory::open(path)` — sqlx SqlitePool with WAL mode
- Tables: `memories`, `memories_fts` (FTS5 virtual), `interactions`
- Auto-migrate on open with indexes on agent_id and session_id
- `save_interaction(session_id, agent_id, role, content)` + `load_interactions`

### 5.3 — Long-term: remember / recall / forget ✅
- `remember(agent_id, content, tags)` — INSERT in memories + memories_fts in tx
- `recall(agent_id, query, limit)` — FTS5 MATCH with rank ordering, JOIN with memories
- `forget(id)` — DELETE from both tables in tx
- Tests: 6 green (keyword recall, multi-match, forget, cross-agent isolation)

### 5.4 — Vector index ✅
- `sqlite-vec` auto-extension registrada via `vector::enable()` (libsqlite3-sys bindings)
- `EmbeddingProvider` trait + `HttpEmbeddingProvider` (OpenAI-compat: `/embeddings` endpoint)
- `LongTermMemory::open_with_vector(path, Option<provider>)` crea `vec_memories` vec0 table con dimension del provider
- `remember()` inserta embedding best-effort (log warn on failure)
- `recall_vector(agent_id, query, k)` — KNN via `embedding MATCH ? AND k = ?`
- `recall_hybrid(agent_id, query, k)` — RRF fusion FTS + vector (k_const=60), fallback graceful a FTS si provider Err
- `MemoryTool` acepta `"mode": "keyword" | "vector" | "hybrid"` (default keyword)
- LRU cache 64-entry en `HttpEmbeddingProvider` para single-query re-embeds
- Dimension mismatch al reabrir DB → error fatal con instrucción de borrar DB
- main.rs wire: lee `memory.vector` de `memory.yaml`, construye provider si `enabled: true`, inyecta en `LongTermMemory::open_with_vector`
- Tests: 4 unit provider (wiremock) + 2 unit vector helpers + 7 integration (vector_test.rs)

### 5.5 — Memory tool ✅
- `MemoryTool::new(Arc<LongTermMemory>)` lives in `agent-core` (avoids cyclic dep)
- `ToolHandler::call` dispatches `remember` | `recall` | `forget` by `action` field
- `MemoryTool::tool_def()` returns `ToolDef` with JSON schema for LLM tool calling
- `AgentContext::memory: Option<Arc<LongTermMemory>>` + `with_memory()` builder
- `LlmAgentBehavior` auto-loads last 20 interactions on new session, persists every turn

---

## Phase 6 — WhatsApp Plugin

**Goal:** Agent receives and sends WhatsApp messages via `wa-agent` crate (crates.io name) / `whatsapp_rs` (import name). Integration optimized for the agent runtime wa-agent ships (ACL, dedup, typing heartbeat, chat-meta skip, outbox, reconnect, rate-limit are inherited — not reimplemented).

**Integration model (Model C):** in-process `Session` driven by `run_agent_with` for inbound; direct `Session::send_*` for proactive outbound (heartbeat, A2A). Bridge between wa-agent handler and core uses `session_id` already carried by `Event` (UUIDv5 derived from remote JID).

### 6.1 — Audit + integration ADR ✅
- Read `../whatsapp-rs/src/lib.rs`, `agent.rs`, `client.rs`, `daemon.rs`
- Verify `run_agent_with` concurrency semantics (serial per chat? global?)
- Verify `Session::download_media` / equivalent method name
- Verify daemon IPC socket path (`.whatsapp-rs.sock`?)
- Decide daemon vs in-process (in-process v1; daemon as follow-up extension)
- Decide version pin (path `../whatsapp-rs` during Phase 6, switch to `wa-agent = "=0.1.x"` post-6.8)
- **Done:** `docs/wa-agent-integration.md` written — API mapping table + ADR + pin plan

### 6.2 — Config + bootstrap ⬜
- `crates/plugins/whatsapp/Cargo.toml` with deps (wa-agent path, agent-core, agent-broker, dashmap, tokio, serde, anyhow, uuid, tracing, async-trait, reqwest, mime_guess)
- `src/config.rs`: `WhatsappPluginConfig` with `session_dir`, `media_dir`, `acl`, `behavior`, `rate_limit`, `bridge`, `transcriber`, `daemon` sections; YAML loader with env-var resolution via `agent-config`
- `src/session.rs`: `bootstrap_session(&cfg) -> Result<Session>` — `XDG_DATA_HOME` override, `Client::new().connect()`, QR ASCII render, `creds.json.bak` backup before re-pair
- `src/plugin.rs` skeleton impl of `Plugin` trait
- `config/plugins/whatsapp.yaml` with spec defaults
- **Done:** `cargo build -p agent-plugin-whatsapp`; config parse unit test green

### 6.3 — Inbound bridge (`run_agent_with`) ✅
- `src/session_id.rs`: deterministic UUIDv5 from bare JID (const namespace)
- `src/events.rs`: `InboundEvent` enum (Message/MediaReceived/Qr/Connected/Disconnected/Reconnecting/PairingSuccess/CredentialsExpired)
- `src/bridge.rs`: handler closure — ignore_from_me filter, oneshot insert into `PendingMap` keyed by session_id, publish `plugin.inbound.whatsapp` Event with session_id + payload, timeout policy (`noop` | `apology_text`)
- `plugin.rs::start()` spawns `session.run_agent_with(acl, handler)`
- **Done:** unit test with mock broker resolves oneshot from outbound event → handler returns `Response::Text`; timeout path returns `Noop`

### 6.4 — Outbound dispatcher ✅
- `src/dispatch.rs`: subscriber of `plugin.outbound.whatsapp` — match `event.session_id` against `PendingMap`; hit → oneshot resolve (reactive reply in-handler); miss → direct `Session::send_text` (proactive)
- Support Commands: `SendMessage`, `Custom { name: "react"|"reply"|"typing" }`
- `plugin.rs::send_command(cmd)` publishes to `plugin.outbound.whatsapp` for programmatic use
- **Done:** LocalBroker integration test — proactive path sends via mocked Session; reactive path resolves oneshot → handler returns proper Response

### 6.5 — Media (send + inbound) ✅
- `src/media.rs`: `download_to_bytes(url)` via reqwest; MIME-sniff → `send_image`/`send_video`/`send_audio`/`send_document` selection
- Inbound: if `ctx.msg` carries `MediaInfo` → `Session::download_media(msg)` → write to `cfg.media_dir/{msg_id}.{ext}` → publish `InboundEvent::MediaReceived` alongside normal text event
- Dispatch: `Command::SendMedia { to, url, caption }` → download → `send_media_auto`
- **Done:** MIME→variant selection unit test; live round-trip deferred to 6.8

### 6.6 — Lifecycle + health ✅
- `plugin.rs::health() -> PluginHealth { connected, last_event, outbox_pending }` from `session.metrics()`
- QR expiry watcher: disconnect reason `qr_expired` → re-render + re-publish `InboundEvent::Qr`
- Cred corruption: on `connect()` failure → backup `creds.json` to `creds.json.bak.{ts}` → restart pair flow
- Boot doctor self-test (logs warnings, fatal only if unrecoverable)
- Daemon collision check (`.whatsapp-rs.sock` exists + `daemon.prefer_existing` = true → boot aborts with clear msg)
- Wire into core `/health` (Phase 9.3)
- **Done:** daemon-collision unit test; reconnect path covered

### 6.7 — Transcriber (voice → text) ✅
- `src/transcriber.rs`: `NatsTranscriber` impl `wa_agent::agent::Transcriber` — `broker.request("skill.whisper.transcribe", { audio_base64, mime }, 30s)` → text
- Plugin uses `run_agent_with_transcribe` when `cfg.transcriber.enabled = true`
- **Done:** broker-mock unit test — audio in → transcribed text reaches handler `ctx.text`

### 6.8 — E2E integration test ✅
- `tests/whatsapp_live_test.rs` behind `#[cfg(feature = "live-wa")]`
- Scenarios: inbound text → LLM echo reply; proactive heartbeat reminder; media round-trip; kill-network → auto-reconnect
- **Done:** `cargo test --features live-wa -p agent-plugin-whatsapp` green on live account; normal `cargo test` still green (feature-gated)

### 6.9 — QR friendly (wa-agent hook + plugin event) ✅
- `wa-agent`: add `Client::on_qr(cb)` builder + `QrCallback` type; `run_pairing` now invokes the callback when set and falls back to `println!` otherwise
- `agent-plugin-whatsapp`: `session::connect_session` installs a callback that publishes `InboundEvent::Qr { ascii, png_base64, expires_at }` on `plugin.inbound.whatsapp` each time the server rotates a pairing ref
- PNG encoded via `qrcode` + `image` (256px min, base64)
- **Done:** pairing QR streams through the broker instead of stdout; any subscriber (web UI, Telegram admin, webhook) can render it. Unit test verifies `render_qr_png` produces valid PNG bytes

---

## Phase 7 — Heartbeat Scheduler

**Goal:** Agents fire `on_heartbeat()` on interval. Proactive behavior works.

### 7.1 — Heartbeat runtime (`crates/core/src/heartbeat.rs`) ✅
- Per-agent `tokio::interval` from `heartbeat.interval` config
- On tick: publish `agent.events.{agent_id}.heartbeat`
- `AgentRuntime` subscribes, calls `on_heartbeat(ctx)`
- `heartbeat_interval()` parses config with `humantime`; disabled agents don't spawn ticker
- **Done:** runtime test with `50ms` interval publishes ticks and calls `on_heartbeat()` repeatedly

### 7.2 — Default heartbeat behaviors ✅
- Check pending reminders in memory → send proactive message if due
- Log heartbeat (debug level) with agent id + timestamp
- `LongTermMemory` now persists reminders in SQLite; `LlmAgentBehavior::on_heartbeat()` claims due reminders atomically, publishes to `plugin.outbound.{plugin}`, and marks them delivered
- Failed delivery releases the claim so the next heartbeat can retry it
- **Done:** tests cover due reminder delivery, no duplicate claim, retry after claim release, and heartbeat debug logging path is wired

### 7.3 — Heartbeat tool ✅
- `schedule_reminder(at: DateTime, message: &str)` tool
- Stored in `agent_facts` with type `reminder`
- Heartbeat checks and fires
- `HeartbeatTool` stores reminders in SQLite and accepts RFC3339 timestamps or relative delays like `10m`
- `LlmAgentBehavior` injects runtime context (`session_id`, `source_plugin`, `recipient`) so the model only chooses `at` and `message`
- **Done:** tests cover scheduling from a live conversation and later heartbeat delivery

---

## Phase 8 — Agent-to-Agent Routing

**Goal:** Agent A can delegate tasks to Agent B and receive results.

### 8.1 — Routing protocol ✅
- Topic: `agent.route.{target_id}`
- Message: `AgentMessage { from, to, correlation_id, payload: AgentPayload }`
- `AgentPayload`: `Delegate { task, context }` | `Result { task_id, output }` | `Broadcast { event, data }`
- Defined in `crates/core/src/agent/routing.rs` with serde tags and round-trip test

### 8.2 — Routing in AgentRuntime ✅
- Subscribe to `agent.route.{self.id}` on boot
- On `Delegate`: run agent loop with delegated task, publish `Result` back
- On `Result`: match by `correlation_id`, resume waiting caller
- Runtime now subscribes to route topic and handles `Delegate`/`Result`/`Broadcast`
- **Done:** integration test validates agent A delegates to agent B and receives correlated result

### 8.3 — Delegation tool ✅
- `delegate(agent_id: &str, task: &str) -> Value` tool
- Registered in ToolRegistry, callable by LLM via tool calling
- Waits for result with configurable timeout
- `DelegationTool` added in `agent-core`; uses `AgentRouter::delegate(...)` with timeout
- `LlmAgentBehavior` injects runtime context into delegate args and now implements `decide()` using the same LLM tool loop
- **Done:** test validates LLM calls `delegate` tool, receives result, and emits final user reply

---

## Phase 9 — Observability & Polish

**Goal:** Production-ready: logs, metrics, health checks, graceful shutdown, Docker.

### 9.1 — Structured logging ✅
- `tracing` + `tracing-subscriber` with JSON formatter in production
- Log levels: ERROR (panics/unrecoverable), WARN (retry/circuit breaker), INFO (lifecycle), DEBUG (message flow)
- Span per agent message: `agent_id`, `session_id`, `message_id`
- Baseline now includes structured fields on runtime + LLM path (`agent_id`, `session_id`, `message_id`, `correlation_id`) and richer formatter metadata (`target`, `thread_id`)
- Logging policy added: `AGENT_LOG_FORMAT=pretty|compact|json`; if unset and `AGENT_ENV=production`, default is `json`
- JSON mode implemented with a dedicated tracing layer (no extra dependency features): emits `ts_unix_ms`, `level`, `target`, `thread_id`, source location, fields, and span stack
- **Done:** all log lines have structured fields; `RUST_LOG=info` shows clean output

### 9.2 — Metrics (Prometheus) ✅
- `metrics` + `metrics-exporter-prometheus` crates
- Track: `llm_requests_total`, `llm_latency_ms`, `messages_processed_total`, `circuit_breaker_state`
- Expose at `http://0.0.0.0:9090/metrics`
- Implemented with internal telemetry module + plain Prometheus text endpoint at `:9090/metrics`
- Tracks `llm_requests_total`, `llm_latency_ms` histogram, `messages_processed_total`, `circuit_breaker_state{breaker="nats"}`
- **Done:** `agent` serves `/metrics` in Prometheus text format without extra infra dependencies

### 9.3 — Health check endpoints ✅
- HTTP server (minimal, `axum`) on port 8080
- `GET /health` → 200 if process alive
- `GET /ready` → 200 if broker connected + at least one agent running
- Implemented minimal HTTP server on `:8080`
- `GET /health` returns `200 {"status":"ok"}`
- `GET /ready` returns `200` only when broker is ready and `agents_running > 0`, otherwise `503` with diagnostic payload
- **Done:** readiness now gates on broker connectivity and runtime agent count

### 9.4 — Graceful shutdown ✅
- Implemented coordinated shutdown: `src/main.rs` marks runtime not-ready, stops plugins first (cuts intake), then stops agent runtimes to drain in-flight work.
- Handle `SIGTERM` and `SIGINT`
- On signal: stop accepting new messages → drain in-flight → flush memory store → stop plugins → exit 0
- Timeout: 30s max drain before force exit
- `AgentRuntime::stop()` now cancels intake, closes per-session queues, drains queued/buffered messages, and waits up to 30s before aborting remaining tasks.
- **Done:** runtime test `runtime_stop_flushes_remaining` now asserts pending buffered message is flushed on `stop()`.
- **Done:** `kill -TERM <pid>` → no messages lost; exits within 30s

### 9.5 — Docker Compose ✅
- Services: `nats`, `agent`, `chrome` (browserless)
- Agent image: multi-stage build (builder → runtime)
- Secrets via Docker secrets files (not env vars in compose file)
- Health checks on all services
- Volume mounts: `./config`, `./data`, `./secrets`
- Scaffold implemented: `Dockerfile`, `.dockerignore`, `docker-compose.yml`, `config/docker/*`, `secrets/*.example`
- `docker compose config` validates
- `docker compose up -d` reaches `healthy` for `nats`, `chrome`, and `agent`
- Docker builder updated to `rust:1-bookworm` to satisfy crates requiring `rustc >= 1.88`
- Restart persistence verified: `docker compose down && docker compose up -d` keeps data/volume state (`/app/data/memory.db`, `proyecto_nats_data`)
- **Done:** `docker compose up` → all services healthy; `docker compose down && up` → state persists

### 9.6 — Integration test suite ✅
- `scripts/integration_stack_smoke.sh` — 8 pasos contra el compose stack real:
  1. Salud de containers · 2. `/health`+`/ready` · 3. `/metrics` (TYPE + etiquetas) ·
  4. NATS `/healthz` · 5. Browser E2E (CDP discover → navigate → screenshot → evaluate) ·
  6. NATS restart recovery (trip + reconexión) · 7. Agent-to-agent delegation ·
  8. DiskQueue drain_nats.
- `scripts/extensions_smoke.sh` — smoke offline-first para extensiones stdio
  (handshake, status, errores esperados sin credenciales) y ahora integrado en
  `make integration-suite`.
- Tests gated por env var: `CDP_URL` (`browser_cdp_e2e`), `NATS_URL`
  (`delegation_e2e_test`, `disk_queue_drain_nats_test`). Skip limpio sin la var.
- Deferred: WhatsApp real end-to-end queda aparcado con Phase 6.
- **Done:** `cargo test --workspace` verde; `make integration-smoke` pasa 8/8 contra compose.

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

### 10.1 — Agent identity system ✅

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

### 10.2 — SOUL.md — agent personality file ✅

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

### 10.3 — MEMORY.md — persistent self-knowledge ✅

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

### 10.4 — Session transcripts ✅

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

### 10.5 — Recall signal tracking ✅

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

### 10.6 — Dreaming — memory consolidation sweep ✅

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

### 10.7 — Concept vocabulary ✅

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

### 10.8 — Agent self-report ✅

Agent can describe its own state on demand.

Tools:
- `who_am_i()` → returns SOUL.md summary + identity fields
- `what_do_i_know()` → returns MEMORY.md sections
- `my_stats()` → session count, facts stored, last dream date, recall signals this week

- **Done:** user asks "what do you remember about me?" → agent reads MEMORY.md → responds with structured summary

### 10.9 — Git-backed memory workspace ✅

Wrap the workspace dir (MEMORY.md, SOUL.md, memory/YYYY-MM-DD.md) in a local git repo so every mutation produces a reviewable, revertable commit. Matches the `DiffMem`/Manus/Claude Code pattern that stabilised in 2026 as the de-facto agent memory protocol.

**Goal:** forensics, rollback, and blame for agent memory without paying operational cost (no remotes, no shell-out, batched commits).

```rust
pub struct MemoryGitRepo {
    root: PathBuf,
    repo: git2::Repository,
    author: git2::Signature<'static>,  // "{agent_id} <agent@{hostname}>"
}

impl MemoryGitRepo {
    pub fn open_or_init(root: &Path, agent_id: &str) -> anyhow::Result<Self>;
    pub fn commit_all(&self, subject: &str, body: &str) -> anyhow::Result<git2::Oid>;
    pub fn log(&self, n: usize) -> anyhow::Result<Vec<CommitSummary>>;
    pub fn diff_since(&self, oid: git2::Oid) -> anyhow::Result<String>;
    pub fn revert(&self, oid: git2::Oid) -> anyhow::Result<()>;
}
```

Design rules:
- **Local only by default** — no remote push. Optional `git.remote` field for operators who run a self-hosted git server (Gitea/forgejo). Never GitHub unless explicitly opted in (PII risk).
- **Commit batching, never per-write** — triggers are:
  1. End of each dreaming sweep (10.6) → commit with LLM-generated summary
  2. Session close in `SessionManager::delete` → commit the day's `memory/YYYY-MM-DD.md`
  3. Explicit `forge_memory_checkpoint` tool invocation
- **`.gitignore`** auto-generated on `init`: `transcripts/`, `media/`, `*.tmp` (PII + binaries stay out of history)
- **`libgit2` via `git2` crate** — no shell-out to `git` CLI; works on hosts without git installed
- **Pre-commit validation** — file size cap, frontmatter valid, forbidden patterns (phone numbers, emails in MEMORY.md)
- **LLM-generated commit messages** — same MiniMax client; prompt: "resume los cambios de este diff en 1 linea subject + 2-5 bullets body"
- **Config** in `agents.yaml`:
  ```yaml
  workspace: "./workspaces/kate"
  git:
    enabled: true
    remote: ""       # empty = local only
    push_on_commit: false
    author_name: "kate"
    author_email: "agent@localhost"
    max_file_bytes: 1048576  # 1 MiB
  ```

Sub-phase breakdown:
- 10.9.1 — `git2` dep + `MemoryGitRepo::open_or_init` + `.gitignore` bootstrap
- 10.9.2 — `commit_all` with batching; integrate in dreaming sweep
- 10.9.3 — Session-close hook in `SessionManager::delete` (requires the `on_session_expire` callback that's also a 12.4 follow-up — land together)
- 10.9.4 — `forge_memory_checkpoint` native tool (agent-triggered commit with user-provided note)
- 10.9.5 — `log` / `diff_since` tools so the LLM can inspect its own history cheaply (DiffMem pattern)
- 10.9.6 — Pre-commit validator + size cap
- 10.9.7 — Optional remote push (guarded by flag, rate-limited)

**Done:** `dreaming` sweep produces a commit visible in `git log`; corrupt MEMORY.md recoverable via `git revert`; `forge_memory_checkpoint("milestone")` works from LLM tool loop; integration test verifies commits land and are diffable.

**Priority:** deferred. Ship after Phase 6 (WhatsApp) and any remaining Phase 12 work. Implement when a real deployment produces the first "why did the agent say X yesterday" incident or when the first memory-corruption bug appears. See FOLLOWUPS for rationale.

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

### 11.1 — Extension manifest (`plugin.toml`) ✅

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

### 11.2 — Extension discovery ✅

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

### 11.3 — Extension runtime (stdio transport) ✅

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

### 11.4 — Extension runtime (NATS transport) ✅

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

### 11.5 — Extension tool registration ✅

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

### 11.6 — Lifecycle hooks ✅

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

### 11.7 — Extension CLI commands ✅

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

### 11.8 — Extension template ✅

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

**Runtime wiring:** `src/main.rs` construye `McpRuntimeManager` al boot cuando `mcp.yaml` está habilitado, mergea servers declarados por extensions via `collect_mcp_declarations`, y registra sus tools + resource meta-tools en cada `ToolRegistry` via `register_session_tools(&runtime, &tools)`. Sentinel `Uuid::nil()` compartido entre agents. Shutdown llama `manager.shutdown_all()` antes de parar los agent runtimes.

### 12.1 — MCP client (stdio transport) ✅

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

### 12.2 — MCP client (HTTP/SSE transport) ✅

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

### 12.3 — MCP tool catalog ✅

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

### 12.4 — Session-scoped MCP runtime ✅

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

### 12.5 — MCP resources ✅

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

### 12.6 — Agent as MCP server (optional) ✅

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

### 12.7 — MCP in extension manifests ✅

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

### 12.8 — tools/list_changed hot-reload ✅

MCP servers can push `notifications/tools/list_changed` when their tool set mutates at runtime (e.g. a filesystem server that gains a new directory capability). Before 12.8 the client silently dropped the notification; the LLM kept using a stale snapshot until the process was restarted.

- `ClientEvent` enum (`ToolsListChanged`, `ResourcesListChanged`, `Other(method)`) emitted on a `tokio::sync::broadcast` channel by both `StdioMcpClient` and `HttpMcpClient`
- Stdio reader parses notifications before response routing (notifs have no `id`)
- HTTP emits from both SSE dispatcher and streamable-stream message scanner
- `SessionMcpRuntime::on_tools_changed<F>(F)` spawns a per-client debounce task (200 ms window) that fires the callback once per server per burst
- `ToolRegistry::clear_by_prefix(&str) -> usize` removes all handlers under `mcp_{sanitized_server}_`
- main.rs wiring: on notification, clear prefix + `register_session_tools` rebuilds the session catalog
- Broadcast `Lagged` is tolerated (debounce re-fire covers it); `Closed` exits the task; dispose aborts all watchers
- **Done:** mock server emits notif → client event fires → session callback invokes after debounce → registry re-registered without process restart (tests in `crates/mcp/tests/hot_reload_test.rs`)

---

## Phase 13 — Skills (OpenClaw-inspired)

Reference: `proyecto/OPENCLAW-SKILLS-PLAN.md`.

### 13.1 — Prompt-layer skill loader   ✅
- `AgentConfig.skills` + `skills_dir` in `crates/config/src/types/agents.rs`
- `crates/core/src/agent/skills.rs` (`SkillLoader`, `render_system_blocks`)
- Injection in `llm_behavior.rs` (workspace → skills → system_prompt)
- 6 skill markdowns under `skills/`
- Agent `kate` activates them in `config/agents.yaml`
- **Done:** skills appear inside the system message, missing files warn instead of fail

### 13.2 — Tool-backed: weather   ✅
- Extension `extensions/weather/` rewritten v0.2.0
- Provider: Open-Meteo (free, no API key); replaces wttr.in + curl subprocess
- Tools: `status`, `current`, `forecast` (1–16 days, metric/imperial)
- Reliability: reqwest blocking + rustls, 5s connect / 10s total timeout, retry 3× on 5xx and timeouts (500ms→1s→2s), per-host circuit breaker (5 fails / 30s open), 24h geocoding cache (cap 1000)
- 13 tests: 8 unit (wmo, breaker, cache) + 5 integration (wiremock-mocked Open-Meteo)
- Skill doc updated to reflect new tool schema (units, days max 16)
- **Done:** `cargo test` green; smoke `./target/release/weather` returns Open-Meteo metadata

### 13.3 — Tool-backed: openstreetmap   ✅
- Extension `extensions/openstreetmap/` rewritten v0.2.0
- Provider: Nominatim (OSM, no API key)
- Tools: `status`, `search` (forward geocode), `reverse` (reverse geocode)
- Reliability: reqwest blocking + rustls, 5s/10s timeouts, retry 3× on 5xx/timeout, circuit breaker (5 fails / 30s), rate limiter ~1 req/sec (Nominatim usage policy)
- 15 tests: 9 unit (breaker, cache, rate-limit) + 6 integration (`wiremock`)
- Skill doc updated; reuses breaker/cache primitives from weather (consider extracting to shared crate, see FOLLOWUPS)
- **Done:** `cargo test` green; smoke `./target/release/openstreetmap` returns Nominatim metadata
### 13.4 — Tool-backed: github (REST direct)   ✅
- Decision: dropped MCP plan and `gh` CLI wrapper in favor of direct REST API calls (consistent with weather/osm reliability stack)
- Extension `extensions/github/` rewritten v0.2.0
- Provider: GitHub REST API v2022-11-28 (`api.github.com`); auth via `GITHUB_TOKEN` Bearer
- Tools: `status`, `pr_list`, `pr_view`, `pr_checks` (two-step PR→check-runs), `issue_list` (filters out PRs)
- Reliability: reqwest blocking + rustls, 5s/15s timeouts, retry 3× on 5xx/timeout, circuit breaker (5 fails / 30s)
- Typed errors: `Unauthorized` (-32011), `Forbidden` (-32012), `RateLimited` with `X-RateLimit-Reset` epoch (-32013), `NotFound` (-32001)
- 13 tests: 4 unit (breaker) + 9 integration (`wiremock`)
- Skill doc rewritten; `GITHUB_DEFAULT_REPO` env removes need for repeated `repo` arg
- **Done:** `cargo test` green; smoke status returns API metadata + token-present flag
### 13.5 — Tool-backed: summarize   ✅
- Extension `extensions/summarize/` rewritten v0.2.0
- Provider: OpenAI-compatible `/chat/completions` (works with OpenAI, MiniMax, Groq, llama.cpp server)
- Tools: `status`, `summarize_text`, `summarize_file` (UTF-8, ≤ 1 MB / 60k chars)
- `length`: short|medium|long → maps to system prompt (1–2 / 3–5 / 6–10 sentences)
- Reliability: reqwest blocking + rustls, 5s/30s timeouts, retry 3× on 5xx/timeout, circuit breaker (5 fails / 30s)
- Typed errors: `Unauthorized` (-32011), `EmptyCompletion` (-32007)
- 10 tests: 4 unit (breaker) + 6 integration (`wiremock` chat/completions)
- Skill doc rewritten with chunking guidance for oversized inputs
- **Done:** `cargo test` green; smoke status returns endpoint+model+token flag
### 13.6 — Tool-backed: openai-whisper   ✅
- Extension `extensions/openai-whisper/` rewritten v0.2.0
- Provider: OpenAI-compatible `/audio/transcriptions` (multipart upload). Compatible con OpenAI, Groq Whisper-large-v3, local whisper.cpp HTTP
- Tools: `status`, `transcribe_file` (≤ 25 MB, model/language/prompt/format/temperature args)
- Formatos respuesta: `text` (default) | `json` | `verbose_json` (segments+timestamps) | `srt` | `vtt`
- Reliability: reqwest blocking + multipart, rustls TLS, 5s/120s timeouts, retry 2× con backoff 1500ms (audio uploads costosos), circuit breaker 5/60s
- Typed errors: `Unauthorized` (-32011), `PayloadTooLarge` (-32014), `UnsupportedMedia` (-32015), `EmptyTranscript` (-32007)
- Reusa `ext_common::Breaker`
- 10 integration tests con wiremock (status, text/json/verbose_json, 401/413/415/5xx, validación)
- **Done:** `cargo test` green; smoke status verde; SKILL.md actualizada con guía por error
### 13.7 — Skill metadata (frontmatter, `requires`)   ✅
- `SkillMetadata { name, description, requires: { bins, env }, max_chars }` parsed from optional YAML frontmatter (`---` delimited) at the top of each `SKILL.md`
- Backwards compatible: skills without frontmatter behave exactly as Phase 13.1 (no breaking change)
- Malformed YAML logs at warn and loads with default metadata — never a hard failure
- `requires.bins` checked via PATH walk at load time; missing bins logged
- `requires.env` checked via `std::env::var`; missing/empty vars logged
- `LoadedSkill.missing_bins` / `missing_env` exposed for downstream policy decisions
- `render_system_blocks` uses `metadata.name` as display heading and `metadata.description` as a `> blockquote` line
- `metadata.max_chars` truncates content with `…[truncated to N chars]` marker
- 6 unit tests in `crates/core/src/agent/skills.rs`
- All 6 user skills (weather, openstreetmap, github, summarize, openai-whisper, goplaces) upgraded with frontmatter
- **Done:** `cargo test --workspace --lib` green; agent-core lib went from 93 → 99 tests
### 13.19 — Tool-backed: anthropic + gemini (LLM providers)   ✅
- **Decisión**: no son extensions stdio — son **LLM providers** nativos que pagan la inversión del LlmRegistry refactor. Van en `crates/llm/` y se registran en `with_builtins()`.
- `crates/llm/src/anthropic.rs` (~230 LOC): Messages API, auth `x-api-key` + `anthropic-version`, text-only (tool-calling followup), system prompt split, token usage mapping
- `crates/llm/src/gemini.rs` (~210 LOC): `generateContent`, auth via `?key=`, roles user→user/assistant→model, systemInstruction separado, generationConfig (maxOutputTokens+temperature)
- Ambos implementan `LlmClient` + `LlmProviderFactory`; registrados en `LlmRegistry::with_builtins()` junto a minimax+openai
- Retry/circuit/rate-limit igual que OpenAI (rate-limit inherited con `with_retry` + `CircuitBreaker`)
- Agregar Anthropic/Gemini a un agente: `llm.yaml` → `providers.anthropic.api_key=$ANTHROPIC_API_KEY`, `providers.gemini.api_key=$GEMINI_API_KEY` + `agents.yaml` → `model.provider: anthropic, model: claude-opus-4-7` o `provider: gemini, model: gemini-2.5-pro`
- Tests: 5 registry tests existentes siguen verde (nuevos builtins sin break)
- **Followup**: tool-call bridging para ambos (Anthropic `tool_use` blocks, Gemini `functionCall`)

### 13.20 — Tool-backed: brave-search   ✅
- Extension `extensions/brave-search/` + 1 tool `brave_search(query, count?, freshness?, country?, safesearch?)`
- Auth via `X-Subscription-Token` header con `BRAVE_SEARCH_API_KEY`
- `freshness` (pd/pw/pm/py), `country` (ISO 2), `safesearch` (off/moderate/strict)
- Retry+CB reuse `ext_common::Breaker`
- 4 integration tests (wiremock): status, search happy-path con query_param check, missing key, 401 → -32011
- Free tier ~2k queries/día

### 13.21 — Tool-backed: wolfram-alpha   ✅
- Extension `extensions/wolfram-alpha/` + 2 tools: `wolfram_short(input, units?)` para single-line answers (endpoint `/v1/result`), `wolfram_query(input, format?, units?)` para full pods (`/v2/query`)
- Auth via `appid` query param con `WOLFRAM_APP_ID`
- Flatten pods: `{id, title, primary, subpods}` para cada pod
- Manejo especial: HTTP 501 del `/v1/result` mapea a `{ok:false, error:"no_result"}` (Wolfram usa 501 como "no entendí")
- 5 integration tests (wiremock)

### 13.22 — Tool-backed: docker-api   ✅
- Extension `extensions/docker-api/` wraps `docker` CLI
- 8 tools: `status`, `ps(all?, filter?)`, `inspect(target)`, `logs(target, lines?, since?)`, `stats(target)`, `start†`, `stop†(timeout_secs?)`, `restart†(timeout_secs?)`
- Write gate: `DOCKER_API_ALLOW_WRITE=true` para start/stop/restart
- Container name validator regex `[a-zA-Z0-9][a-zA-Z0-9_.-]*` bloquea shell injection en args
- `ps --format '{{json .}}'` — parsea cada linea como JSON → array estructurado
- Subprocess con watchdog SIGKILL (mismo patrón video-frames)
- 8 tests (2 unit name validator + 6 integration contra docker real con guards graceful si no hay docker)

### 13.23 — Tool-backed: proxmox   ✅
- Extension `extensions/proxmox/` wraps Proxmox VE REST API
- 6 tools: `status`, `list_nodes`, `list_vms(node?)`, `list_containers(node?)`, `vm_status(node, vmid, kind?)`, `vm_action†(node, vmid, kind?, action)`
- Auth: API Token via `Authorization: PVEAPIToken=user@realm!tokenid=value` header (`PROXMOX_TOKEN` env)
- Write gate: `PROXMOX_ALLOW_WRITE=true` para vm_action (start/stop/shutdown/reboot/suspend/resume)
- `PROXMOX_INSECURE_TLS=true` para self-signed certs en LAN
- `kind` ∈ qemu|lxc; `list_containers` sin node usa `/cluster/resources` + filter client-side
- 7 integration tests (wiremock): status, list_nodes con auth header, vm_action gated, vm_status lxc path, node validator, 401, missing url/token

### 13.18 — Tool-backed: google (Gmail + Calendar + Tasks + Drive + People + Photos)   ✅
- Extension `extensions/google/` con OAuth 2.0 user refresh-token flow (service accounts no aplican para Gmail personal sin Workspace domain-wide delegation)
- `oauth.rs` con cache in-process (parking_lot::Mutex) con margen de 60s antes del expiry; endpoint override via `GOOGLE_OAUTH_TOKEN_URL`
- **32 tools**: status + 5 Gmail + 5 Calendar + 5 Tasks + 6 Drive + 7 People + 4 Photos
  - Gmail: list, read (body_text decoded base64url + headers flat), search (alias), send†, modify_labels†
  - Calendar: list_calendars, list_events (date-only vs RFC 3339 auto-detectado), create†, update†, delete†
  - Tasks: list_lists, list_tasks, add†, complete†, delete†
  - Drive: list, get, download (sandbox enforced), upload†, create_folder†, delete†
  - People: list, search (fuzzy), get, other_list, create†, update† (auto-fetch etag + field mask), delete†. Render flat: display_name + emails + phones + organization + notes
  - Photos (readonly): list_media, search (date ranges + content categories + media types + favorites + album — album_id y filters son mutually exclusive), get_media, list_albums
- **Write gate** 5 flags independientes: `GOOGLE_ALLOW_SEND`, `GOOGLE_ALLOW_CALENDAR_WRITE`, `GOOGLE_ALLOW_TASKS_WRITE`, `GOOGLE_ALLOW_DRIVE_WRITE`, `GOOGLE_ALLOW_CONTACTS_WRITE`. Writes sin flag → `-32043 WriteDenied`
- **Drive sandbox**: `GOOGLE_DRIVE_SANDBOX_ROOT` (default temp); download/upload paths enforced
- Multipart upload custom (no dep de `multipart` crate) — metadata JSON + bytes en boundary manual
- Error surface: -32011/-32012/-32001/-32013/-32043/-32602
- `#![recursion_limit="512"]` en lib.rs por el tamaño del schema JSON macro
- reuse `ext_common::Breaker`; reqwest blocking + rustls
- 22 integration tests (wiremock) — cubren gmail/calendar/tasks/drive + people search flattening + people write-gate + photos album-vs-filter exclusivity + photos filter serialization
- OAuth setup documentado: Cloud Console project → OAuth client ID → OAuth Playground → refresh_token (operador, ~5 min)
- Skill doc con scopes recomendados + patterns (resolve contact → email/invite, search photos por fecha/contenido, PDF Drive→summarize)

### 13.17 — Tool-backed: rtsp-snapshot   ✅
- Reinterpret de OpenClaw `camsnap` (bin propietario `camsnap`) → ffmpeg subprocess
- Tools: `status`, `snapshot(url, output_path, transport?, width?)`, `clip(url, output_path, duration_secs, transport?)`
- URL allowlist estricta: `rtsp/rtsps/http/https`; rechaza `file://`, `concat:` (ffmpeg gadgets)
- Sandbox `RTSP_SNAPSHOT_OUTPUT_ROOT` (default temp); path traversal bloqueado
- Watchdog SIGKILL en timeout; `RTSP_SNAPSHOT_TIMEOUT_SECS` configurable (default 60s, max 600s)
- Clip con `-c copy` (stream copy, sin re-encode)
- 12 tests (5 unit + 7 integration) — live-camera path testeado con URL unreachable para ejercitar subprocess runner
- Pipeline doc: snapshot→vision-lm, clip→video-frames.extract_audio→whisper

### 13.16 — Tool-backed: spotify   ✅
- Reinterpret de OpenClaw `spotify-player` (wraps TUI CLIs `spogo`/`spotify_player`) → Spotify Web API directo (más práctico en servidor headless)
- Tools: `status`, `now_playing`, `search`, `play`, `pause`, `next`, `previous`
- Auth: `SPOTIFY_ACCESS_TOKEN` env (refresh flow OAuth a cargo del operador — doc explica que la extension no refreshea)
- Detección de `NO_ACTIVE_DEVICE` via body sniffing → `-32070` (mensaje específico: "abre Spotify en un device")
- Rate-limit 429 con `retry_after_secs` parseado del header
- URI validation: `spotify:track|album|playlist|artist|show|episode:...`
- reuse `ext_common::Breaker`; reqwest blocking + rustls
- 12 integration tests (wiremock): now_playing shapes, 204/no-device, search, 401/403/429/NO_ACTIVE_DEVICE, URI validation, missing token

### 13.15 — Tool-backed: endpoint-check   ✅
- Reinterpret de OpenClaw `healthcheck` (host hardening, no portable) → HTTP probe + TLS cert inspection
- Tools: `status`, `http_probe(url, method?, timeout?, follow_redirects?, expected_status?)`, `ssl_cert(host, port?, timeout?, warn_days?)`
- HTTP: GET/HEAD con latency_ms, final_url, content_type, body_preview (≤500 chars), matches_expected opcional
- TLS cert: TCP + rustls handshake con **accept-any verifier** (ver expired/self-signed cert info sin bloquear); parse vía `x509-parser` → subject, issuer, SANs, serial_hex, signature_algorithm, chain_length, not_before/not_after_unix, seconds_until_expiry, days_until_expiry, expiring_soon, expired
- Error codes nuevos: -32060 Resolve, -32061 Connect, -32062 TLS, -32063 Parse
- 10 integration tests (wiremock HTTP + unreachable host para SSL resolve)

### 13.14 — Tool-backed: tmux-remote   ✅
- OpenClaw port directo (sin reinterpret)
- Tools: `status`, `new_session`, `send_keys`, `capture_pane`, `list_sessions`, `kill_session`
- Socket dedicado `TMUX_REMOTE_SOCKET` (default `$TMPDIR/agent-rs-tmux.sock`), aislado del tmux del operador
- Session name validator regex `[A-Za-z0-9_-]{1,64}` — bloquea shell injection en `tmux -t`
- send_keys split en dos invocaciones (literal keys + Enter) para evitar que tmux parsee `C-m` dentro del string
- `list_sessions` normaliza "no server" / "No such file or directory" a `{count:0, sessions:[]}`
- Format string `#{session_name}|#{session_created}|#{session_windows}` → parsing trivial
- 11 tests (3 unit + 8 integration con socket efímero per-test + cleanup kill)

### 13.13 — Tool-backed: onepassword   ✅
- Nueva extension `extensions/onepassword/` (crate `onepassword-ext`, bin `onepassword`)
- Wraps `op` CLI con service-account auth (`OP_SERVICE_ACCOUNT_TOKEN`) — descarta el tmux+desktop-app hack de OpenClaw (solo macOS)
- Tools: `status`, `whoami`, `list_vaults`, `list_items`, `read_secret`
- **Reveal policy opt-in**: `read_secret` devuelve solo `{length, fingerprint_sha256_prefix}` por defecto; con `OP_ALLOW_REVEAL=true|1|yes` añade `value`. Fingerprint = primeros 8 bytes de `sha256(secret)` hex → permite verificar identidad sin leak
- `list_items` **siempre** strippea campos tipo `fields[].value` antes de serializar (test dedicado que verifica `"SECRET_LEAK"` nunca aparece en la salida JSON)
- Strict validator `op://Vault/Item/field`: rechaza wildcards, query strings, segmentos vacíos, esquemas no-`op`
- Subprocess runner con watchdog SIGKILL (mismo patrón video-frames), timeout configurable via `OP_TIMEOUT_SECS` (30s default, 300s max)
- `OP_BIN_OVERRIDE` para tests — apunta a bash script fake que emite JSON predeterminado
- Error codes nuevos: -32040 MissingBinary, -32041 MissingServiceToken, -32042 NonZeroExit (con stderr preview), -32043 RevealDenied (informativo)
- 16 tests (5 unit + 11 integration) serializados vía `serial_test` por env var compartida
- SKILL.md con warning prominente sobre reveal flow (secret → LLM → transcripts → memory → NATS) + patrón recomendado "verify-by-fingerprint, reveal only when forced"
- Smoke release verde: reporta correctamente missing bin + missing token sin crashear
- **Done:** `cargo test -p onepassword-ext` verde

### 13.12 — Tool-backed: session-logs   ✅
- Tool **agent-core puro** (no extension; lee filesystem local sin subprocess ni red) — primera skill backed "in-process"
- Archivo `crates/core/src/agent/session_logs_tool.rs`; wire en `agent/mod.rs` (`pub use SessionLogsTool`)
- Lee JSONL transcripts bajo `ctx.config.transcripts_dir` (Phase 10.4 writer)
- Tool `session_logs` con dispatch por action:
  - `list_sessions` — escanea directorio, devuelve summary (header + entry_count + first/last timestamps), ordenado por modificación más reciente
  - `read_session { session_id }` — entradas ordenadas + header + `truncated` flag
  - `search { query }` — substring case-insensitive across todas las sesiones, devuelve hits con preview
  - `recent { session_id? }` — tail N entradas; defaults a la sesión actual del context
- Límites: `MAX_LIMIT=500`, `max_chars` 20–4000 default 200 para preview (evita blow context window)
- Aislamiento: scoped al `transcripts_dir` del agente; sin acceso cross-agent
- Transcript dir vacío → `{ok: false, error: "transcripts_dir is not configured..."}`
- Skill doc con frontmatter (Phase 13.7) y guía para diferenciar vs. `memory` tool (vector search)
- 8 unit tests: list (2 sessions), read (order preserved), read missing (error), search (case-insensitive Unicode), recent (default current session + tail), truncates long content, missing transcripts_dir, unknown action
- **Done:** `cargo test -p agent-core --lib session_logs` verde (8/8); agent-core lib pasa de 112 → 120

### 13.11 — Tool-backed: video-frames   ✅
- Nueva extension `extensions/video-frames/` (crate `video-frames-ext`, bin `video-frames`)
- Wraps `ffmpeg` + `ffprobe` subprocess (no pure-Rust codec; declarado como `requires.bins` en plugin.toml)
- Tools: `status`, `probe`, `extract_frames`, `extract_audio`
- Features: `probe` devuelve duración + streams JSON; `extract_frames` soporta evenly-spaced via `count` o `fps` fijo + resize opcional; `extract_audio` mp3 default o wav con mono+sample_rate (default 16k para Whisper)
- **Sandbox**: `VIDEO_FRAMES_OUTPUT_ROOT` (default temp) — todo output path debe estar dentro; rechaza con `-32034`
- **Watchdog**: per-subprocess timeout vía mpsc + SIGKILL on Unix; configurable `VIDEO_FRAMES_TIMEOUT_SECS` (default 600s, max 3600s); evita dep completa de `libc` con extern "C" kill
- Input cap 500 MB, frames cap 1000
- Error codes nuevos: `-32030` MissingBinary, `-32031` SpawnFailed, `-32032` NonZeroExit (stderr preview), `-32033` Timeout, `-32034` IoError
- 14 tests (4 unit + 10 integration) serializados via `serial_test` por env var compartida; synthetic fixture red+sine generado en runtime
- Pipeline doc: `video → extract_audio → whisper.transcribe → summarize`
- **Done:** `cargo test -p video-frames-ext` verde; smoke release OK

### 13.10 — Tool-backed: fetch-url   ✅
- Decisión: OpenClaw `xurl` es Twitter/X API (naming confuso). Reinterpretado como fetch URL genérico — valor real para pipeline `url → text → summarize`
- Nueva extension `extensions/fetch-url/` (crate `fetch-url-ext`, bin `fetch-url`)
- Tools: `status`, `fetch_url(url, method?, headers?, body?, max_bytes?, timeout_secs?, allow_private?)`
- Métodos soportados: GET/POST/PUT/DELETE/HEAD/PATCH/OPTIONS
- Límites: 5 MB default / 50 MB hard cap; 15s default / 120s max timeout
- Reliability: reqwest blocking + rustls + gzip/brotli, retry 3× en 5xx+timeouts, `ext_common::Breaker` (threshold 10 / 30s), 5 redirects max
- **SSRF guard** activo por default:
  - Blocklist hostnames: `localhost`, `metadata.google.internal`, `metadata`
  - IPv4: loopback, private (10/172.16/192.168), link-local, `169.254.169.254` (cloud metadata)
  - IPv6: loopback `::1`, unique-local `fc00::/7`, link-local `fe80::/10`
  - Override per-call con `allow_private: true`
  - Documentado: DNS-based SSRF no cubierto (usar `allow_private` en callers confiables)
- Body decoding: UTF-8 → `body_text`, binario → `body_base64` (content-type heurística)
- Error codes: -32020 BlockedHost, -32021 SizeCap, plus estándares (-32002/3/4/5)
- Reuse `ext_common::Breaker` (5ª extension con el patrón dedup)
- 17 tests: 7 unit SSRF guards (localhost, ipv4 loopback/private, ipv6 loopback/unique-local, metadata, public allowed) + 10 integration wiremock (GET json, POST body+headers, size cap truncate, 404→-32002, 5xx retry→-32003, private blocked, non-http scheme, bad url, status limits, binary→base64)
- Skill doc con pipeline patterns URL→summary y URL→PDF→summary
- Smoke release verde
- **Done:** `cargo test -p fetch-url-ext` verde

### 13.9 — Tool-backed: pdf-extract   ✅
- Nueva extension `extensions/pdf-extract/` (crate `pdf-extract-ext`, bin `pdf-extract`)
- Decisión: OpenClaw `nano-pdf` es un editor Python con LLM — reinterpretado como **extractor** puro Rust (caso de uso real: PDFs → summarize pipeline)
- Backend: crate `pdf-extract 0.7` (sin Poppler/Python/ffmpeg)
- Tools: `status`, `extract_text(path, max_chars?)` con defaults seguros (25 MB file cap, 200 000 chars output cap)
- Typed errors: `-32602` bad input (missing, empty path, bad max_chars), `-32006` provider error (malformed PDF)
- Frontmatter (Phase 13.7) en `skills/pdf-extract/SKILL.md` con guía para chain a summarize
- Fixture tests: `tests/fixtures/hello.pdf` (2.4 KB, generado via ps2pdf)
- 8 integration tests: status, extraction happy path, max_chars truncate, missing file, empty path, max_chars=0, non-pdf → provider error, unknown tool
- README con pipeline pattern extract → summarize
- Smoke release verde: extrae texto del fixture correctamente
- **Done:** `cargo test -p pdf-extract-ext` verde

### 13.8 — Taskflow runtime   ➡️ Promoted to Phase 14
TaskFlow es un substrate runtime, no una skill markdown. Movido a su propia phase con sub-fases. Ver Phase 14 abajo.

---

## Phase 14 — TaskFlow Runtime (durable multi-step flows)

**Objetivo**: substrate de flows durables/resumibles que sobreviven restart, soportan revision-checked mutations y enlazan child tasks bajo un owner-session único. Reemplaza la Phase 13.8 que asumía SKILL.md — TaskFlow necesita runtime nuevo.

**Inspirado en**: `research/skills/taskflow/SKILL.md` + `research/docs/automation/taskflow.md` + `research/src/plugins/runtime/runtime-taskflow.ts`. Adaptado a Rust + microservices: persistencia en SQLite (en lugar de Node KV), wait/resume vía heartbeat (Phase 7) en lugar de event loop, exposición a agent vía ToolRegistry (Phase 3.5) en lugar de plugin SDK.

**Modelo**:
- `Flow { id, owner_session, requester_origin, controller_id, goal, current_step, state_json, wait_json, status, revision }`
- `FlowStep { flow_id, runtime: managed|mirrored, child_session_key, run_id, status }`
- `FlowStatus { Created, Running, Waiting, Cancelled, Finished, Failed }`
- Cada mutación lleva `expected_revision`; conflicto → `RevisionMismatch` error

### 14.1 — Schema + FlowStore (SQLite)   ✅
- Crate nuevo `crates/agent-taskflow` (member del workspace)
- Tablas: `flows`, `flow_steps`, `flow_events` (audit trail append-only)
- `FlowStatus { Created, Running, Waiting, Cancelled, Finished, Failed }` con `is_terminal()` y round-trip de string
- `Flow` con `state_json` (Value) + `wait_json` opcional + `cancel_requested` sticky + `revision` i64
- `FlowStore` trait async + `SqliteFlowStore` impl sobre `sqlx::SqlitePool`
- CRUD: `insert`, `get`, `list_by_owner`, `list_by_status`, `update_with_revision` (UPDATE ... WHERE revision = ?), `append_event`, `list_events`
- Schema idempotente, foreign keys ON
- **Done:** 8 unit tests verde (2 types + 6 store): insert/get round trip, list filters, revision conflict detection, event log append/list. Workspace build verde.

### 14.2 — Flow types + state machine   ✅
- `FlowStatus::can_transition_to(next)` con tabla legal: Created→Running, Running→Waiting/Finished/Failed, Waiting→Running/Failed, **cancel desde cualquier no-terminal**
- `Flow::transition_to(next)` aplica + valida + actualiza `updated_at`
- `Flow::request_cancel()` flag sticky idempotente; bloquea cualquier transición no-Cancelled
- Errors: `IllegalTransition`, `AlreadyTerminal`, `CancelPending`
- 6 unit tests nuevos: legal sequence, illegal rejected, cancel from any non-terminal, terminal rejects, cancel_requested blocks non-cancel, request_cancel idempotent
- **Done:** 14 tests verde total en el crate (8 store + 6 types). Workspace build verde.

### 14.3 — Managed flow API (FlowManager)   ✅
- `FlowManager::new(Arc<dyn FlowStore>)` — store-agnostic (in-mem/SQLite/futuro NATS)
- API: `create_managed`, `start_running`, `set_waiting`, `resume`, `finish`, `fail`, `request_cancel`, `cancel`, `update_state`
- Cada método: read → mutate → `transition_to` → `update_with_revision` → audit event
- Retry-on-conflict 1× (RETRY_ATTEMPTS=2): refetch automático para race heartbeat-vs-tool, sin livelock
- `set_waiting` persiste `wait_json` (timer/external/manual); `resume` lo limpia + permite shallow-merge en `state_json`
- `fail(reason)` estampa `state_json.failure = {reason, at}` y deja audit event
- `update_state(patch, next_step?)` mutación de estado sin cambiar status; rechazada si `cancel_requested` o terminal
- Shallow merge: keys top-level del patch sobreescriben las de `state_json`; non-object reemplaza entero
- 8 unit tests nuevos: happy path completo, fail con reason, cancel mid-flight, request_cancel bloquea finish, update_state preserva status + merge, cancel_pending bloquea update_state, audit event en create, double finish → AlreadyTerminal
- **Done:** 22 tests verde total en el crate; workspace build verde.

### 14.4 — Wait/resume engine (heartbeat-driven)   ✅
- `WaitCondition { Timer { at }, ExternalEvent { topic, correlation_id }, Manual }` con serde tagged enum (`kind` discriminator)
- Persistencia en `flow.wait_json`; parsing tolerante vía `WaitCondition::from_value`
- `WaitEngine::tick_at(now)` escanea flows en `Waiting`, aplica `evaluate(flow, now)`:
  - `cancel_requested` → flip a `Cancelled` (prioridad sobre wait)
  - `Timer { at }` con `now >= at` → `resume(None)`
  - `Timer { at }` con `now < at` → permanecer Waiting
  - `ExternalEvent`/`Manual` → permanecer Waiting (necesitan señal explícita)
- `WaitEngine::try_resume_external(flow_id, topic, correlation_id, payload)` host-driven (NATS bridge lo invoca); payload se persiste como `state.resume_event`
- `WaitEngine::run(interval, shutdown_token)` long-running loop para cron/heartbeat
- `TickReport { scanned, resumed, cancelled, still_waiting, errors }` para métricas
- Engine **broker-agnostic**: no importa `agent-broker`; host wirea el bridge NATS → `try_resume_external`
- 10 tests nuevos: timer fires past deadline, timer doesn't fire early, external matches resumes + clears wait_json, external no-match no-op, manual ignored by tick, cancel_requested flips to cancelled, run loop honra shutdown token, unknown flow no-op, running flow no-op, WaitCondition round-trip
- **Done:** 32 tests verde total (8 store + 6 transitions + 8 manager + 10 engine). Workspace build verde, zero warnings.

### 14.5 — Agent tool integration (TaskFlowTool)   ✅
- Tool único `taskflow` con dispatch por `action`: `start|status|advance|cancel|list_mine`
- Archivo `crates/core/src/agent/taskflow_tool.rs`; wire en `agent/mod.rs` (`pub use TaskFlowTool`)
- `crates/core` añade dep `agent-taskflow`
- `owner_session_key` derivado de `agent:{agent_id}:session:{session_id}` — aisla flows por sesión y bloquea cross-session access
- Auto-start en `start` action (Created → Running inmediato) para UX del LLM
- Revision handling **oculto al LLM** — la tool siempre refetcha antes de mutar
- `list_mine` usa `list_by_owner` para devolver solo los flows de esta sesión
- `advance` hace shallow-merge sobre `state_json` + optional `current_step` update
- 8 unit tests en `taskflow_tool.rs`:
  - start crea + running flow
  - status devuelve el estado actual
  - advance merge shallow + actualiza step
  - cancel → Cancelled
  - list_mine filtra por sesión (multi-session setup)
  - cross-session access rechazado con error "different session"
  - falta session_id → error
  - unknown flow_id → `{ok:false, error:"not_found"}`
- **Done:** 8 tests verde, agent-core lib total pasa a 107 (antes 99); workspace build verde.

### 14.6 — Mirrored mode + CLI commands   ✅
- `FlowStore` extendido: `insert_step`, `update_step`, `get_step`, `list_steps`, `find_step_by_run_id`
- `FlowManager::create_mirrored(input)` crea + auto-start Running
- `FlowManager::record_step_observation(StepObservation)` upsert-style por `(flow_id, run_id)`: inserta si nuevo, actualiza si existe; preserva `child_session_key` cuando la observation no lo trae
- `FlowManager::list_steps(flow_id)` para inspección
- Cada observation emite audit event `step_observed` con runtime + status
- **Engine sigue broker-agnostic**: host bridge (NATS subscriber, CLI task, cron) llama `record_step_observation`
- CLI en `src/main.rs`:
  - `agent flow list [--json]` — tabla de todos los flows (sort by `updated_at` DESC)
  - `agent flow show <id> [--json]` — detalle + steps
  - `agent flow cancel <id>` — llama `manager.cancel`
  - `agent flow resume <id>` — llama `manager.resume` manual
  - `agent flow` → help text
- Path SQLite vía `TASKFLOW_DB_PATH` env (default `./data/taskflow.db`)
- 9 tests nuevos: 5 store step CRUD + 4 manager mirrored
- Smoke verificado: `agent flow` help + `list` empty state
- **Done:** 41 tests verde en agent-taskflow; workspace build + full lib tests verde; CLI binario smoke passing.

### 14.7 — Integration tests + restart durability + skill doc   ✅
- `crates/taskflow/tests/e2e_test.rs` con 4 tests multi-thread:
  - `flow_state_survives_reopen`: escribe flow (run + update + wait) → drop store → reopen mismo path → verifica `current_step`, `state_json`, `wait_json`, `revision=3`; luego `resume` funciona
  - `concurrent_mutations_serialize_via_revision_retry`: 2 tasks simultáneas → ambas succeed por retry interno → final revision +2, ambos patches presentes
  - `heavy_contention_surfaces_revision_mismatch`: 10 tasks concurrentes → ok + conflict ≤ 10, invariante `revision == 1 + ok`
  - `mirrored_steps_survive_reopen`: 3 step observations persistidas → reopen → list_steps devuelve 3 en orden correcto
- `skills/taskflow/SKILL.md` nuevo con frontmatter Phase 13.7 (name + description + requires)
- `crates/taskflow/README.md` con layout, quick start, tests, CLI, error codes, related phases
- FOLLOWUPS entry con 9 items pendientes explícitos (heartbeat wiring, NATS bridge, set_waiting/finish LLM actions, etc.)
- **Done:** 45 tests totales en agent-taskflow (41 unit + 4 integration), workspace build verde.

---

## Phase 15 — Claude subscription auth

**Goal:** allow the Anthropic provider to authenticate with API key,
`claude setup-token`, imported Claude Code CLI credentials, or a raw
OAuth bundle with auto-refresh — all configurable through
`agent setup anthropic`.

### 15.1 — Config schema ✅

Extend `LlmAuthConfig` with `setup_token_file`, `refresh_endpoint`,
`client_id`. YAML parsing tests for the 5 modes
(`api_key | setup_token | oauth_bundle | cli_import | auto`).

### 15.2 — `anthropic_auth.rs` (bundle + OAuthState refresh) ✅

`OAuthBundle` with atomic save, `AnthropicAuth` enum (ApiKey /
SetupToken / OAuth), `OAuthState` with refresh mutex against
`https://console.anthropic.com/v1/oauth/token`, rotation persisted.

### 15.3 — Claude CLI credentials reader ✅

`read_claude_cli_credentials()` parses `~/.claude/.credentials.json`
and reads the macOS Keychain entry `Claude Code-credentials`. Converts
`expiresAt` (ms) to unix seconds.

### 15.4 — `AnthropicClient` uses `AnthropicAuth` ✅

`AnthropicClient::new() -> Result<Self>`, `resolve_headers()` per
request (x-api-key vs Authorization + `anthropic-beta`), classifies
401/403 as `LlmError::CredentialInvalid` (not retried, not counted by
breaker), marks OAuth state stale.

### 15.5 — Setup wizard ✅

`services/llm.rs::anthropic` expanded with `auth_mode` select (4
options). `writer::persist_anthropic()` branches: api_key → secrets
file; setup_token → validates prefix + length; cli_import → reads
`~/.claude/.credentials.json`, converts to our bundle shape;
oauth_bundle → accepts pasted JSON. All branches patch
`llm.yaml::providers.anthropic.auth.*`.

### 15.6 — Error classification ✅

`LlmError::CredentialInvalid` variant added; `with_retry` does not
retry it; HTTP 401/403 from the Anthropic endpoint maps to it with a
hint pointing the operator at `agent setup anthropic`.

### 15.8 — OAuth browser login flow (PKCE) ✅

`services/anthropic_oauth.rs` nuevo: PKCE authorization_code flow
contra `https://claude.ai/oauth/authorize` + `https://console.anthropic.com/v1/oauth/token`.
Muestra URL, abre browser (best-effort), user pega `<code>#<state>`,
exchange → `OAuthToken`. Nuevo modo `oauth_login` en wizard que
ejecuta el flow y persiste bundle. Estado CSRF verificado. Soporta
pegar URL completa o `code#state` directo.

### 15.7 — Docs + YAML example ✅

`config/llm.yaml` ships with a commented `auth.mode: auto` block.
`FOLLOWUPS.md` records: multi-profile round-robin deferred, device
code flow deferred, live smoke test gated (skipped in default CI).

---

## Phase 16 — Per-binding capability override

A single agent can now expose distinct capability surfaces per
`InboundBinding` — narrow sales tools on WhatsApp, full power on a
private Telegram channel, no process duplication. Shared identity,
shared workspace, shared memory; per-channel policy.

### 16.1 — Schema (InboundBinding overrides) ✅

`InboundBinding` gains optional overrides: `allowed_tools`,
`outbound_allowlist`, `skills`, `model`, `system_prompt_extra`,
`sender_rate_limit` (untagged enum `inherit | disable | {rps, burst}`),
`allowed_delegates`. `ModelConfig: Clone`, `InboundBinding: Default`
so existing struct literals spread with `..Default::default()`. Seven
YAML parse tests lock down every form including `deny_unknown_fields`.

### 16.2 — EffectiveBindingPolicy + merge rules ✅

`crates/core/src/agent/effective.rs`: concrete capability snapshot
built once by `resolve(&AgentConfig, binding_index)`. Merge rules:
replace for lists/structs, append for `system_prompt_extra` as a
`# CHANNEL ADDENDUM` block, inherit/disable/config for rate-limit.
`from_agent_defaults` synthesises a policy for unbound paths
(delegation, heartbeat) keyed at `binding_index = usize::MAX`.
`tool_allowed()` + a shared `allowlist_matches()` helper keep agent-
level and per-binding matching in one place. 13 unit tests.

### 16.3 — Boot validation ✅

`binding_validate.rs` fails boot on duplicate `(plugin, instance)`,
unknown telegram instance, missing skill directories, unknown tool
names (when a catalogue is supplied), and provider mismatches
between agent and binding. Soft warn on bindings with no overrides.
Hooked in `src/main.rs` right after `AppConfig::load`. 13 unit tests.

### 16.4 — AgentContext + registry cache ✅

`AgentContext` gains `effective: Option<Arc<EffectiveBindingPolicy>>`
with an `effective_policy()` helper that falls back to agent-level
defaults. `ToolRegistryCache` (`DashMap<(AgentId, usize),
Arc<ToolRegistry>>`) uses `entry()` for atomic `get_or_build`. Base
registry stays authoritative; per-binding filtered clones share
handlers. 7 unit tests.

### 16.5 — Runtime intake + rate limiter ✅

`match_binding_index` replaces the `binding_matches` bool; runtime
picks the matched index, looks up the pre-resolved policy (allocated
once at `AgentRuntime::new` to keep the hot path a single `Arc`
clone), attaches it to the session `AgentContext`. Sender rate
limiter is now per-binding keyed by `binding_index`, so flood on one
channel cannot exhaust the quota on another.

### 16.6 — LLM, prompt, skills, outbound, delegation ✅

`llm_behavior` reads `effective.model`, `effective.skills`,
`effective.system_prompt`, `effective.allowed_delegates`. Tool list
shown to the LLM and tool-call execution both consult
`effective.tool_allowed(name)` (defense-in-depth). `whatsapp_*` and
`telegram_*` outbound tools read the per-binding allowlist from
`ctx.effective_policy()`. Agent-level boot prune is skipped when
bindings exist so per-binding overrides can both narrow and expand
within the registry.

### 16.7 — Example YAML + end-to-end tests ✅

`config/agents.d/ana.per-binding.example.yaml` ships a two-binding
Ana (WA sales narrow + TG full power). Integration suite:
`crates/core/tests/per_binding_override_test.rs` — 5 end-to-end
tests covering both-bindings dispatch, unmatched drop, legacy
fallback, per-binding rate limit isolation, and defense-in-depth.
All green; back-compat for bindingless agents verified byte-for-byte.

**Progress: 7/7 sub-phases done. Follow-ups tracked in `FOLLOWUPS.md`
under "Per-binding capability override" (tool-name check at boot,
aggregate validate errors, wildcard/specific overlap warning,
provider registry check, skills CWD, hot-reload, sentinel design).**

---

## Phase 17 — Per-agent credentials (WhatsApp / Telegram / Google)

**Goal:** Each agent declares which plugin instance / Google account it
uses for outbound traffic. Outbound tools resolve the target instance
from the agent's binding, not from LLM args. Boot-time gauntlet
validates every invariant (missing instance, path overlap, insecure
file permissions, cross-agent intent) in a single pass so operators
fix their YAML in one edit.

### 17.1 — `agent-auth` scaffold + opaque handle ✅
- New crate `crates/auth/` with `CredentialHandle` (Debug redacts
  raw id, fingerprint = `sha256(account_id)[..8]`), `CredentialError`,
  `BuildError`, `CredentialStore` trait.
- **Done:** `cargo test -p agent-auth` green; handle Debug proven not
  to leak raw id; fingerprint pinned to known vector.

### 17.2 — Boot gauntlet (paths + permissions) ✅
- Pure functions: `canonicalize_session_dirs`, `check_duplicate_paths`,
  `check_prefix_overlap`, `check_permissions` (linux 0o077 mask, skip
  `/run/secrets/`, env override `CHAT_AUTH_SKIP_PERM_CHECK=1`).
- **Done:** accumulative error reporting; 5 unit tests.

### 17.3 — Per-channel stores (WA + TG + Google) ✅
- `WhatsappCredentialStore`, `TelegramCredentialStore` (token redacted
  in `Debug`), `GoogleCredentialStore` with per-fingerprint
  `tokio::Mutex` refresh lock.
- **Done:** 1:1 `agent_id` rule for Google; `allow_agents` filter for
  WA/TG; 10 unit tests.

### 17.4 — Resolver with invariant accumulation ✅
- `AgentCredentialResolver::build` returns `Err(Vec<BuildError>)` so
  every violation surfaces in one pass. Checks: missing instance,
  ambiguous inbound, allow_agents exclusion, asymmetric binding
  (warn in Lenient, error in Strict), single-inbound inference.
- **Done:** 9 tests covering every invariant, including
  `boot_reports_all_errors_in_one_pass`.

### 17.5 — Prometheus telemetry ✅
- 9 series: `credentials_accounts_total`, `credentials_bindings_total`,
  `channel_account_usage_total`, `channel_acl_denied_total`,
  `credentials_resolve_errors_total`, `credentials_breaker_state`,
  `credentials_boot_validation_errors_total`,
  `credentials_insecure_paths_total`,
  `credentials_google_token_refresh_total`.
- **Done:** `render_prometheus()` returns deterministic ordering;
  sample test asserts every TYPE line is present.

### 17.6 — Config schemas ✅
- `AgentConfig.credentials: { whatsapp, telegram, google,
  <channel>_asymmetric }`.
- `WhatsappPluginConfig.allow_agents`, `TelegramPluginConfig.allow_agents`.
- Optional `config/plugins/google-auth.yaml` with
  `accounts: [{id, agent_id, client_id_path, client_secret_path,
  token_path, scopes}]`.
- `AccountConfig.agent_id: Option<String>` in gmail-poller (defaults
  to `id` for back-compat).
- **Done:** 4 deserialisation tests.

### 17.7 — `agent --check-config [--strict]` ✅
- New CLI subcommand runs the gauntlet + resolver against the loaded
  config and prints a report. Exit 0 clean, 1 errors, 2 warnings-only.
- **Done:** validated against the real `config/` — catches dangling
  `credentials.telegram='ana_tg'` binding in
  `ana.per-binding.example.yaml`.

### 17.8 — Runtime integration ✅
- `AgentContext.credentials: Option<Arc<AgentCredentialResolver>>`
  + `with_credentials()` builder. `AgentRuntime` threads it into
  every `AgentContext` it constructs.
- `main.rs` runs the gauntlet (lenient) at boot, attaches the resolver
  to every runtime; errors are logged but don't abort (legacy configs
  keep working).
- **Done:** `cargo build --workspace` + tests green.

### 17.9 — Plugin tool migration ✅
- `whatsapp_*` / `telegram_*` tools publish to
  `plugin.outbound.<ch>.<instance>` when resolver yields a handle;
  fall back to legacy bare topic otherwise. Emit `audit_outbound`
  + `inc_usage{direction=outbound}` on every publish.
- **Done:** unlabelled instances keep legacy topic (100% back-compat).

### 17.10 — Google tool store lookup ✅
- `main.rs` registers `google_*` tools either from the legacy inline
  `agents.<id>.google_auth` block or from a matching entry in
  `GoogleCredentialStore`. Inline → store migration happens
  transparently in `build_credentials` (prefix `inline:` so the
  gauntlet skips file-exists).
- **Done:** agents without either source simply don't see `google_*`.

### 17.11 — E2E isolation + fingerprint stability ✅
- `crates/auth/tests/cross_agent_isolation.rs` — two agents, two WA
  instances, two TG bots; verifies each resolves their own accounts
  and Kate cannot bind to Ana's instance (boot rejects with
  `AllowAgentsExcludes`).
- `crates/auth/tests/fingerprint_stability.rs` — pins sha256 output
  to a known vector + 1000-id no-collision smoke.
- **Done:** 47 tests across `agent-auth`, no flakes.

**Progress: 11/11 sub-phases done.**

Deferred to follow-up (no current demand):
- Circuit breaker per `(channel, instance)` at the dispatch layer
  (today WA/TG plugins share the global breaker already covered by
  Phase 2.5).
- Hot-reload of `credentials` block without restart.
- `agent setup google --account <id> --agent <agent_id>` CLI.

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
