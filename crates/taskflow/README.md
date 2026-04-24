# agent-taskflow

Durable multi-step flow runtime for the Rust agent framework. Phase 14 of
`proyecto`. Inspired by OpenClaw's `taskflow` skill; rewritten around
SQLite persistence, revision-checked mutations, and a broker-agnostic
wait/resume engine.

## Layout

```
src/
├── lib.rs       — re-exports
├── types.rs     — Flow, FlowStatus, FlowStep, FlowStepStatus, FlowError, state machine
├── store.rs     — FlowStore trait + SqliteFlowStore (flows, flow_steps, flow_events)
├── manager.rs   — FlowManager (managed + mirrored APIs, retry-on-conflict)
└── engine.rs    — WaitEngine (tick-based resume for Timer / ExternalEvent / Manual)

tests/
└── e2e_test.rs  — restart durability + concurrent mutation safety
```

## What it owns

- Flow identity (`Uuid`) tied to an owner session
- `current_step`, `state_json`, `wait_json`
- Revision tracking for optimistic locking
- Linked child tasks (`flow_steps`) for mirrored-mode observation
- Sticky cancel intent that survives restart
- Append-only audit log (`flow_events`)

## What it does **not** own

- Business branching logic — keep it in the controller (caller)
- Message transport — engine is broker-agnostic; a NATS subscriber or
  agent-to-agent bridge calls `try_resume_external` / `record_step_observation`
- LLM dispatch — exposed through the `taskflow` agent tool in `agent-core`

## Quick start

```rust
use std::sync::Arc;
use agent_taskflow::{CreateManagedInput, FlowManager, SqliteFlowStore};
use serde_json::json;

# tokio_test::block_on(async {
let store = Arc::new(SqliteFlowStore::open("./data/taskflow.db").await?);
let manager = FlowManager::new(store);

let flow = manager.create_managed(CreateManagedInput {
    controller_id: "kate/inbox-triage".into(),
    goal: "triage today's inbox".into(),
    owner_session_key: "agent:kate:session:abc".into(),
    requester_origin: "user-1".into(),
    current_step: "classify".into(),
    state_json: json!({ "messages": 10, "processed": 0 }),
}).await?;

let flow = manager.start_running(flow.id).await?;
let flow = manager.update_state(flow.id, json!({ "processed": 5 }), None).await?;
let _ = manager.finish(flow.id, None).await?;
# Result::<_, agent_taskflow::FlowError>::Ok(())
# }).unwrap();
```

## Tests

```bash
cargo test -p agent-taskflow
```

Coverage (2026-04-23):
- 41 unit tests (types state machine, store CRUD, manager API, engine ticks)
- 4 integration tests (`tests/e2e_test.rs`): restart durability, concurrent
  revision retry, heavy contention surfacing `RevisionMismatch`, mirrored
  steps persistence

## CLI (see also `agent flow --help`)

```bash
agent flow list [--json]
agent flow show <uuid> [--json]
agent flow cancel <uuid>
agent flow resume <uuid>
```

`TASKFLOW_DB_PATH` env var overrides the SQLite path.

## Error codes

Errors bubble to the `taskflow` agent tool with these JSON-RPC codes:

| Variant | Code |
|---------|------|
| `BadInput` | -32602 |
| `NotFound` | -32001 |
| `RevisionMismatch` | (internal, retried or surfaced as `-32006` context) |
| `IllegalTransition` | -32602 (surface as `bad input` in the tool layer) |
| `AlreadyTerminal` | -32602 |
| `CancelPending` | -32602 |

The `taskflow` tool (in `agent-core`) does not expose every variant to the
LLM — user-friendly summaries are rendered per action.

## Related phases

- Phase 5 (Memory) — same SQLite stack (`sqlx`, `libsqlite3-sys` bundled)
- Phase 7 (Heartbeat) — the host wires `WaitEngine::tick` into a scheduled task
- Phase 8 (Agent-to-agent) — flows wait on delegation replies via `try_resume_external`
- Phase 13.7 (Skill frontmatter) — `skills/taskflow/SKILL.md` uses the
  metadata convention
