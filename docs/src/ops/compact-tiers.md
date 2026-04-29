# Compact tiers

Context compaction and memory extraction in Nexo currently has four tiers:

## Tier 1: micro compact (inline tool-result shrink)

Reduces oversized `tool_result` payloads before request send, keeping
`tool_use_id` correlation stable while replacing bulky content with a
compact marker (or provider-summary path when configured).

Operational intent:
- protect prompt budget from one-off large tool outputs
- preserve turn continuity without rewriting full history

## Tier 2: auto compact (history folding — Phase 67.9 + 77.2)

When token pressure crosses configured thresholds or session age expires,
runtime folds older history into a compact summary while preserving the
hot tail.

Two independent triggers (Phase 77.2):

### Token-pressure trigger

Fires when `estimated_tokens / context_window >= token_pct` (default 0.80
when `auto` block is present, fallback to legacy `threshold` 0.70 when
absent).

### Age trigger

Fires when `session_age_minutes >= max_age_minutes` (default 120).
Disabled when `auto` block is absent or `max_age_minutes: 0`.

### Guards

- **Anti-storm:** `min_turns_between` (default 5) turns must elapse
  between consecutive compactions.
- **Circuit breaker:** after `max_consecutive_failures` (default 3)
  consecutive compaction failures, the policy stops requesting compacts
  for the remainder of the goal. A successful compact resets the counter.
- **Buffer tokens:** `buffer_tokens` (default 13000) safety margin below
  effective context window.

Operational intent:
- keep long-running sessions inside context window
- age-based trigger catches memory pressure from accumulated tool outputs
  even when estimated tokens are low
- reduce repeated cost of stale historical turns

### Events

| Event | Subject | When |
|-------|---------|------|
| `CompactRequested` | `agent.driver.compact` | Policy classifies and schedules a compact turn |
| `CompactCompleted` | `agent.driver.compact.completed` | Turn after compact, with `after_tokens` |

## Tier 3: session memory compact (Phase 77.3)

Persists compact summaries to long-term memory so resumed sessions
can inject the last compact summary into the prompt without
re-executing elided turns.

Operational intent:
- survive daemon restart without losing compaction progress
- feed prior summary into resumed goal's first-turn prompt
- avoid redundant re-compaction of the same history

### How it works

1. After a successful compact turn, the orchestrator extracts the
   LLM-generated summary from `result.final_text`.
2. Summary is persisted via `LongTermMemory::remember()` with tag
   `compact_summary` and goal_id embedded in the content for FTS5 recall.
3. On goal resume (daemon restart), `load()` retrieves the most recent
   summary and injects it into `next_extras` as `compact_summary`.
4. `PostCompactCleanup` runs after persistence (no-op placeholder for
   77.5+ extractMemories integration).

### Events

| Event | Subject | When |
|-------|---------|------|
| `CompactSummaryStored` | `agent.driver.compact.summary_stored` | Summary persisted to LTM |

### Config

```yaml
compact_policy:
  sm_compact:                  # Phase 77.3 (optional)
    min_tokens: 10000          # min tokens before store (default 10000)
    max_tokens: 40000          # max tokens per summary (default 40000)
    store_in_long_term_memory: true  # default true
```

`sm_compact` defaults to `None` — set it to enable session-memory
persistence. `store_in_long_term_memory: false` uses the noop store
for testing.

## Tier 4: extractMemories (post-turn LLM extraction — Phase 77.5)

After every N eligible turns, a small LLM call reads the recent
conversation transcript and writes durable memories to the persistent
memory directory (`~/.claude/projects/<path>/memory/*.md` + `MEMORY.md`).

Four-type taxonomy (user / feedback / project / reference) with an
explicit exclusion list (code patterns, git history, debug recipes,
CLAUDE.md contents, ephemeral task details). Extraction is single-turn:
the existing memory manifest is pre-injected into the system prompt so
the LLM can decide what to update without file-system exploration.
Response is parsed as a JSON array of `{file_path, content}` objects.

Operational intent:
- complement Phase 10.6 dreaming (offline/recall-signal-based) with an
  inline/transcript-based path
- keep the memory directory current without manual `remember` invocations
- surface durable context to future sessions without re-reading full
  conversation history

### Guards

- **Throttle:** `turns_throttle` (default 1 = every turn; recommend 3+
  in production to limit token cost).
- **Circuit breaker:** after `max_consecutive_failures` (default 3)
  consecutive extraction failures, the breaker opens and extraction is
  skipped for the remainder of the goal.
- **Mutual exclusion:** at most one extraction in-flight per goal.
  When a new turn arrives mid-extraction, its context is coalesced and
  runs as a single trailing extraction.
- **Main-agent write detection:** extraction is skipped when the main
  agent already wrote to the memory directory this turn, avoiding
  clobbering intentional user-directed writes.
- **Path sandbox:** file paths from the LLM are validated — absolute
  paths and `..` traversal are rejected.

### Events

| Event | Subject | When |
|-------|---------|------|
| `ExtractMemoriesCompleted` | `agent.driver.extract_memories.completed` | Extraction succeeded, N memories saved |
| `ExtractMemoriesSkipped` | `agent.driver.extract_memories.skipped` | Extraction skipped (disabled / throttled / in-progress / circuit-breaker / main-agent-wrote) |

### Config

```yaml
compact_policy:
  extract_memories:            # Phase 77.5 (optional — default: disabled)
    enabled: true              # master switch (default false — opt-in)
    turns_throttle: 3          # run every N eligible turns (default 1)
    max_turns: 5               # max LLM turns per extraction (default 5)
    max_consecutive_failures: 3  # circuit breaker (default 3, 0=disabled)
```

`extract_memories` defaults to `None` — set it to enable post-turn
extraction. The LLM backend is wired via the driver orchestrator's
`extract_memories()` builder method; the binary crate supplies the
`LlmClient` adapter.

## Configuration surface

All tiers are controlled under `llm.context_optimization.compaction` in
`llm.yaml`, with per-agent enable switches in `agents.yaml`.

Driver-side config (`config/driver/claude.yaml`):

```yaml
compact_policy:
  enabled: true
  context_window: 200000      # model context window in tokens
  threshold: 0.7              # legacy token-pressure threshold (0.0-1.0)
  min_turns_between_compacts: 5
  auto:                       # Phase 77.2 (optional — age trigger disabled when absent)
    token_pct: 0.80           # token-pressure threshold (0.0-1.0, default 0.80)
    max_age_minutes: 120      # fire age trigger after 2 h (0 disables, default 120)
    buffer_tokens: 13000      # safety margin below context window (default 13000)
    min_turns_between: 5      # anti-storm gap (default 5)
    max_consecutive_failures: 3  # circuit breaker (default 3)
  sm_compact:                  # Phase 77.3 (optional)
    min_tokens: 10000
    max_tokens: 40000
    store_in_long_term_memory: true
  extract_memories:            # Phase 77.5 (optional — default disabled)
    enabled: true
    turns_throttle: 3
    max_turns: 5
    max_consecutive_failures: 3
```

Agent-side config (`agents.yaml` or per-binding `llm.context_optimization.compaction`):

```yaml
compaction:
  enabled: true
  compact_at_pct: 0.7         # legacy threshold
  auto:                       # Phase 77.2
    token_pct: 0.80
    max_age_minutes: 120
    buffer_tokens: 13000
    min_turns_between: 5
    max_consecutive_failures: 3
```

See:
- [Context optimization](./context-optimization.md)
- [Hot reload](./hot-reload.md)

## Telemetry to watch

- `llm_compaction_triggered_total{agent,trigger,outcome}` — `trigger` is
  `token_pressure` or `age`
- `llm_compaction_duration_seconds{agent,outcome}`
- `agent_driver_compaction_requested_total{trigger}`
- `agent_driver_compaction_completed_total{outcome}`
- `agent_driver_compact_summary_stored_total`
- `agent_driver_extract_memories_completed_total`
- `agent_driver_extract_memories_skipped_total{reason}`
- prompt/token drift counters from token counter telemetry
