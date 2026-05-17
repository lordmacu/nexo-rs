# Poller V2 — Laravel-style dispatch

Phase 96 refactored the poller subsystem around a single principle:
**the runner is a dumb scheduler**. It knows nothing about channels,
credentials, outbound topics, or LLMs. Pollers reach the world
through one egress trait, `PollerHost`. Everything else is the
runner's business: schedule, lease, retry, breaker, cursor
persistence, telemetry.

If you've used Laravel's queue, the cut is familiar: `Queue::push`
takes an opaque job, `Worker` pops + invokes — the queue never
introspects what the job does or what it returns.

## Why

V1 (pre-Phase-96) leaked too much. The runner needed to know:

- That outbound goes to a `Channel` enum (whatsapp / telegram / google).
- That every poller might want a `CredentialsBundle` + per-channel
  `AgentCredentialResolver`.
- That `agent_turn` specifically needs `LlmRegistry` + `LlmConfig`.
- How to translate `OutboundDelivery { channel, recipient, payload }`
  into `plugin.outbound.<channel>.<account_id>` topic publishes
  (`dispatch.rs`, ~200 LOC).

Every new poller kind risked widening the runner's surface — and
out-of-tree pollers couldn't escape the in-tree types at all. Phase
96 cut all of it.

## Contract

```rust
#[async_trait]
pub trait Poller: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    async fn tick(&self, ctx: &PollContext) -> Result<TickAck, PollerError>;
}

pub struct PollContext {
    pub job_id: String,
    pub agent_id: String,
    pub kind: &'static str,
    pub config: Value,
    pub cursor: Option<Vec<u8>>,
    pub now: DateTime<Utc>,
    pub interval_hint: Duration,
    pub cancel: CancellationToken,
    pub host: Arc<dyn PollerHost>,
}

pub struct TickAck {
    pub next_cursor: Option<Vec<u8>>,
    pub next_interval_hint: Option<Duration>,
    pub metrics: Option<TickMetrics>,
}
```

`PollerHost` is the single egress:

```rust
#[async_trait]
pub trait PollerHost: Send + Sync + 'static {
    async fn broker_publish(&self, topic: String, payload: Vec<u8>) -> Result<(), HostError>;
    async fn credentials_get(&self, channel: String) -> Result<Value, HostError>;
    async fn log(&self, level: LogLevel, message: String, fields: Value) -> Result<(), HostError>;
    async fn metric_inc(&self, name: String, labels: Value) -> Result<(), HostError>;
    async fn llm_invoke(&self, request: LlmInvokeRequest) -> Result<LlmInvokeResponse, HostError>;
}
```

## Two host implementations

| Adapter | Use case | Crate |
|---------|----------|-------|
| `InProcessHost` | In-tree builtins (`webhook_poll`, `agent_turn`) | `nexo-poller` (private use) |
| `BrokerPollerHost` | Subprocess plugin pollers | `nexo-microapp-sdk::poller` |

`InProcessHost` calls directly into the daemon's `AnyBroker`,
`AgentCredentialResolver`, `LlmRegistry`. `BrokerPollerHost` pipes
the same trait methods through broker reverse-RPC on
`daemon.rpc.<plugin_id>`. Both produce identical poller-visible
behavior — the trait surface is the contract, not the impl.

## Dispatch topology

```
                      ┌─────────────────────────────────────┐
                      │ daemon: PollerRunner                │
                      │  ├─ webhook_poll  (in-tree)         │
                      │  ├─ agent_turn    (in-tree)         │
                      │  └─ plugin proxies (Arc<dyn Poller>) │
                      └──────────────┬──────────────────────┘
                                     │
                ┌────────────────────┼────────────────────────┐
                │                    │                        │
       broker_publish         broker tick request         broker reverse-RPC
   plugin.outbound.X.Y    plugin.poller.<kind>.tick      daemon.rpc.<plugin_id>
                │                    │                        │
                ▼                    ▼                        │
      ┌──────────────┐     ┌──────────────┐                  │
      │ wa / tg / …  │     │ subprocess   │──────────────────┘
      │ channel      │     │ plugin       │
      │ plugin       │     │  (PollerHandler)
      └──────────────┘     └──────────────┘
```

The daemon's `PluginPollerRouter` owns `(plugin_id, kinds, topic_prefix)`
mappings. For each `(handle, kind)` it wraps a `PluginPollerProxy`
that implements `Poller` and forwards every tick through broker
JSON-RPC. The runner's registry is homogeneous — it cannot tell which
entries are in-tree and which forward over the wire.

## `[plugin.poller]` manifest section

Seventh manifest section closing the Phase 81.33.b.real lineage
(pairing → http → admin → metrics → dashboard → ✶ → poller).

```toml
[plugin.poller]
kinds                = ["google_calendar"]
broker_topic_prefix  = "plugin.poller.google_calendar"
lifecycle            = "long_lived"        # or "ephemeral"
max_concurrent_ticks = 1                   # 1..=64, default 1
tick_timeout_secs    = 60                  # 1..=3600, default 60
```

Boot-time validation:

- `kinds` non-empty + unique within the plugin.
- Each kind matches `^[a-z][a-z0-9_]+$`.
- `broker_topic_prefix` non-empty + no trailing dot + no spaces.
- Cross-plugin uniqueness — two plugins declaring the same kind
  fail boot loud (`PollerRouteRegistrationError::DuplicateKind`).

## Lifecycle

- **`long_lived`** (default) — daemon spawns the plugin subprocess
  once at boot. The subprocess subscribes to its tick topic and
  replies through the message's `reply_to`. Best for pollers with
  warm state (OAuth tokens, HTTP connection pools, parsed feeds).
- **`ephemeral`** — manifest accepts the value but the daemon
  currently rejects it with a config error. Tracked as a Phase 96
  follow-up: spawn-per-tick path requires new stdio JSON-RPC
  primitives (no broker subscription, direct stdin/stdout dispatch
  + SIGTERM on reply).

## Reverse-RPC

Subprocess pollers call back to the daemon via broker request-reply
on `daemon.rpc.<plugin_id>`. Methods:

| Method | Daemon response |
|--------|-----------------|
| `credentials_get(channel, agent_id)` | `{ account_id, … }` plus typed Google fields (`client_id_path`, `token_path`) when channel == google |
| `log(level, message, fields)` | Forwards to daemon tracing |
| `metric_inc(name, labels)` | Forwards to daemon tracing (Prometheus aggregator is a follow-up) |
| `llm_invoke(request)` | Proxies to `LlmRegistry::build(...)::chat(...)`, returns `{ content, model_id, usage }` |

Error envelopes use JSON-RPC codes that mirror `PollerError`
classification: `-32001` transient (retry with backoff), `-32002`
permanent (auto-pause job until `agent pollers reset <id>`),
`-32602` config (bad config — operator fixes YAML), `-32601` method
not found.

## What this unlocks

- New poller kinds (Jira, Linear, Stripe, custom internal APIs)
  ship as standalone crates published to crates.io. No fork of
  `nexo-poller`, no PR to the framework.
- The framework's `nexo-poller` dep tree no longer carries
  `nexo-plugin-google` — gmail + google_calendar moved out to
  `nexo-rs-poller-gmail` + `nexo-rs-poller-google-calendar` standalone
  repos. Closes the Phase 94 close-out follow-up.
- LLM-using pollers (`agent_turn` + future custom prompts) no longer
  need `llm_registry` / `llm_config` fields baked into `PollContext`.
  Any subprocess plugin can call `host.llm_invoke` and get the daemon's
  configured provider stack for free.

## References

- Phase 81.33.b.real lineage: Stage 1 pairing → 2 http → 4 admin →
  5 metrics → 6 dashboard → ✶ → Phase 96 poller. Same `RwLock`
  interior mutability pattern, same broker-RPC forwarder shape, same
  "construct empty at boot, populate after wire" rule.
- OpenClaw cron service (`research/src/cron/service/locked.ts:11-21`,
  `service.restart-catchup.test.ts:79-116`) informed lease + restart
  semantics.
- claude-code-leak MCP elicitation handler
  (`src/services/mcp/elicitationHandler.ts:77-106`) shaped the
  reverse-RPC pattern: server (here, subprocess) sends a request UP
  to the client (here, daemon), client responds via the same
  channel.
