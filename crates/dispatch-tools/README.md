# nexo-dispatch-tools

The tool surface the chat agent (Cody) calls to drive the
programmer subsystem. Wraps the orchestrator + registry + tracker
+ hooks behind LLM-callable handlers, gates each call through a
capability check, and forwards every state transition to NATS so
the operator sees what the agent is doing.

## Where it sits

```
nexo-driver-loop        ← orchestrator (per-goal lifecycle)
nexo-driver-claude      ← bindings (origin, dispatcher)
nexo-agent-registry     ← admit / release / snapshot / logs
nexo-project-tracker    ← phases / followups / acceptance
        ↑
nexo-dispatch-tools     ← THIS crate (LLM tool handlers)
        ↑
nexo-core               ← agent runtime registers tools
```

This crate is the only place that knows how to glue all four
substrates together. Anything above it (core, main.rs) sees a
`DispatchToolContext` bundle and a list of tool names; anything
below it sees raw types.

## Tool groups (`tool_names.rs`)

`ToolGroup::{Read, Write, Admin}` — `allowed_tool_names(group)`
returns the canonical name list. `should_register(name, policy)`
is the per-binding gate that hot-reload uses to filter the
registered set without restarting the daemon.

| Group | Tools |
|---|---|
| Read | `project_status`, `project_phases_list`, `followup_detail`, `git_log_for_phase`, `agent_status`, `list_agents`, `agent_logs_tail`, `agent_hooks_list` |
| Write | `program_phase_dispatch`, `dispatch_followup`, `program_phase_chain`, `program_phase_parallel`, `cancel_agent`, `pause_agent`, `resume_agent`, `update_budget` |
| Admin | `set_concurrency_cap`, `flush_agent_queue`, `evict_completed` |

## Capability gate (`policy_gate.rs`)

`DispatchGate::admit(request) -> Result<(), DispatchDenied>` —
pure function. Inputs: `DispatchRequest { dispatcher_id,
phase_id, mode }` + `CapSnapshot { mode, max_concurrent,
allowed_phase_ids, forbidden_phase_ids, running_per_dispatcher }`.

Denials are typed: `ModeReadOnly`, `ModeNone`, `ConcurrencyCap`,
`PhaseNotAllowed`, `PhaseForbidden`. Each denial publishes a
`DispatchDeniedPayload` on `agent.dispatch.denied` (PT-7).

The per-binding override lives in `inbound_bindings[].dispatch_policy`
and supersedes the agent-level default; `EffectiveBindingPolicy`
in nexo-core resolves them.

## Dispatch handlers

### `program_phase_dispatch` (`program_phase.rs`)

Spawns a goal for one sub-phase. Inputs: `phase_id`, optional
`extra_context`, `budget_override: BudgetOverride`,
`acceptance_override: Vec<AcceptanceCriterion>`. Pipeline:

1. Resolve `SubPhase` from tracker (rejects unknown ids).
2. `DispatchGate::admit` against caller snapshot.
3. Build `Goal` — title from sub-phase, body from sub-phase
   prose, acceptance precedence: caller override → parsed
   `Acceptance:` bullets (B21) → workspace defaults
   (`cargo build --workspace`, `cargo test --workspace`).
4. `AgentRegistry::admit(goal_id, parent, dispatcher_id, …)` —
   reserves slot, persists row, fires `RegistryEvent::Admitted`.
5. Auto-attach the audit-before-done hook (B19 add_unique).
6. `DriverOrchestrator::spawn_goal`.
7. Publish `DispatchSpawnedPayload` on `agent.dispatch.spawned`.

### `dispatch_followup` (`dispatch_followup.rs`)

Same pipeline but for a `FollowUp` code (`PR-3`, `H-1`). The
synthesised `phase_id` is `followup:<code>` (used by the gate's
allow/forbid lists). Auto-audit attached the same way.

### `program_phase_chain` / `program_phase_parallel` (`chain.rs`)

Sequential and fan-out wrappers. `chain` admits N sub-phases,
links each one's `on_done` hook to dispatch the next via
`HookAction::DispatchPhase` (67.F.2). `parallel` admits all at
once subject to the dispatcher cap.

## Agent control + query (`agent_control.rs`, `agent_query.rs`)

Operator-style tools that target a live `GoalId`:

- `cancel_agent` — flips per-goal `CancellationToken`.
- `pause_agent` / `resume_agent` — flips watch::Sender<bool>.
- `update_budget` — only-grow override (B2).
- `agent_status({ goal_id })` — registry row + last 3 events.
- `list_agents({ filter? })` — table of all known goals.
- `agent_logs_tail({ goal_id, n })` — `LogBuffer` ring slice.
- `agent_hooks_list({ goal_id })` — registered hooks, one per
  trigger.

All tools cap output bytes for chat surfaces.

## Admin tools (`admin.rs`)

- `set_concurrency_cap({ dispatcher_id, max })` — hot-mutates
  the dispatcher cap in `RegistrySnapshot`.
- `flush_agent_queue({ dispatcher_id })` — cancels pending
  (admitted-but-not-spawned) goals.
- `evict_completed({ older_than_secs })` — sweeps registry rows.

Gated to admin-tier capability only.

## Completion hooks (`hooks/`)

| Module | Purpose |
|---|---|
| `types` | `CompletionHook { id, goal_id, trigger, action, payload }`, `HookTrigger::{OnDone, OnFail, OnTransition(state)}`, `HookAction::{NotifyOrigin, NotifyChannel, DispatchPhase, DispatchAudit, NatsPublish, Shell}`. |
| `dispatcher` | `HookDispatcher` trait + `DefaultHookDispatcher`. Resolves `HookAction` → side effect. `DispatchPhaseChainer` trait owns the orchestrator handle: `chain(parent, next_phase_id)` and `audit(parent_goal_id)`. `NatsHookPublisher` is the wire; `NoopNatsHookPublisher` for tests. |
| `registry` | `HookRegistry` + `HookRegistryStore` trait + `SqliteHookRegistryStore`. In-memory `DashMap<GoalId, Vec<CompletionHook>>` with mirror-through writes; `reload_from_store()` on boot (B17). `add_unique(goal_id, hook)` is idempotent on `(goal_id, trigger, action_kind)` — used by auto-audit (B19). `goal_ids()` lets the boot-time orphan sweep drop hooks for terminated goals (B23). |
| `idempotency` | `HookIdempotencyStore` — `(goal_id, hook_id, trigger_state)` SQLite table. `reserve()` returns `AlreadyDispatched` if hit. `release(key)` (B10) frees the slot when the action errors so a retry can fire. |

### Audit-before-done flow

Every dispatched goal gets one auto-attached hook:
`HookTrigger::OnDone → HookAction::DispatchAudit`. When Claude
claims done and acceptance passes, the dispatcher fires through
`AuditChainer::audit(parent)`:

1. Look up parent registry row → grab `last_diff_stat`.
2. Synthesize audit `Goal` with `workspace = workspace_root.join(parent_id)`
   (B22 — audit sees parent's worktree, not a fresh one).
3. Stuff `parent_diff` summary into the prompt context.
4. Admit with `enqueue=false` and a separate `audit_cap`
   counter so audits don't starve the main dispatcher cap (B24).
5. Audit goal's own acceptance is "review the diff and either
   approve or list blockers".

## Event forwarder (`event_forwarder.rs`)

`EventForwarder` wraps any `DriverEventSink` and:

- Mirrors every event into `LogBuffer` (B7) so `agent_logs_tail`
  has data without re-subscribing to NATS.
- Updates the registry on `AttemptCompleted` (`apply_attempt`)
  and on terminal states (`set_status`) — B5 + B6.
- Forwards the event to the inner sink (NATS in production).

This is the single seam between the driver loop and the rest of
the agent runtime.

## NATS subjects (`subjects.rs`)

| Subject | Payload | When |
|---|---|---|
| `agent.dispatch.spawned` | `DispatchSpawnedPayload` | After successful admit + spawn. |
| `agent.dispatch.denied` | `DispatchDeniedPayload` | Gate rejection. |
| `agent.hook.dispatched` | `HookDispatchedPayload` | After `HookDispatcher` runs an action. |
| `agent.hook.failed` | `HookFailedPayload` | Action errored; idempotency slot released. |
| `agent.registry.snapshot` | `registry_snapshot_subject` | Periodic snapshot for the admin UI. |

`DispatchTelemetry` trait lets tests inject `NoopTelemetry`;
production uses `NatsDispatchTelemetry` (PT-7).

## Bin

`bin/nexo_driver_tools.rs` — standalone CLI mirror of the
agent-callable tools. `dispatch <phase>`, `cancel <goal>`,
`status <goal>`, `list`, `hooks <goal>`. Same code paths as the
LLM uses, so operators can reproduce a Cody dispatch from a
shell.

## What's NOT here

- The orchestrator itself (`nexo-driver-loop`).
- The Claude subprocess plumbing (`nexo-driver-claude`).
- The MCP permission gate (`nexo-driver-permission`).
- The agent runtime that registers these tools as LLM-callable
  (`nexo-core::agent`).

## See also

- `architecture/driver-subsystem.md` — Phase 67 walkthrough.
- `architecture/project-tracker.md` — programmer agent overview.
- `proyecto/PHASES.md` §67 — sub-phase log including hook /
  capability work.
