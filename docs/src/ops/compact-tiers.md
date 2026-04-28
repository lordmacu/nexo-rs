# Compact tiers

Context compaction in Nexo currently has two practical tiers:

## Tier 1: micro compact (inline tool-result shrink)

Reduces oversized `tool_result` payloads before request send, keeping
`tool_use_id` correlation stable while replacing bulky content with a
compact marker (or provider-summary path when configured).

Operational intent:
- protect prompt budget from one-off large tool outputs
- preserve turn continuity without rewriting full history

## Tier 2: auto compact (history folding)

When token pressure crosses configured thresholds, runtime folds older
history into a compact summary while preserving the hot tail.

Operational intent:
- keep long-running sessions inside context window
- reduce repeated cost of stale historical turns

## Configuration surface

Both are controlled under `llm.context_optimization.compaction` in
`llm.yaml`, with per-agent enable switches in `agents.yaml`.

See:
- [Context optimization](./context-optimization.md)
- [Hot reload](./hot-reload.md)

## Telemetry to watch

- `llm_compaction_triggered_total{agent,outcome}`
- `llm_compaction_duration_seconds{agent,outcome}`
- prompt/token drift counters from token counter telemetry
