# Context optimization

Four independent mechanisms reduce the number of tokens sent to the LLM
on every request, without changing the agent's behavior. They live
under `llm.context_optimization` in `llm.yaml` and can be flipped per
agent under `agents.<id>.context_optimization`.

```yaml
# config/llm.yaml
context_optimization:
  prompt_cache:
    enabled: true                   # default
    long_ttl_providers: [anthropic, vertex]
  compaction:
    enabled: false                  # default off — opt in per agent
    compact_at_pct: 0.75
    tail_keep_tokens: 20000
    tool_result_max_pct: 0.30
    summarizer_model: ""            # empty = reuse the agent's main model
    lock_ttl_seconds: 300
  token_counter:
    enabled: true                   # default
    backend: auto                   # auto | anthropic_api | tiktoken
    cache_capacity: 1024
  workspace_cache:
    enabled: true                   # default
    watch_debounce_ms: 500
    max_age_seconds: 0              # 0 = never force refresh (notify is authoritative)
```

## 1. Prompt caching

Materializes the system prompt as a list of `cache_control` blocks on
the Anthropic wire so the stable prefix (workspace + skills + tool
catalog + binding glue) is billed at **0.1× input cost** on every cache
hit. OpenAI / DeepSeek paths surface their automatic
`prompt_tokens_details.cached_tokens` field through the same
`CacheUsage` struct. Gemini and MiniMax flatten the blocks into the
legacy system slot today (warned once per process).

Block layout (4 cache breakpoints, the Anthropic max):

1. `workspace` — IDENTITY / SOUL / USER / AGENTS / MEMORY (`Ephemeral1h`)
2. `skills` — per-binding skill catalog (`Ephemeral1h`)
3. `binding_glue` — peer directory + per-binding system prompt + language directive (`Ephemeral1h`)
4. `channel_meta` — sender id + per-turn context (`Ephemeral5m`)

Tools array is sorted alphabetically by name (the registry iterates a
non-deterministic `DashMap`) and the **last** tool gets a 1h
`cache_control` marker when `cache_tools=true`.

### What to watch
- `llm_cache_read_tokens_total{agent, provider, model}` — should
  dominate `llm_cache_creation_tokens_total` after the first turn of a
  warm session.
- `llm_cache_hit_ratio{agent}` — target >0.7 on multi-turn agents;
  <0.3 means you're paying the write premium without the discount.

### When to flip off
- Provider rejects the request with a 400 mentioning `cache_control`
  (very old model). Mitigation: the framework already strips markers
  for `claude-2.x`; if Anthropic adds another exception, override
  `ANTHROPIC_CACHE_BETA="..."` to disable the beta header.
- A custom-built LLM gateway in front of Anthropic doesn't pass the
  `cache_control` field through.

## 2. Compaction (online history folding)

When the pre-flight token estimate crosses `compact_at_pct *
effective_window`, the agent runs a secondary LLM call to fold
`history[..tail_start]` into a single summary string. The summary
replaces the head; the last `tail_keep_tokens` worth of turns ride
forward verbatim. Subsequent turns prepend the summary as a synthetic
user/assistant pair so Anthropic's role-alternation rule stays valid.

Defaults are intentionally conservative: **off** by default. Roll out
per agent via `agents.<id>.context_optimization.compaction: true`.

```yaml
agents:
  - id: ana
    context_optimization:
      compaction: true   # ana opts in early, others stay off
```

### What to watch
- `llm_compaction_triggered_total{agent, outcome}` — outcomes are
  `ok`, `failed`, `lock_held`, `no_boundary`, `tool_result_truncated`.
- `llm_compaction_duration_seconds{agent, outcome="ok"|"failed"}` — a
  rising p99 means the summarizer model is overloaded; lower
  `compact_at_pct` so triggers are smaller (cheaper) and more frequent.

### When to flip off
- Quality regression in long sessions — the summary may be losing
  active-task state. Inspect `compactions_v1` rows in the SQLite store
  to see what was folded; bump `tail_keep_tokens` so more verbatim
  context survives.
- Lock contention spikes — multiple processes (NATS multi-node) racing
  on the same session. The lock is per-session so this only happens
  with sticky-session misrouting; fix at the broker level rather than
  disabling compaction.

### Safety nets
- `compaction_locks_v1` carries TTL (`lock_ttl_seconds`) — a crashed
  compactor doesn't deadlock the session; the next acquire after the
  TTL wins automatically.
- Audit log: every successful compaction inserts a row in
  `compactions_v1` with the summary text + token cost. Inspect with
  `sqlite3 memory.db "SELECT * FROM compactions_v1 WHERE session_id =
  ? ORDER BY compacted_at DESC"`.
- Failure path: 3 retries with backoff; on total failure the original
  history goes to the LLM unchanged (graceful degradation, never silent
  data loss).

## 3. Token counting (pre-flight sizing)

`TokenCounter` trait with two backends:

- **AnthropicTokenCounter** — calls
  `POST /v1/messages/count_tokens`. Exact (matches billing).
  LRU-cached on `blake3(payload)`: the stable tools+identity prefix
  hashes the same on every turn, so the network round-trip happens
  ~once per process lifetime.
- **TiktokenCounter** — offline `cl100k_base` approximation. Drift
  vs Anthropic billing measured at 5–15%. Fine for budget gating,
  not for hard limits.

The cascade wraps the primary in a `CircuitBreaker`
(failure_threshold=3, 30s→300s backoff): on count_tokens outage the
agent loop falls back to tiktoken so the request still goes through.
Once the breaker has opened at least once, `is_exact()` flips to false
for the rest of the process so dashboards don't conflate sample
populations.

### What to watch
- `llm_prompt_tokens_estimated{agent, provider, model}` — compare
  against `llm_prompt_tokens_drift{...}` (histogram in percent).
- A drift p99 climbing past 20% means the active backend is wrong for
  your model — switch from `tiktoken` to `anthropic_api` (or vice
  versa for non-Anthropic providers).

### When to flip off
- The agent runs against a self-hosted gateway that doesn't honor
  `count_tokens`. Set `backend: tiktoken` to skip the round-trip.

## 4. Workspace bundle cache

Reads of IDENTITY / SOUL / USER / AGENTS / MEMORY MDs go through an
in-memory `Arc<WorkspaceBundle>` cache keyed by `(root, scope, sorted
extras)`. A `notify-debouncer-full` watcher (default 500ms) drops
every entry under a workspace root when any `*.md` changes. Non-MD
file changes are ignored.

### What to watch
- `workspace_cache_hits_total{path}` should dominate
  `workspace_cache_misses_total{path}` once the cache is warm.
- `workspace_cache_invalidations_total{path}` rising without operator
  edits points to a tool that writes to the workspace too aggressively.

### When to flip off
- NFS / FUSE filesystems where `notify(7)` drops events. Set
  `workspace_cache.max_age_seconds: 60` (or similar) to force a
  refresh after the absolute TTL even without a watch event.

## Per-agent overrides

The four enables — and only the enables — can be flipped per agent in
`agents.yaml`. The numeric knobs (`compact_at_pct`, `tail_keep_tokens`,
`watch_debounce_ms`, …) stay global to keep the surface narrow.

```yaml
agents:
  - id: ana
    context_optimization:
      prompt_cache: true
      compaction: true
      token_counter: true
      workspace_cache: true
  - id: bob
    context_optimization:
      prompt_cache: false  # bob runs against a gateway that strips cache_control
```

## Hot-reload behavior

Changing global knobs (`llm.yaml`) takes effect on the next request
once the reload coordinator picks up the file change (Phase 18). For
**per-agent enables**, the override rides on `Arc<AgentConfig>` inside
`RuntimeSnapshot` and is observed on the next
`policy_for(...)` lookup. The `LlmAgentBehavior` struct itself still
caches its compactor / prompt_cache_enabled fields at construction —
toggling those without a process restart requires the future
`ArcSwap<CompactionRuntime>` refactor noted in `proyecto/FOLLOWUPS.md`.

## Rollout playbook

1. Deploy with everything at defaults — `prompt_cache=true`,
   `compaction=false`, `token_counter=true`, `workspace_cache=true`.
2. Watch `llm_cache_hit_ratio` for 24h. Expect it to climb to >0.7
   on chatty agents; if it stays low, check that the workspace bundle
   is stable across turns (no MD writes mid-session).
3. Pick one agent, opt it into compaction (`agents.<id>.context_optimization.compaction:
   true`), reload config, watch for a week.
4. If `llm_compaction_triggered_total{outcome="ok"}` > 0 and quality
   feedback is positive, roll compaction out to the rest of the fleet.
5. If drift on `llm_prompt_tokens_drift` is consistently <10%, leave
   `token_counter.backend: auto`. If higher, consider
   `backend: tiktoken` for non-Anthropic providers — saves the
   round-trip without losing accuracy you didn't have anyway.
