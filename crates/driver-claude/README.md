# nexo-driver-claude

Subprocess + persistence layer of the programmer agent. Owns:

1. Spawning the `claude` CLI in headless mode and consuming its
   `stream-json` output as typed events.
2. The `SessionBinding` SQLite store that maps `goal_id →
   claude session_id` so the next turn can `--resume <id>`.

If the orchestrator (`nexo-driver-loop`) is the brain, this
crate is the hand that holds the subprocess.

## What's in here

### Subprocess plumbing

| Type | Purpose |
|---|---|
| `ClaudeCommand` | Builder for the spawn argv. `.cwd(workspace).mcp_config(path).resume(session_id).apply_defaults(&cfg.default_args)`. |
| `ClaudeConfig`, `ClaudeDefaultArgs`, `OutputFormat` | YAML-deserialised config: binary path (or `which("claude")` lookup), allowed tools, model override, `permission_prompt_tool`, turn timeout, forced kill grace period. |
| `ChildHandle` | Owns the `tokio::process::Child` + a SIGTERM-then-SIGKILL shutdown helper. |
| `EventStream` | Async iterator over `ClaudeEvent` parsed from stdout JSON-lines. Errors carry the line number for debug. |
| `ClaudeEvent` (+ `InitEvent`, `AssistantEvent`, `UserEvent`, `ResultEvent`) | Discriminated typed events: `init`, `assistant`, `user`, `result.success`, `result.error_max_turns`, `result.error_during_execution`. The orchestrator uses these to decide turn outcome. |
| `TurnHandle` | Bundle: spawn the command, return a stream + a shutdown handle. The orchestrator's `run_attempt` consumes one TurnHandle per turn. |
| `spawn_turn(cmd, cancel, turn_timeout, forced_kill_after)` | Top-level helper used by the orchestrator. Honours the per-goal cancel token + per-turn wall-clock cap. |

### Session binding (SQLite)

| Type | Purpose |
|---|---|
| `SessionBinding { goal_id, session_id, model, workspace, created_at, updated_at, last_active_at, origin_channel, dispatcher }` | The row. `origin_channel` + `dispatcher` (Phase 67.B.1, schema v2) are populated by `attempt.rs` from `Goal.metadata` so the chat that triggered a goal survives daemon restart. |
| `SessionBindingStore` trait | `get / upsert / clear / mark_invalid / touch / purge_older_than / list_active`. |
| `MemoryBindingStore` | DashMap-backed for tests / dev. Loses state on restart. |
| `SqliteBindingStore` | File-backed, WAL on disk, migrates v1 → v2 idempotently via `pragma table_info`. `with_idle_ttl(d) / with_max_age(d)` hide stale bindings from `get`. |
| `OriginChannel { plugin, instance, sender_id, correlation_id }` | The chat / channel that triggered the goal. Lifted into `notify_origin` payloads so completion summaries route back. |
| `DispatcherIdentity { agent_id, sender_id, parent_goal_id, chain_depth }` | The agent that issued the dispatch. `chain_depth` bounds `dispatch_phase` hook fan-out. |

### Errors

`ClaudeError { Spawn, Io, Stream, Cancelled, Timeout, ExitNonZero, Sqlx, Binding, Json }` — every failure mode the orchestrator's replay-policy needs to classify.

## Where it sits

```
                    ┌──────────────────────┐
                    │  driver-loop         │
                    │  (run_goal loop)     │
                    └─────────┬────────────┘
                              │
                  ┌───────────┴───────────┐
                  │                       │
       ┌──────────▼──────────┐   ┌────────▼─────────┐
       │  driver-claude      │   │ driver-permission│
       │  • ClaudeCommand    │   │  • MCP tool      │
       │  • spawn_turn       │   │  • PermissionDecider │
       │  • SessionBinding   │   └──────────────────┘
       └─────────────────────┘
```

driver-loop calls `spawn_turn(cmd, cancel, ...)` per turn, drains the
event stream, persists the resulting `SessionBinding` (with origin /
dispatcher lifted from `goal.metadata`), and feeds `AttemptResult`
back to the orchestrator's replay-policy + agent-registry.

## See also

- `architecture/project-tracker.md` — programmer agent overview.
- `architecture/driver-subsystem.md` — Phase 67.1 spawn details.
