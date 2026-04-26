# nexo-taskflow

> Durable multi-step workflow runtime for Nexo — `Flow` state machine + SQLite-backed `FlowStore` + `WaitEngine` tick loop + `taskflow_tool` LLM-facing API. Survives restarts; reanudates timer-based + external-event waits.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`Flow` state machine** — `Created → Running → Waiting →
  Resumed → Finished | Failed | Cancelled`. Transitions
  validated; can_transition_to() refuses illegal moves.
- **`FlowStore` trait** + SQLite impl — every transition writes
  through; restart re-loads the open set (Running + Waiting).
- **`WaitCondition`** — `Timer { at }`, `ExternalEvent { topic,
  correlation_id }`, `Manual { signal }`. Each is serialised
  into the DB so the engine can resume without losing the
  wait spec.
- **`WaitEngine` tick loop** — runs as a single global tokio
  task; every interval scans `Waiting` flows, fires due
  timers, and reanudates them. Sub-millisecond tick-no-due
  (bench-covered).
- **NATS resume bridge** — subscribes the topics every
  `ExternalEvent` wait declared; on event arrival, looks up
  the matching flow by `correlation_id` and reanudates.
- **Mirrored mode** — `MirroredFlow` lets the host record
  externally-driven steps (e.g. a webhook poller produced an
  event) without owning the flow lifecycle.
- **`taskflow_tool`** — LLM-facing tool with actions `start`,
  `status`, `advance`, `wait`, `finish`, `fail`, `cancel`,
  `list_mine`. Per-binding capability gate + timer max
  horizon guardrail (`timer_max_horizon`).

## Public API

```rust
pub struct FlowManager { /* … */ }

impl FlowManager {
    pub fn new(store: Arc<dyn FlowStore>) -> Self;
    pub async fn create_managed(&self, input: CreateManagedInput) -> Result<Flow>;
    pub async fn start_running(&self, id: Uuid) -> Result<Flow>;
    pub async fn set_waiting(&self, id: Uuid, wait: Value) -> Result<Flow>;
    pub async fn resume(&self, id: Uuid, patch: Option<Value>) -> Result<Flow>;
    pub async fn finish(&self, id: Uuid, state: Option<Value>) -> Result<Flow>;
    pub async fn fail(&self, id: Uuid, reason: impl Into<String>) -> Result<Flow>;
}

pub struct WaitEngine { /* … */ }

impl WaitEngine {
    pub fn new(manager: FlowManager) -> Self;
    pub async fn tick(&self) -> TickReport;
    pub async fn try_resume_external(&self, topic: &str, correlation_id: &str) -> Result<()>;
    pub async fn run(&self, interval: Duration, shutdown: CancellationToken);
}
```

## When to use

- Agent has to wait days for an external signal (payment
  received, approval given, document uploaded).
- Multi-step workflow that must survive daemon restart with
  cursor preserved.
- Periodic recurring task with state (rolling N-day reminder).
- Long-running batch process with checkpointing.

## Install

```toml
[dependencies]
nexo-taskflow = "0.1"
```

## Documentation for this crate

- [TaskFlow model](https://lordmacu.github.io/nexo-rs/taskflow/model.html)
- [FlowManager](https://lordmacu.github.io/nexo-rs/taskflow/manager.html)
- [Wait / resume](https://lordmacu.github.io/nexo-rs/taskflow/wait.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
