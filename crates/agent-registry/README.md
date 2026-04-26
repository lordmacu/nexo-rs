# nexo-agent-registry

The state-tracking layer of the programmer agent. Owns
`AgentRegistry`, the in-memory + SQLite map of every goal the
driver has admitted, so the chat tools can answer "what's
running" / "what did agent X just do" / "show me the audit
findings" across daemon restarts.

## Where it sits

```
nexo-driver-types       ← contract
nexo-driver-claude      ← OriginChannel, DispatcherIdentity
        ↓
nexo-agent-registry     ← THIS crate
        ↑
nexo-dispatch-tools     ← consumes the registry to render
                           list_agents / agent_status / etc.
```

The registry doesn't spawn anything itself. The orchestrator
admits goals here at dispatch time; the EventForwarder (in
`nexo-dispatch-tools`) pumps `AttemptResult` events into
`apply_attempt` so the snapshot stays live.

## Public surface

### `AgentRegistry::new(store, cap)`

`store: Arc<dyn AgentRegistryStore>` is either
`MemoryAgentRegistryStore` (dev) or `SqliteAgentRegistryStore`
(production). `cap` is the global concurrent-running limit;
beyond it, admit returns `Queued`.

### Lifecycle

| Method | Behaviour |
|---|---|
| `admit(handle, enqueue) -> AdmitOutcome` | PT-5 + B16 — atomic cap check + queue mutation under `admit_lock` (tokio mutex), then persistent upsert outside the lock so disk IO doesn't gate every admit. |
| `release(goal_id, terminal) -> Option<GoalId>` | B12 — atomic pop of the next queued goal. Caller is expected to call `promote_queued(id)` to flip status. |
| `promote_queued(id)` | Pops + sets status `Running` + persists. |
| `set_status(id, status)`, `set_max_turns(id, n)`, `set_acceptance(id, verdict)` | In-place edits, persisted. |
| `apply_attempt(&AttemptResult)` | B6 — refreshes `snapshot.turn_index / usage / last_acceptance / last_decision_summary / last_diff_stat / last_progress_text`. Idempotent against out-of-order replay. |
| `cap() / set_cap(new)` | Operator-tunable global cap (Phase 67.G.4 admin tool). |
| `flush_queue()` | Drains queued goals, marks each Cancelled. |
| `evict_terminal_older_than(cutoff)` | Drops in-memory + persistent terminal rows. |

### Read accessors

| Method | Behaviour |
|---|---|
| `snapshot(goal_id)` | Lock-free clone of the live `AgentSnapshot` (uses `ArcSwap`). |
| `handle(goal_id)` | Full `AgentHandle` for `agent_status` / hook payload construction. |
| `list()` | Merges live in-memory entries with persisted terminal-only rows so a single chat reply covers active + recent history. |
| `count_running()` | Cheap O(N) sweep used by `admit`. |

## Snapshot shape (`AgentSnapshot`)

`turn_index / max_turns / usage / last_acceptance /
last_decision_summary / last_event_at / last_diff_stat /
last_progress_text`. Held behind `Arc<ArcSwap<AgentSnapshot>>`
per entry so the hot path (`list_agents`, `agent_status`)
never blocks on writers.

## Reattach (Phase 67.B.4)

`reattach(registry, store, ReattachOptions { resume_running,
keep_terminal_for })` walks the SQLite store at boot and seeds
the in-memory map:

- `Running` rows with `resume_running=true` → `Resume(handle)`.
  Caller respawns or `pre_register_goal`s the orchestrator
  tokens before next intake.
- `Running` rows with `resume_running=false` → flipped to
  `LostOnRestart`.
- `Queued` rows → re-admitted as `Queued` (still in queue).
- `Paused` rows → kept paused.
- Terminal rows → kept if `finished_at` within
  `keep_terminal_for`, evicted from store otherwise.

## `LogBuffer`

Per-goal ring buffer of `(subject, summary, ts)` lines. Capped
to `capacity` at construction. The EventForwarder pushes a
line per driver event so `agent_logs_tail` doesn't re-stream
NATS. `tail(goal_id, n)` returns oldest-to-newest.

## Errors

`RegistryError { NotFound, CapReached, InvalidTransition,
Store(AgentRegistryStoreError) }`. Most paths bubble to the
caller (the dispatch tool) which renders the friendly message.

## See also

- `architecture/project-tracker.md` — programmer agent overview.
- `architecture/driver-subsystem.md` — Phase 67.B detail.
