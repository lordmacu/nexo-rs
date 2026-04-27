# `cron_create` / `cron_list` / `cron_delete` (Phase 79.7 — MVP)

LLM-time scheduling: from inside a turn, the model registers a cron
entry that fires a future goal. Complements Phase 7 Heartbeat
(config-time only) and Phase 20 `agent_turn` poller (config-time
only) — this is the only path where the model itself mutates the
schedule.

Lift from
`claude-code-leak/src/tools/ScheduleCronTool/CronCreateTool.ts:1-157`
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

## MVP scope

- `cron_create { cron, prompt, channel?, recurring? }` — schedule a
  recurring or one-shot prompt.
- `cron_list` — read-only, returns the binding's entries.
- `cron_delete { id }` — remove an entry.
- 5-field cron expression (`M H DoM Mon DoW`); 6-field also
  accepted (passthrough).
- 60-second minimum interval — sub-minute schedules refuse with
  a clear message.
- Cap 50 entries per binding (lift from leak).
- Per-binding namespace: entries from a `whatsapp:ops` goal stay
  isolated from `telegram:bot` entries.
- SQLite-backed (`nexo_cron_entries` table); survives daemon
  restart.

## Runtime firing — shipped (LLM dispatcher live)

`crates/core/src/cron_runner.rs::CronRunner` polls
`store.due_at(now)` every 5 s and dispatches due entries
through an `Arc<dyn CronDispatcher>`. State advance is
**unconditional** — even when the dispatcher fails the runner
recomputes `next_fire_at` (recurring) or deletes the entry
(one-shot) so a single broken entry never re-fires forever.

Production wiring at boot uses `LlmCronDispatcher`
(`crates/core/src/llm_cron_dispatcher.rs`): builds a
`ChatRequest` from `entry.prompt`, calls `LlmClient::chat` on a
client built from the FIRST agent's `model` config, logs the
response with id + binding + cron expression + 200-char
preview. Operators tail the log to verify cron fires actually
talk to the model.

Fallback: when no agents are configured or the LLM-client build
fails, the runner falls back to `LoggingCronDispatcher` so cron
fires stay observable in degraded boot.

Outbound publish to the binding's channel (so the user sees the
response on WhatsApp / Telegram / email) is the remaining
follow-up. Tracked in `FOLLOWUPS.md::Phase 79.7`.

## Tool shapes

### `cron_create`

```json
{
  "cron": "*/5 * * * *",
  "prompt": "Check the build queue and report",
  "channel": "whatsapp:default",
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
  "instructions": "Entry persisted. The runtime fires it on schedule (firing wired in a follow-up). Use cron_list to inspect, cron_delete to cancel."
}
```

### `cron_list`

```json
{}
```

Returns the binding's full entry list, sorted by `next_fire_at` asc.

### `cron_delete`

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

- `cron_create` and `cron_delete` → `Schedule` (mutating). Plan
  mode refuses with `PlanModeRefusal`.
- `cron_list` → `ReadOnly`. Stays callable while plan mode is on.

## References

- **PRIMARY**:
  `claude-code-leak/src/tools/ScheduleCronTool/CronCreateTool.ts:1-157`
  (schema, validation, 50-entry cap), plus the sibling
  `CronListTool.ts` / `CronDeleteTool.ts`.
- **SECONDARY**: `research/src/cron/schedule.ts` (OpenClaw —
  `croner` JS lib + cache pattern, semantically compatible).
- Plan + spec: `proyecto/PHASES.md::79.7`.
