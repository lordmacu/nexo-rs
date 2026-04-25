# Agent Framework - Microservices Architecture

## Overview

Multi-agent system with robust microservices architecture, event-driven communication via message broker, and LLM-powered decision making. Designed for horizontal scalability and fault isolation.

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           MESSAGE BROKER                                 │
│                     (NATS / RabbitMQ / Kafka)                           │
│                                                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────┐ │
│  │ WhatsApp │  │ Browser  │  │ Telegram │  │  Email   │  │  ...   │ │
│  │  Plugin  │  │  Plugin  │  │  Plugin  │  │  Plugin  │  │ Plugin │ │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬────┘ │
│       │            │            │            │               │        │
└───────┼────────────┼────────────┼────────────┼───────────────┼────────┘
        │            │            │            │               │
        ▼            ▼            ▼            ▼               ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                         AGENT CORE (Rust)                               │
│  ┌────────────────────────────────────────────────────────────────────┐ │
│  │                      EVENT BUS (in-memory + disk)                   │ │
│  │                                                                       │ │
│  │   ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐           │ │
│  │   │SessionMgr│  │  LLM     │  │  Tool    │  │ Memory   │           │ │
│  │   │          │  │ Client   │  │ Registry │  │ Manager  │           │ │
│  │   └──────────┘  └──────────┘  └──────────┘  └──────────┘           │ │
│  └────────────────────────────────────────────────────────────────────┘ │
│                              │                                           │
│                              ▼                                           │
│  ┌────────────────────────────────────────────────────────────────────┐ │
│  │                    AGENTS (configurable)                            │ │
│  │   ┌────────┐  ┌────────┐  ┌────────┐                                │ │
│  │   │Agent A │  │Agent B │  │Agent C │  ...                           │ │
│  │   └────────┘  └────────┘  └────────┘                                │ │
│  └────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌──────────────────┐
                    │   MEMORY STORE   │
                    │ (Redis + SQLite) │
                    └──────────────────┘
```

## Core Components

### 1. Message Broker (Transport Layer)

**Options:**
- **NATS** (recommended) - Lightweight, high-performance, Rust-native client (`async-nats`)
- **RabbitMQ** - More features, heavier
- **Kafka** - Best for massive scale

**Topic Structure:**
```
agent.events.{agent_id}.{event_type}
agent.commands.{agent_id}
agent.responses.{session_id}
agent.route.{target_agent_id}          # Agent-to-agent communication
plugin.inbound.{plugin_name}
plugin.outbound.{plugin_name}
```

**Message Format:**
```json
{
  "id": "uuid",
  "timestamp": "2026-01-01T00:00:00Z",
  "topic": "agent.events.kate.click",
  "source": "browser",
  "session_id": "uuid",
  "payload": {
    "type": "click",
    "target": "@e1",
    "metadata": {}
  }
}
```

### 2. Plugin System

Each platform (WhatsApp, Browser, Telegram, etc) is a plugin that:
- Connects to the message broker
- Translates platform events → agent messages
- Translates agent commands → platform actions

**Plugin Interface:**
```rust
#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self, broker: &BrokerHandle) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn send_command(&self, cmd: &Command) -> Result<Response>;
}
```

**Built-in Plugins:**
- `whatsapp-rs` - WhatsApp integration (Signal Protocol + QR pairing, already implemented)
- `browser CDP` - Chrome DevTools Protocol control
- `telegram` - Telegram Bot API
- `email` - SMTP/IMAP integration

### 3. Browser Control (Low-Level CDP)

Direct Chrome DevTools Protocol access for maximum control:

```rust
// CDP Command types
pub enum BrowserCommand {
    Navigate { url: String },
    Click { selector: String },
    Fill { selector: String, value: String },
    Screenshot,
    Snapshot,
    Evaluate { script: String },
    Wait { condition: WaitCondition },
    // ... full CDP coverage
}

// Browser Plugin subscribes to commands and publishes events
// Events: page_load, navigation, element_update, download_request, etc.
```

**Features:**
- Direct WebSocket connection to Chrome CDP
- Automatic Chrome discovery/launch
- Session management with state persistence
- Element reference system (@e1, @e2) with lifecycle tracking
- Full CDP coverage: Network, Page, Runtime, DOM, Input, Target

### 4. Agent System

**Agent Definition (YAML):**
```yaml
agent:
  id: "kate"
  description: "Personal assistant for Cristian"
  model:
    provider: "minimax"
    model: "MiniMax-M2.5"
  system_prompt: |
    You are Kate. Read IDENTITY.md and MEMORY.md at start.
  heartbeat:
    enabled: true
    interval: "5m"
  channels:
    - whatsapp
    - telegram
  tools:
    - browser
    - memory
    - gmail
  memory:
    short_term: true
    long_term: true
```

**Agent Definition (Rust):**
```rust
pub struct Agent {
    id: String,
    model: ModelConfig,
    system_prompt: String,
    plugins: Vec<Box<dyn ToolPlugin>>,
    memory: MemoryConfig,
    heartbeat: Option<HeartbeatConfig>,
}

#[async_trait]
pub trait AgentBehavior: Send + Sync {
    async fn on_message(&self, ctx: &Context, msg: &Message) -> Result<Response>;
    async fn on_event(&self, ctx: &Context, event: &Event) -> Result<()>;
    async fn on_heartbeat(&self, ctx: &Context) -> Result<()>;
    async fn decide(&self, ctx: &Context, state: &AgentState) -> Result<Action>;
}
```

### 5. Heartbeat System

Agents with `heartbeat.enabled: true` fire `on_heartbeat` on every interval tick.

**Use cases:**
- Check pending tasks / reminders
- Send proactive messages to user
- Sync external state (calendar, email, etc.)
- Cleanup stale sessions

```rust
pub struct HeartbeatConfig {
    pub enabled: bool,
    pub interval: Duration,
}

// Runtime schedules a tokio interval per agent
// On tick: publishes `agent.events.{agent_id}.heartbeat`
// Agent receives and calls on_heartbeat()
```

### 6. LLM Integration

**Client Interface:**
```rust
pub trait LLMClient: Send + Sync {
    async fn complete(&self, prompt: &Prompt) -> Result<Completion>;
    async fn stream(&self, prompt: &Prompt, cb: impl FnMut(String)) -> Result<()>;
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
}
```

**Providers:**
- **MiniMax** (primary — `MiniMax-M2.5`) — `crates/llm/src/minimax.rs`
- OpenAI (GPT-4, GPT-3.5) — `crates/llm/src/openai.rs`
- Anthropic (Claude) — `crates/llm/src/anthropic.rs`
- Ollama (local models) — `crates/llm/src/ollama.rs`
- Custom OpenAI-compatible APIs

**Tool Calling:**
```rust
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

// Agent receives tool definitions and decides which to call
// LLM returns JSON with tool calls
// Agent executes via ToolRegistry
```

**Rate Limiting & Quota:**
```rust
pub struct RateLimiter {
    requests_per_second: f32,
    token_bucket: TokenBucket,
    quota_tracker: Option<QuotaTracker>,
}

// On 429 response: exponential backoff starting at 1s, max 60s
// QuotaTracker alerts when remaining quota < threshold
// Configurable per provider in llm.yaml
```

### 7. Memory System

**Short-term (in-process):**
```rust
pub struct ShortTermMemory {
    sessions: HashMap<SessionId, Session>,
    context: RollingWindow<Interaction>,
}

pub struct Session {
    id: SessionId,
    agent_id: String,
    history: Vec<Interaction>,
    context: Value,
    created_at: DateTime,
    last_access: DateTime,
}
```

**Long-term (disk/Redis):**
```rust
pub struct LongTermMemory {
    store: SqliteStore,          // conversation history + agent facts
    index: Option<VectorIndex>,  // semantic search via sqlite-vec
}

// Stores:
// - Conversation history
// - Agent memories (important facts)
// - Learned patterns
// - User preferences
```

**Vector Search:**

Using `sqlite-vec` (zero extra infra) or `qdrant` for distributed setups.

```toml
# sqlite-vec — embedded, no external service
sqlite-vec = "0.1"
```

**Memory Operations:**
- `store(session_id, interaction)` - Save interaction
- `recall(query, limit)` - Semantic search via vector index
- `context(session_id, max_turns)` - Build context for LLM

### 8. Agent-to-Agent Communication

Agents publish to `agent.route.{target_id}` to delegate tasks or share results.

```rust
pub struct AgentMessage {
    pub from: String,
    pub to: String,
    pub correlation_id: Uuid,
    pub payload: AgentPayload,
}

pub enum AgentPayload {
    Delegate { task: String, context: Value },
    Result { task_id: Uuid, output: Value },
    Broadcast { event: String, data: Value },
}
```

**Example flow:**
```
kate (receives user request)
    → delegates research to "research" agent
    → publishes to `agent.route.research`
    → research agent processes, publishes result to `agent.route.kate`
    → kate receives result, composes response
```

### 9. Secrets Management

API keys and credentials never live in YAML config files.

**Priority order (runtime resolution):**
1. Environment variables (Docker secrets / `.env`)
2. File secrets (`/run/secrets/` in Docker)
3. Encrypted secrets file (`secrets.enc.toml` + key from env)

```yaml
# config/llm.yaml — references env vars, not values
providers:
  minimax:
    api_key: "${MINIMAX_API_KEY}"
    group_id: "${MINIMAX_GROUP_ID}"
    base_url: "https://api.minimax.chat/v1"
  openai:
    api_key: "${OPENAI_API_KEY}"

# config/plugins/whatsapp.yaml
whatsapp:
  session_dir: "/data/sessions"   # persisted volume
  credentials_file: "${WA_CREDENTIALS_FILE}"
```

**Docker Compose secrets pattern:**
```yaml
services:
  agent:
    secrets:
      - minimax_api_key
      - telegram_token
    environment:
      MINIMAX_API_KEY_FILE: /run/secrets/minimax_api_key

secrets:
  minimax_api_key:
    file: ./secrets/minimax_api_key.txt
```

### 10. Fault Tolerance & Circuit Breaker

```rust
pub struct CircuitBreaker {
    state: CircuitState,         // Closed | Open | HalfOpen
    failure_threshold: u32,
    recovery_timeout: Duration,
    last_failure: Option<Instant>,
}

pub enum CircuitState {
    Closed,    // normal operation
    Open,      // reject requests immediately
    HalfOpen,  // probe: allow one request through
}
```

**Retry policy per component:**

| Component | Strategy | Max attempts | Backoff |
|-----------|----------|-------------|---------|
| LLM call (429) | Exponential | 5 | 1s → 60s |
| LLM call (5xx) | Exponential | 3 | 2s → 30s |
| NATS publish | Fixed | 3 | 100ms |
| CDP command | Fixed | 2 | 500ms |
| Plugin restart | Linear | 5 | 5s |

**NATS offline fallback:**
- EventBus switches to local `tokio::mpsc` channels
- Pending events persist to disk queue (`./data/queue/`)
- On reconnect: drain disk queue → publish to NATS

### 11. Event Bus

```rust
pub struct EventBus {
    broker: NatsClient,
    subscriptions: HashMap<SubscriptionId, Handler>,
    local_queue: Channel<Event>,
    persistence: Option<PersistentQueue>,
    circuit_breakers: HashMap<String, CircuitBreaker>,
}

impl EventBus {
    pub async fn subscribe(&mut self, topic: &str, handler: Handler) -> Result<SubscriptionId>;
    pub async fn publish(&self, topic: &str, event: Event) -> Result<()>;
    pub async fn request(&self, topic: &str, msg: Message) -> Result<Response>;
}
```

**Features:**
- In-memory fast path for local events
- Broker integration for distributed delivery
- Persistent queue for offline delivery
- Dead letter queue for failed messages
- Backpressure handling
- Circuit breaker per topic

## Data Flow

### 1. Inbound (User → Agent)

```
WhatsApp message
    → Plugin (whatsapp-rs) parses
    → Publish to `plugin.inbound.whatsapp`
    → EventBus routes to relevant agents
    → Agent processes via LLM
    → Decision: respond, act (browser), store memory
    → If response: publish to `plugin.outbound.whatsapp`
    → Plugin sends message back to WhatsApp
```

### 2. Outbound (Agent → Browser)

```
Agent decides: "click @e1"
    → ToolCall to BrowserTool
    → BrowserTool publishes to `plugin.outbound.browser`
    → BrowserPlugin receives command
    → Executes CDP: Input.dispatchMouseEvent
    → Publishes result event
    → Agent receives confirmation
```

### 3. Async Event (Browser → Agent)

```
Page loads (CDP event)
    → BrowserPlugin captures
    → Publishes to `agent.events.{agent_id}.page_load`
    → EventBus delivers to subscribed agent
    → Agent decides: need more info? snapshot
    → Agent subscribes to next event
```

### 4. Agent-to-Agent Delegation

```
kate receives complex request
    → Publishes to `agent.route.research`
    → research agent processes independently
    → Publishes result to `agent.route.kate` with correlation_id
    → kate correlates response, continues conversation
```

## Configuration

### config/agents.yaml

```yaml
agents:
  - id: "kate"
    model:
      provider: "minimax"
      model: "MiniMax-M2.5"
    plugins:
      - whatsapp
      - browser
      - memory
    heartbeat:
      enabled: true
      interval: "5m"
    workspace: "./data/workspace/kate"
    config:
      debounce_ms: 2000
      queue_cap: 5

  - id: "ventas"
    model:
      provider: "minimax"
      model: "MiniMax-M2.5"
    workspace: "./data/workspace/ventas"
    extra_docs:
      - "SALES_SCRIPT.md"
      - "PRODUCT_CATALOG.md"
    inbound_bindings:
      - plugin: telegram
        instance: bot_sales
    allowed_tools:
      - "memory_*"
      - "ext_weather_*"
      - "delegate"
```

### Per-agent isolation

Each agent can be tuned independently along several axes. Every
setting defaults to "off / wildcard" so legacy single-agent configs
keep working unchanged.

| Setting | Purpose | Empty default |
|---|---|---|
| `workspace` | Per-agent `IDENTITY.md` / `SOUL.md` / `USER.md` / `AGENTS.md` / `MEMORY.md` + daily notes. Loaded at turn start. | No workspace layer. |
| `extra_docs` | Additional workspace-relative MDs rendered as `# RULES — <filename>` blocks. Scoped context (sales script, product catalog) kept out of SOUL.md. | No extra blocks. |
| `inbound_bindings` | Allowlist of `(plugin, instance?)` pairs the agent accepts. Non-matching events are dropped at runtime. | Wildcard — receive every inbound. |
| `allowed_tools` | Strict glob allowlist of tool names. Non-matching tools are pruned from the agent's `ToolRegistry` at build time so the LLM never sees them. | All registered tools callable. |
| `allowed_delegates` | Allowlist of peer agent ids this one can call via `delegate`. Rejected at tool-call time with a clear error. | Delegate to anyone. |
| `sender_rate_limit` | Token bucket per `(agent_id, sender_id)` applied before the message is enqueued. Denies are silently dropped (no oracle for spammers). | Unlimited. |
| `description` | One-line role summary shown in other agents' `# PEERS` block. | No annotation next to the id. |

Memory stays partitioned by `agent_id` in SQL columns regardless of
setup, so two agents sharing `memory.db` never see each other's rows.

### Peer directory

`PeerDirectory` is built once at boot from every `AgentConfig` and
rendered as a `# PEERS` block right after workspace and before the
inline `system_prompt`. Each agent's view filters itself out and
annotates peers with `✓` / `✗` based on its own `allowed_delegates`:

```markdown
# PEERS
Other agents you can reach via `delegate({agent_id, task, ...})`:

- ✗ `boss` — takes decisions
- ✓ `soporte_lvl1` — first-line support
- ✓ `ventas` — closes deals
```

This replaces the need to hand-write each workspace's `AGENTS.md` in
multi-agent setups. A user-written `AGENTS.md` still loads on top.

### Multi-instance plugins

Some plugins support more than one "account" in a single process.
`TelegramPluginConfig.telegram` in `plugins/telegram.yaml` accepts
either a single map (legacy) or a sequence of bots:

```yaml
telegram:
  - token: ${TELEGRAM_BOT_BOSS}
    instance: boss
    allowlist: { chat_ids: [...] }
  - token: ${TELEGRAM_BOT_SALES}
    instance: sales
    allowlist: { chat_ids: [...] }
```

Each instance publishes to `plugin.inbound.telegram.<instance>` and
subscribes to `plugin.outbound.telegram.<instance>`. Agents target a
specific bot with `inbound_bindings: [{plugin: telegram, instance: X}]`;
replies carry `source_instance` so `llm_behavior` routes outbound to
the matching bot. Unlabelled instances fall through to the legacy
`plugin.inbound.telegram` / `plugin.outbound.telegram` topics.

The same mechanism applies to WhatsApp: `whatsapp:` accepts a sequence
of accounts and each gets an isolated `FileStore` (rooted at
`<session_dir>/.whatsapp-rs`) via `whatsapp_rs::Client::new_in_dir`.
No process-wide `XDG_DATA_HOME` mutation, so Signal keys are never
shared between accounts.

The health server exposes per-instance pairing UIs alongside the
legacy routes:

| Route | Target |
|---|---|
| `/whatsapp/instances` | JSON array of registered instance labels |
| `/whatsapp/pair[/qr\|/status]` | First instance (legacy single-account) |
| `/whatsapp/<instance>/pair[/qr\|/status]` | Named instance |

The HTML page is shared; its JS derives the QR/status URLs from
`window.location.pathname` so opening `/whatsapp/biz/pair` in a
browser polls `/whatsapp/biz/pair/qr` and `/whatsapp/biz/pair/status`
without any template-time baking.

### Tool-execution policy

`config/tool_policy.yaml` (optional) controls caching, bounded
parallelism, and relevance filtering. Per-agent overrides fully
replace the global settings for named agents:

```yaml
cache:
  ttl_secs: 60
  tools: ["ext_weather_*", "ext_wikipedia_*"]
  max_entries: 1024
  max_value_bytes: 262144          # skip cache for payloads > 256 KiB
parallel_safe: ["ext_weather_*", "ext_wikipedia_*"]
parallel:
  max_in_flight: 4
  call_timeout_secs: 30
relevance:
  enabled: true
  top_k: 24
  min_score: 0.01
  always_include: ["delegate", "memory_*"]
per_agent:
  kate:
    parallel_safe: ["ext_weather_*"]
    parallel: { max_in_flight: 2, call_timeout_secs: 5 }
```

### Admin HTTP (loopback `127.0.0.1:9091`)

All routes bind to loopback only (no auth). For remote ops, ssh-tunnel
`-L 9091:127.0.0.1:9091`.

| Route | Purpose |
|---|---|
| `GET /admin/agents` | JSON array of every agent's id, description, model, bindings, `allowed_tools`, `allowed_delegates`, `extra_docs`, sender rate-limit flag, workspace flag. |
| `GET /admin/agents/<id>` | Same shape as one entry above, for a single agent. 404 when the id isn't registered. |
| `GET /admin/tool-cache/stats` | `{entries, per_agent_overrides}` |
| `POST /admin/tool-cache/clear` | Drop every cache entry |
| `POST /admin/tool-cache/invalidate?agent=X&tool=Y` | Scoped purge |

### CLI

| Command | Purpose |
|---|---|
| `agent` | Start the daemon (default) |
| `agent status [<id>] [--json] [--endpoint=URL]` | Query the admin endpoint and pretty-print the agent directory; pass `<id>` to narrow to one agent (`/admin/agents/<id>`). |
| `agent --dry-run [--json]` | Load config, validate env vars + fields, print a summary, exit 0. CI pre-deploy gate. |

### config/broker.yaml

```yaml
broker:
  type: "nats"
  url: "nats://localhost:4222"
  auth:
    enabled: true
    nkey_file: "/run/secrets/nats_nkey"
  persistence:
    enabled: true
    path: "./data/queue"
  limits:
    max_payload: "4MB"
    max_pending: 10000
  fallback:
    mode: "local_queue"         # use in-memory + disk if NATS unreachable
    drain_on_reconnect: true
```

### config/browser.yaml

```yaml
browser:
  cdp_url: "http://127.0.0.1:9222"
  auto_connect: true
  profiles:
    default:
      user_data_dir: "~/.chrome-agent"
      headless: true
    development:
      user_data_dir: "~/.chrome-dev"
      headless: false
      devtools: true
```

### config/llm.yaml

```yaml
providers:
  minimax:
    api_key: "${MINIMAX_API_KEY}"
    group_id: "${MINIMAX_GROUP_ID}"
    base_url: "https://api.minimax.chat/v1"
    rate_limit:
      requests_per_second: 2.0
      quota_alert_threshold: 10000   # remaining tokens
  openai:
    api_key: "${OPENAI_API_KEY}"
    base_url: "https://api.openai.com/v1"
    rate_limit:
      requests_per_second: 5.0
  anthropic:
    api_key: "${ANTHROPIC_API_KEY}"
    rate_limit:
      requests_per_second: 1.0

retry:
  max_attempts: 5
  initial_backoff_ms: 1000
  max_backoff_ms: 60000
  backoff_multiplier: 2.0
```

### config/memory.yaml

```yaml
short_term:
  max_history_turns: 50
  session_ttl: "24h"

long_term:
  backend: "sqlite"             # sqlite | redis
  sqlite:
    path: "./data/memory.db"
  redis:
    url: "${REDIS_URL}"

vector:
  backend: "sqlite-vec"         # sqlite-vec | qdrant
  qdrant:
    url: "http://localhost:6333"
  embedding:
    provider: "minimax"
    model: "embo-01"
    dimensions: 1536
```

### config/plugins/telegram.yaml

Single-bot (legacy shape):

```yaml
telegram:
  token: "${TELEGRAM_BOT_TOKEN}"
  polling:
    enabled: true
    interval_ms: 1000
  allowlist:
    chat_ids: []                # empty = allow all
```

Multi-bot — declare a sequence with `instance:` labels:

```yaml
telegram:
  - token: "${TELEGRAM_BOT_BOSS}"
    instance: boss
    allowlist: { chat_ids: [111] }
  - token: "${TELEGRAM_BOT_SALES}"
    instance: sales
    allowlist: { chat_ids: [222, 333] }
```

Each bot runs in the same process with its own `BotClient`, media
cache dir (`<TELEGRAM_MEDIA_DIR>/<instance>`), offset file, and
inbound/outbound topics. Registry name collapses to `telegram` (legacy)
or `telegram.<instance>` so `PluginRegistry` doesn't overwrite.

## Directory Structure

```
mi-agente/
├── Cargo.toml
├── config/
│   ├── agents.yaml
│   ├── broker.yaml
│   ├── browser.yaml
│   ├── llm.yaml
│   ├── memory.yaml
│   └── plugins/
│       ├── whatsapp.yaml
│       ├── telegram.yaml
│       └── email.yaml
├── secrets/                    # gitignored — values only, not committed
│   └── .gitkeep
├── crates/
│   ├── core/                   # Agent runtime
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── agent/
│   │       ├── event_bus/
│   │       ├── session/
│   │       ├── heartbeat.rs
│   │       ├── circuit_breaker.rs
│   │       └── context.rs
│   ├── plugins/                # Plugin implementations
│   │   ├── browser/
│   │   ├── whatsapp/           # wraps whatsapp-rs crate
│   │   ├── telegram/
│   │   ├── email/
│   │   └── template/           # For new plugins
│   ├── llm/                   # LLM clients
│   │   └── src/
│   │       ├── client.rs
│   │       ├── minimax.rs      # primary
│   │       ├── anthropic.rs
│   │       ├── openai.rs
│   │       ├── ollama.rs
│   │       └── rate_limiter.rs
│   ├── memory/                # Memory implementations
│   │   └── src/
│   │       ├── short_term.rs
│   │       ├── long_term.rs
│   │       └── vector.rs
│   ├── broker/               # Message broker clients
│   │   └── src/
│   │       ├── nats.rs
│   │       ├── local.rs        # fallback in-memory broker
│   │       └── types.rs
│   └── config/               # Configuration loading
├── tests/
├── docs/
└── examples/
```

## Key Dependencies (Cargo.toml)

```toml
[workspace]
members = [
    "crates/core",
    "crates/plugins/browser",
    "crates/plugins/whatsapp",
    "crates/plugins/telegram",
    "crates/plugins/email",
    "crates/llm",
    "crates/memory",
    "crates/broker",
    "crates/config",
]

[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Message broker
async-nats = "0.35"

# CDP (Chrome DevTools)
tungstenite = "0.21"
tokio-tungstenite = "0.21"

# LLM clients
reqwest = { version = "0.12", features = ["rustls-tls", "json"] }

# Memory
sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio"] }
sqlite-vec = "0.1"

# Config
config = "0.14"
```

## Implementation Phases

### Phase 1: Core Runtime
- EventBus implementation (in-memory first, local broker)
- Basic agent skeleton + `AgentBehavior` trait
- Plugin interface
- Session management
- Config loading (YAML + env var resolution)
- Secrets loading

### Phase 2: Message Broker Integration
- `async-nats` client integration
- Persistent queue (disk fallback)
- Dead letter handling
- Backpressure
- Circuit breaker per topic

### Phase 3: LLM Integration
- LLM client trait
- **MiniMax client** (primary)
- OpenAI-compatible client (covers OpenAI + Ollama)
- Tool calling protocol
- Prompt management
- Rate limiter + quota tracker
- **Streaming** (`LlmClient::stream()` → `BoxStream<Result<StreamChunk>>`): providers SSE
  (MiniMax OpenAI-compat + Anthropic flavors, OpenAiClient) produce token-level deltas;
  providers without SSE inherit a default that synthesizes a one-chunk stream from `chat()`.
  `StreamChunk` variants: `TextDelta`, `ToolCallStart/ArgsDelta/End`, `Usage`, `End{finish_reason}`.
  Circuit breaker + retry apply to request-open; mid-stream errors are not retried.
  `collect_stream()` helper rebuilds `ChatResponse` from the stream.

### Phase 4: Browser Plugin
- CDP client
- Chrome auto-discovery
- Command execution
- Event subscription

### Phase 5: Memory System
- Short-term memory (rolling window)
- Long-term persistence (SQLite)
- `sqlite-vec` vector index
- Semantic recall

### Phase 6: WhatsApp Integration
- Wrap existing `whatsapp-rs` as plugin
- `whatsapp-rs` exposes: `send_text()`, `send_media()`, event stream via channel
- Plugin translates channel events → `plugin.inbound.whatsapp` topics
- Session persistence already handled by `whatsapp-rs` credentials store

### Phase 7: Heartbeat System
- Per-agent tokio interval scheduler
- `on_heartbeat()` hook
- Configurable actions per agent

### Phase 8: Agent-to-Agent Communication
- `agent.route.{target_id}` topic
- Correlation ID tracking
- Delegation + result flow

### Phase 9: Polish
- Observability: structured logs (tracing), metrics (prometheus)
- Health check endpoints (HTTP `/health`, `/ready`)
- Graceful shutdown (drain queues before exit)
- Docker Compose setup

## Docker Compose Setup

```yaml
version: "3.9"

services:
  nats:
    image: nats:2.10-alpine
    ports:
      - "4222:4222"
    volumes:
      - nats_data:/data

  agent:
    build: .
    depends_on:
      - nats
    volumes:
      - ./config:/app/config:ro
      - agent_data:/app/data
      - ./secrets:/run/secrets:ro
    environment:
      MINIMAX_API_KEY_FILE: /run/secrets/minimax_api_key
      TELEGRAM_BOT_TOKEN_FILE: /run/secrets/telegram_token
    restart: unless-stopped

  chrome:
    image: browserless/chrome:latest
    ports:
      - "9222:9222"
    environment:
      MAX_CONCURRENT_SESSIONS: "5"

volumes:
  nats_data:
  agent_data:
```

## Comparison with OpenClaw

| Aspect | OpenClaw | This Design |
|--------|----------|-------------|
| Language | TypeScript | Rust |
| Broker | Custom/Internal | NATS (`async-nats`) |
| Browser | CDP via tool | Direct CDP plugin |
| Agents | JSON config | YAML + Rust |
| Scaling | Single process | Microservices-ready |
| Fault Tolerance | Basic | Circuit breaker + persistent queue |
| Secrets | Env vars | Env vars + Docker secrets |
| LLM primary | MiniMax | MiniMax (`minimax.rs`) |
| Agent comms | None | `agent.route.{id}` topics |
| Heartbeat | External cron | Built-in per-agent ticker |

## Next Steps

1. Confirm this design matches your vision
2. Choose broker (NATS recommended — `async-nats` crate is well maintained)
3. Start with Phase 1: core + local broker + config loading
4. Phase 3 MiniMax client early — needed to test agent loop end-to-end

---

*Document version: 1.1*
*Updated: 2026-04-21*
