# Project tracker + multi-agent dispatch (Phase 67.A–H)

The project-tracker subsystem lets a `nexo-rs` agent answer "qué fase
va el desarrollo" through Telegram / WhatsApp / a shell, and lets it
dispatch async programmer agents that ship phases on its behalf.

The implementation is layered:

| Layer | Crate | Responsibility |
|-------|-------|----------------|
| Project files | `nexo-project-tracker` | Parse `PHASES.md` + `FOLLOWUPS.md`, watch for changes, expose read tools. |
| Multi-agent state | `nexo-agent-registry` | DashMap + SQLite store of every in-flight goal, cap + queue + reattach. |
| Goal control | `nexo-driver-loop` | `spawn_goal` / `pause_goal` / `resume_goal` / `cancel_goal` per-goal. |
| Tool surface | `nexo-dispatch-tools` | `program_phase`, `dispatch_followup`, hook system, agent control + query, admin. |
| Capability gate | `nexo-config` + `nexo-core` | `DispatchPolicy` per agent / binding, `ToolRegistry` filter. |

## Project tracker (Phase 67.A)

`FsProjectTracker` reads `<root>/PHASES.md` (required) and
`<root>/FOLLOWUPS.md` (optional) at startup, caches parsed state
behind a parking-lot RwLock with a 60 s TTL, and starts a `notify`
watcher on the parent directory that invalidates the cache on
`Modify | Create | Remove` events.

Read tools register through `nexo_dispatch_tools::READ_TOOL_NAMES`
(`project_status`, `project_phases_list`, `followup_detail`,
`git_log_for_phase`).

Set `${NEXO_PROJECT_ROOT}` to point at a workspace other than the
daemon's cwd.

## Multi-agent registry (Phase 67.B)

`AgentRegistry` is the single source of truth for every goal the
driver has admitted. Each entry holds an `ArcSwap<AgentSnapshot>`
(turn N/M, last acceptance, last decision summary, diff_stat) so
`list_agents` / `agent_status` readers never block writers.

- `admit(handle, enqueue)` enforces the global cap. Beyond the cap,
  `enqueue=true` parks the goal as `Queued`; `enqueue=false` rejects.
- `release(goal_id, terminal)` returns the next-up queued goal so
  the orchestrator can promote it via `promote_queued` once the
  worktree / binding is ready.
- `apply_attempt(AttemptResult)` refreshes the live snapshot.
  Idempotent against out-of-order replay (lower turn_index ignored).
- Reattach (Phase 67.B.4) walks the SQLite store at boot and
  rehydrates `Running` rows. With `resume_running=false` they flip
  to `LostOnRestart` and surface to the operator.

`LogBuffer` keeps a per-goal ring of recent driver events for the
`agent_logs_tail` tool — bounded so a chatty goal cannot OOM the
process.

## Async dispatch (Phase 67.C + 67.E)

`DriverOrchestrator::spawn_goal(self: Arc<Self>, goal)` returns a
`tokio::task::JoinHandle` so the calling tool returns the goal id
instantly without waiting for the run to finish. Per-goal pause /
cancel signals (`watch<bool>` and `CancellationToken::child_token`)
let `pause_agent` / `cancel_agent` target one goal without taking
down the rest of the orchestrator.

`program_phase_dispatch` is the heart of the dispatch surface: it
reads the sub-phase out of PHASES.md, runs `DispatchGate::check`,
constructs a `Goal` with the dispatcher / origin metadata, asks the
registry for a slot, and either spawns the goal or returns
`Queued` / `Forbidden` / `NotFound`. `dispatch_followup` is the
mirror that pulls the description from a FOLLOWUPS.md item.

## Capability gate (Phase 67.D)

`DispatchPolicy { mode, max_concurrent_per_dispatcher,
allowed_phase_ids, forbidden_phase_ids }` lives on `AgentConfig`
and (as `Option<DispatchPolicy>`) on `InboundBinding`. The
per-binding override fully replaces the agent-level value so an
operator can be precise per channel ("`asistente` is `none`
everywhere except this Telegram chat where it is `full`").

`DispatchGate::check` short-circuits in this order:

1. capability `None` → `CapabilityNone` (every kind).
2. `ReadOnly` capability + write kind → `CapabilityReadOnly`.
3. write + `require_trusted` + `!sender_trusted` →
   `SenderNotTrusted`. Read tools bypass the trust gate so
   `list_agents` stays open for unpaired senders.
4. `forbidden_phase_ids` match → `PhaseForbidden`.
5. non-empty `allowed_phase_ids` + no match → `PhaseNotAllowed`.
6. dispatcher / sender / global caps. Global cap with
   `queue_when_full=true` is admitted; the orchestrator queues it.
   Without queue → `GlobalCapReached`.

`ToolRegistry::apply_dispatch_capability(policy, is_admin)` prunes
the registry of dispatch tool names not allowed by the resolved
policy. `ToolRegistryCache::get_or_build_with_dispatch` builds the
per-binding filtered registry that respects both `allowed_tools`
and `dispatch_policy`. Hot reload (Phase 18) constructs a fresh
`ToolRegistryCache` per snapshot, so a new dispatch_policy lands
on the next intake without restart; in-flight goals keep their
pre-reload tool surface so a hot reload never preempts.

## Completion hooks (Phase 67.F)

Each hook is `(on: HookTrigger, action: HookAction, id)`. Triggers
fire on `Done | Failed | Cancelled | Progress { every_turns }`.
Actions:

- `notify_origin` — publish a markdown summary to the chat that
  triggered the goal. No-op when `origin.plugin == "console"`.
- `notify_channel { plugin, instance, recipient }` — publish to an
  explicit channel different from the origin (escalate to ops).
- `dispatch_phase { phase_id, only_if }` — chain another goal when
  `only_if` matches the firing transition. Implemented via a
  pluggable `DispatchPhaseChainer` so the runtime owns
  `program_phase_dispatch` plumbing.
- `nats_publish { subject }` — JSON payload to a custom subject.
- `shell { cmd, timeout }` — opt-in via `allow_shell_hooks`.
  Capability `PROGRAM_PHASE_ALLOW_SHELL_HOOKS` registered with the
  setup inventory so `agent doctor capabilities` flags it the
  moment the operator exports the env var. Receives
  `NEXO_HOOK_GOAL_ID` / `PHASE_ID` / `TRANSITION` /
  `PAYLOAD_JSON` env vars.

`HookIdempotencyStore` (SQLite) keeps `(goal_id, transition,
action_kind, action_id)` UNIQUE so at-least-once NATS replay or a
mid-hook restart cannot fire a hook twice.

`HookRegistry` (in-memory `DashMap<GoalId, Vec<CompletionHook>>`)
backs `add_hook` / `remove_hook` / `agent_hooks_list`.

## NATS subjects (Phase 67.H.2)

| Subject | Producer |
|---------|----------|
| `agent.dispatch.spawned` | `program_phase_dispatch` admitted |
| `agent.dispatch.denied` | `DispatchGate::check` denied |
| `agent.tool.hook.dispatched` | hook fired ok |
| `agent.tool.hook.failed` | hook attempt errored |
| `agent.registry.snapshot.<goal_id>` | per-goal periodic beacon |
| `agent.driver.progress` | every Nth completed work-turn |

Plus the existing Phase 67.0–67.9 subjects:
`agent.driver.{goal,attempt}.{started,completed}`,
`agent.driver.{decision,acceptance,budget.exhausted,escalate,replay,compact}`.

## CLI (Phase 67.H.1)

`nexo-driver-tools` mirrors the chat tool surface for shell use:

```text
nexo-driver-tools status [--phase <id> | --followups]
nexo-driver-tools dispatch <phase_id>
nexo-driver-tools agents list [--filter running|queued|...]
nexo-driver-tools agents show <goal_id>
nexo-driver-tools agents cancel <goal_id> [--reason "…"]
```

`origin.plugin = "console"` so `notify_origin` is a no-op (the
operator sees stdout, not a chat reply).

## See also

- `proyecto/PHASES.md` — Phase 67.A–H sub-phase status of record.
- `architecture/driver-subsystem.md` — Phase 67.0–67.9 driver loop
  + replay + compact policies.
