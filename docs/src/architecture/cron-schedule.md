# `cron_create` / `cron_list` / `cron_delete` / `cron_pause` / `cron_resume`

LLM-time scheduling: from inside a turn, the model registers a cron
entry that fires a future goal. Complements Phase 7 Heartbeat
(config-time only) and Phase 20 `agent_turn` poller (config-time
only) — this is the only path where the model itself mutates the
schedule.

Lift from
`upstream agent CLI`
(5-field cron schema, recurring + durable flags, 50-entry cap).
OpenClaw `research/src/cron/schedule.ts` provides the parallel
naming convention — we use Rust's `cron = "0.12"` crate (already a
transitive workspace dep).

## Diff vs Phase 7 Heartbeat vs Phase 20 `agent_turn` poller

| Mechanism | Trigger source | Mutable at runtime | Persists |
|-----------|----------------|--------------------|----------|
| Phase 7 Heartbeat | YAML `heartbeat.interval_secs` | No (hot-reload only) | Config |
| Phase 20 `agent_turn` poller | YAML cron spec | No (hot-reload only) | Config |
| **Phase 79.7 ScheduleCron** | LLM tool call mid-turn | Yes (model-driven) | SQLite |

## Tool surface and constraints

- `cron_create { cron, prompt, channel?, recipient?, recurring? }` —
  schedule a recurring or one-shot prompt. `recipient` is the
  `to` address for outbound publish (JID for WhatsApp, chat id
  for Telegram, email for SMTP); without it the dispatcher only
  logs the LLM response.
- `cron_list` — read-only, returns the binding's entries.
- `cron_delete { id }` — remove an entry.
- `cron_pause { id }` — soft-disable an entry (`paused = true`).
- `cron_resume { id }` — re-enable a paused entry (`paused = false`).
- 5-field cron expression (`M H DoM Mon DoW`); 6-field also
  accepted (passthrough).
- 60-second minimum interval — sub-minute schedules refuse with
  a clear message.
- Cap 50 entries per binding (lift from upstream).
- Origin-tagged binding namespace: entries from a `whatsapp:ops`
  goal stay isolated from `telegram:bot` entries. `binding_id`
  resolves from inbound origin (`plugin:instance`) with `agent_id`
  fallback for non-interactive turns.
- SQLite-backed (`nexo_cron_entries` table); survives daemon
  restart.
- Model pinning at schedule time: `cron_create` stores
  `model_provider` + `model_name` from effective binding policy so
  each fire can resolve the same provider/model pair later.

## Runtime firing — shipped (end-to-end)

`crates/core/src/cron_runner.rs::CronRunner` polls
`store.due_at(now)` every 5 s and dispatches due entries
through an `Arc<dyn CronDispatcher>`. State advance is
**policy-driven**:

- recurring entries always advance (even on dispatch failure) so a
  broken downstream never hot-loops one row forever.
- one-shot entries delete on success; on failure they retry with
  bounded exponential backoff (`runtime.cron.one_shot_retry`) and are
  deleted only after the retry budget is exhausted.

Production wiring at boot uses `LlmCronDispatcher`
(`crates/core/src/llm_cron_dispatcher.rs`): builds a
`ChatRequest` from `entry.prompt`, resolves the LLM client from the
entry's pinned `model_provider`/`model_name` (with legacy fallback
for old rows), logs the response with id + binding + cron expression
and a 200-char preview, then forwards the body to the user-facing channel
via `BrokerChannelPublisher` when the entry carries both a `channel`
and a `recipient`.

Tool-call execution is now available as an explicit opt-in:
`runtime.cron.tool_calls.enabled: true`. In that mode, the dispatcher
advertises the binding-filtered tool set, executes returned tool calls,
feeds `tool_result` messages back to the model, and repeats up to
`runtime.cron.tool_calls.max_iterations`.

Fallback: when no agents are configured or the LLM-client build
fails, the runner falls back to `LoggingCronDispatcher` so cron
fires stay observable in degraded boot.

### Outbound publish

`BrokerChannelPublisher` parses `<plugin>:<instance>` from
`entry.channel` and emits an event on
`plugin.outbound.<plugin>.<instance>` carrying:

```json
{ "kind": "text", "to": "<recipient>", "text": "<llm body>" }
```

This is the same envelope the WhatsApp / Telegram / Email
outbound tools already speak — the receiving plugin's dispatcher
delivers the message to the user.

Failure mode: a publish error is logged via `tracing::warn!`
but never fails `fire()`. The runner still advances state, so a
stuck downstream channel (NATS down, plugin not subscribed)
cannot deadlock the cron loop. Set both `channel` and
`recipient` on `cron_create` to enable user-facing delivery —
either missing → the dispatcher only logs.

## Tool shapes

### `cron_create`

```json
{
  "cron": "*/5 * * * *",
  "prompt": "Check the build queue and report",
  "channel": "whatsapp:default",
  "recipient": "5511999999999@s.whatsapp.net",
  "recurring": true
}
```

Returns:

```json
{
  "ok": true,
  "id": "01J...",
  "binding_id": "whatsapp:default",
  "cron": "*/5 * * * *",
  "recurring": true,
  "next_fire_at": 1700000300,
  "instructions": "Entry persisted. The runtime fires it on schedule. Use cron_list to inspect, cron_pause/cron_resume to temporarily stop/restart, and cron_delete to cancel."
}
```

### One-shot retry policy

Process-level policy in `config/runtime.yaml`:

```yaml
cron:
  one_shot_retry:
    max_retries: 3        # 0 => drop on first failure
    base_backoff_secs: 30 # attempt #1 delay
    max_backoff_secs: 1800
  tool_calls:
    enabled: false        # default: log-only for tool calls
    max_iterations: 6
    allowlist: []         # optional extra narrowing (glob syntax)
```

Attempt delays are exponential (`base * 2^(attempt-1)`), capped by
`max_backoff_secs`.

### `cron_list`

```json
{}
```

Returns the binding's full entry list, sorted by `next_fire_at` asc.

### `cron_delete`

```json
{ "id": "01J..." }
```

### `cron_pause`

```json
{ "id": "01J..." }
```

### `cron_resume`

```json
{ "id": "01J..." }
```

## Cron expression semantics

Standard 5-field UTC: `M H DoM Mon DoW`. Examples:

| Expression | Means |
|------------|-------|
| `*/5 * * * *` | Every 5 minutes |
| `0 9 * * *` | Daily 09:00 UTC |
| `30 14 28 2 *` | Feb 28 14:30 UTC (one-shot if `recurring: false`) |
| `0 */2 * * *` | Every 2 hours on the hour |

The 60-second minimum is enforced by checking that two consecutive
fires are ≥ 60 seconds apart. Sub-minute expressions like `*/30 * *
* * *` (every 30 s, 6-field) are rejected.

## Plan-mode classification

- `cron_create`, `cron_delete`, `cron_pause`, and `cron_resume` →
  `Schedule` (mutating). Plan
  mode refuses with `PlanModeRefusal`.
- `cron_list` → `ReadOnly`. Stays callable while plan mode is on.

## References

- **PRIMARY**:
  `upstream agent CLI`
  (schema, validation, 50-entry cap), plus the sibling
  `CronListTool.ts` / `CronDeleteTool.ts` / `CronPauseTool.ts` /
  `CronResumeTool.ts`.
- **SECONDARY**: `research/src/cron/schedule.ts` (OpenClaw —
  `croner` JS lib + cache pattern, semantically compatible).
- Plan + spec: `proyecto/PHASES.md::79.7`.
