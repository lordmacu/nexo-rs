# nexo-driver-loop

The brain of the programmer agent. Owns `DriverOrchestrator`,
the long-running async runtime that drives one `Goal` end-to-end
through Claude Code, gates every tool call through MCP, runs the
acceptance criteria, and emits events that downstream subscribers
(EventForwarder, registry, hooks) consume.

## Where it sits

```
nexo-driver-types       ← contract
   ↑
nexo-driver-claude      ← subprocess + bindings
   ↑
nexo-driver-permission  ← MCP gate
   ↑
nexo-driver-loop        ← THIS crate (orchestrator)
   ↑
nexo-dispatch-tools     ← tool surface for the LLM
   ↑
nexo-core               ← agent runtime
   ↑
src/main.rs             ← boot wiring
```

## Public surface (`DriverOrchestrator`)

### Construction

`DriverOrchestrator::builder()` — typed builder that requires
`claude_config`, `binding_store`, `decider`, `workspace_manager`,
`bin_path`, `socket_path` and accepts optional `acceptance`,
`event_sink`, `replay_policy`, `compact_policy`,
`compact_context_window`, `progress_every_turns`, `cancel_root`.

`build()` binds the Unix socket the MCP bin connects to (Phase
67.3) and returns the orchestrator.

### Per-goal lifecycle

| Method | Behaviour |
|---|---|
| `spawn_goal(self: Arc<Self>, goal) -> JoinHandle` | Phase 67.C.1 — fire-and-forget. The runner registers per-goal cancel + pause tokens, walks the loop, persists bindings, drains events. |
| `run_goal(&self, goal)` | The actual loop. Used by tests; production goes through `spawn_goal`. |
| `cancel_goal(GoalId)` / `is_cancelled(GoalId)` | Phase 67.G.2 — child token signals the loop to exit at the next safe point. |
| `pause_goal(GoalId)` / `resume_goal(GoalId)` / `is_paused(GoalId)` | Phase 67.C.2 — hold the loop between turns without killing the in-flight Claude turn. |
| `pre_register_goal(GoalId)` | B11 — wires cancel + pause tokens before `run_goal` starts so reattach paths can target them. |
| `interrupt_goal(GoalId, message)` | New — push an operator note that the next turn's Claude prompt sees as `[OPERATOR INTERRUPT]`. FIFO across multiple pushes. |
| `set_goal_max_turns(GoalId, new_max)` | B2 — only-grow override of the live budget. Other axes stay at the original goal's `BudgetGuards`. |
| `shutdown(self)` | Cancel root, drain socket server, await tasks. |

### Per-goal state (DashMap'd)

- `pause_signals: DashMap<GoalId, watch::Sender<bool>>`
- `cancel_tokens: DashMap<GoalId, CancellationToken>`
- `budget_overrides: DashMap<GoalId, BudgetGuards>`
- `pending_interrupts: DashMap<GoalId, VecDeque<String>>`

All four are wiped at goal exit — no leaks.

## The loop in one screen

```
loop {
    drain pause signal — block while paused
    if cancel_root or per-goal cancel → break
    if budget exhausted (with B2 override) → break
    publish AttemptStarted
    build extras: prior_failures + budget_meta + operator_messages
    checkpoint pre-attempt (Phase 67.6 git worktree)
    result = run_attempt(ctx, params)  // see attempt.rs
    publish AttemptCompleted
    if last_was_compact → continue (compact turn doesn't bump turn_index)
    apply diff_stat to extras
    every N turns → publish Progress (Phase 67.C.1)
    classify outcome via replay policy:
      Done → break
      NeedsRetry → feed failures into next prompt, continue
      Continue/Escalate → ask replay-policy: FreshSessionRetry /
                          NextTurn / Escalate
    classify with compact-policy: schedule /compact for next turn
}
```

`attempt.rs` does the actual `spawn_turn` plumbing: builds the
`ClaudeCommand`, drains events, handles `compact_turn` /
`operator_messages` extras, persists the binding with origin +
dispatcher (B1), runs the acceptance evaluator on Claude-claimed
done.

## Subsidiary subsystems in this crate

| Module | Phase | Purpose |
|---|---|---|
| `events` | — | `DriverEvent` enum + `DriverEventSink` trait. NATS subjects: `agent.driver.{goal,attempt}.{started,completed}`, `decision`, `acceptance`, `budget.exhausted`, `escalate`, `replay`, `compact`, `progress`. `NoopEventSink` for tests; `NatsEventSink` is the production wire. |
| `replay` | 67.8 | `ReplayPolicy` trait + `DefaultReplayPolicy`. Classifies mid-turn errors as `FreshSessionRetry` / `NextTurn` / `Escalate`. Reads recent `Decision` rows for deny-shortcut grounding. |
| `compact` | 67.9 | `CompactPolicy` trait + `DefaultCompactPolicy`. Schedules `/compact <focus>` slash commands when token pressure crosses threshold. |
| `acceptance` | 67.5 | `AcceptanceEvaluator` trait + `DefaultAcceptanceEvaluator`. Runs `Shell` / `FileMatch` / `Custom` criteria post-Claude-claimed-done. Two built-in custom verifiers: `no_paths_touched`, `git_clean`. |
| `workspace` | 67.6 | `WorkspaceManager` — git-worktree-per-goal sandbox. Per-turn checkpoints + rollback. |
| `socket` | 67.3 | `DriverSocketServer` — Unix socket the MCP bin in `nexo-driver-permission` connects to. |
| `mcp_config` | 67.3 | Writes the per-goal MCP config JSON Claude reads. |
| `config` | 67.4 | YAML schema (`DriverConfig`). |
| `bin/nexo_driver.rs` | — | Standalone `nexo-driver` CLI. `run <goal-yaml>`, `list-active`, `list-worktrees`, `rollback`. The agent-bin (`nexo-rs`) calls into this crate directly via `boot_dispatch_ctx_if_enabled`. |

## See also

- `architecture/project-tracker.md` — programmer agent overview.
- `architecture/driver-subsystem.md` — full Phase 67 walkthrough.
