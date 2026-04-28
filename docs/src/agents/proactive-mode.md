# Proactive Mode (Phase 77.20)

Proactive mode lets an agent run autonomously between user messages.
Instead of waiting for a new inbound event, the runtime injects periodic
`<tick>` prompts and the model decides whether to do work now or call
`Sleep { duration_ms, reason }`.

## Configuration

Enable at agent level or per binding (`inbound_bindings[].proactive`):

```yaml
proactive:
  enabled: true
  tick_interval_secs: 600
  jitter_pct: 25
  max_idle_secs: 86400
  initial_greeting: true
  cache_aware_schedule: true
  allow_short_intervals: false
  daily_turn_budget: 200
```

Per-binding override replaces the full proactive block for that binding.

## Sleep Tool

`Sleep` is the canonical way to wait in proactive mode.
Do not use shell `sleep` for this.

- Bounds: `duration_ms` is clamped to `[60_000, 86_400_000]`.
- Wake-up: runtime injects a synthetic `<tick>` with elapsed time + reason.
- Interrupt: real inbound user messages cancel pending sleep immediately.

## Inbound Queue Priority

Inbound events can optionally carry `priority` in payload:

- `now` — highest priority (urgent interrupt)
- `next` — default priority (normal user input)
- `later` — deferred background notifications

When multiple messages are batched in the same debounce window, runtime
processes them in `now > next > later` order, preserving FIFO within
each priority class.
`now` also bypasses debounce delay and flushes immediately.
If `now` arrives during an in-flight turn, runtime preempts that turn and
runs the `now` message first.

## Cache-Aware Scheduling

When `cache_aware_schedule: true`, runtime biases sleep duration to avoid
the Anthropic cache dead-zone:

- `<= 270_000ms`: keep as-is (cache warm window).
- `270_001..1_199_999ms`: snap to `270_000` or `1_200_000` (nearest).
- `>= 1_200_000ms`: keep as-is.

## Daily Tick Budget

`daily_turn_budget` limits proactive tick-driven turns per 24h window.

- `0` means unlimited.
- When exhausted, wake-ups are suppressed and re-armed using the effective
  tick interval.

This prevents runaway autonomous loops from burning quota.

## Telemetry

Prometheus counter:

- `nexo_proactive_events_total{agent,event}`

Events:

- `tick.fired`
- `sleep.entered`
- `sleep.interrupted`
- `cache_aware.snapped`

## Relation to `agent_turn` Poller

Phase 20 `agent_turn` is cron-driven external scheduling.
Proactive mode is model-driven self-pacing inside a live goal.
They are complementary and can coexist across different bindings.
