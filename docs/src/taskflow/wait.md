# Wait / resume

Durable flows can park themselves between steps. The runtime drives
parked flows back to `Running` either on a wall-clock deadline (timer),
when an external signal arrives (NATS), or when an operator resumes
them by hand (manual).

Two pieces wire this together:

- **`WaitEngine`** — single global tokio task. Every `tick_interval`
  it scans `Waiting` flows and resumes any whose timer has fired or
  whose cancel intent has been set.
- **`taskflow.resume` bridge** — single broker subscriber that
  translates incoming events into `WaitEngine::try_resume_external`
  calls.

Source: `crates/taskflow/src/engine.rs`, `src/main.rs::spawn_taskflow_resume_bridge`.

## Wait conditions

The `wait_json` column on a flow stores one of:

| Kind | Shape | Resumed by |
|------|-------|-----------|
| `timer` | `{kind:"timer", at:"<RFC3339>"}` | `WaitEngine.tick()` once `now >= at` |
| `external_event` | `{kind:"external_event", topic:"…", correlation_id:"…"}` | `taskflow.resume` bridge with matching `(topic, correlation_id)` |
| `manual` | `{kind:"manual"}` | Explicit `manager.resume(...)` (CLI / ops) |

`Timer.at` is validated by the tool against `taskflow.timer_max_horizon`
(default 30 days). Past deadlines and topics/correlation_ids that are
empty are rejected before the flow ever enters `Waiting`.

## Tool actions

The `taskflow` tool exposes the LLM-facing surface. Beyond the existing
`start | status | advance | cancel | list_mine`, three actions drive
the wait/resume lifecycle:

### `wait`

```json
{
  "action": "wait",
  "flow_id": "…uuid…",
  "wait_condition": {"kind": "timer", "at": "2026-04-26T09:00:00Z"}
}
```

Move flow `Running → Waiting`. Validates `wait_condition` shape and
guardrails before persisting.

### `finish`

```json
{
  "action": "finish",
  "flow_id": "…uuid…",
  "final_state": {"result": "ok"}
}
```

Move flow → `Finished`. `final_state` (optional) is shallow-merged
into `state_json` before transition.

### `fail`

```json
{
  "action": "fail",
  "flow_id": "…uuid…",
  "reason": "downstream-error"
}
```

Move flow → `Failed`. `reason` is required. The reason is stamped
under `state_json.failure.reason` and recorded in the audit event.

## NATS resume bridge

A single subscriber lives at `taskflow.resume`. Anything that wants to
wake a parked flow publishes a JSON message there:

```json
{
  "flow_id": "f5e0…",
  "topic": "agent.delegate.reply",
  "correlation_id": "corr-42",
  "payload": {"answer": 42}
}
```

The bridge calls `WaitEngine::try_resume_external(flow_id, topic,
correlation_id, payload)`. If the flow is `Waiting` with a matching
`external_event` condition, it resumes; the `payload` (if any) is
merged into `state_json.resume_event`. Mismatches and unknown flow
ids are silent debug logs.

Example with the `nats` CLI:

```sh
nats pub taskflow.resume '{
  "flow_id": "f5e0…",
  "topic": "agent.delegate.reply",
  "correlation_id": "corr-42",
  "payload": {"answer": 42}
}'
```

Single subject (no flow_id in suffix) is intentional — it keeps the
subject namespace flat and avoids per-flow subscription churn. Volume
is expected to be low (<10/s); if that ever changes, the bridge can
shard internally without protocol changes.

## Configuration

`config/taskflow.yaml` (optional; absent → defaults):

```yaml
tick_interval: 5s        # WaitEngine cadence
timer_max_horizon: 30d   # max future Timer.at allowed by tool
db_path: ./data/taskflow.db   # also honored via TASKFLOW_DB_PATH
```

`agents.yaml` enables the tool per agent:

```yaml
agents:
  - id: kate
    plugins: [taskflow, memory]
```

Without `taskflow` in `plugins`, the agent does not see the tool —
the engine and bridge still run process-wide.

## Tick interval guidance

- `5s` (default) is plenty for human-scale timers.
- Bring it down to `1s` only if you have sub-minute timers and care
  about the worst-case lag.
- The tick is idempotent and pull-based; missing a tick is harmless.

## Telemetry

Each tick logs at `debug` level when `scanned > 0`:

```
DEBUG wait engine tick scanned=3 resumed=1 cancelled=0 still_waiting=2 errors=0
```

The bridge logs at `info` on each successful resume:

```
INFO taskflow resumed via NATS flow_id=… topic=…
```
