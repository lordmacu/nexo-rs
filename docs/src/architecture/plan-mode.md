# Plan mode (Phase 79.1)

Plan mode is a per-goal toggle that puts the agent into a read-only
"exploration + design" phase. While active, every mutating tool call
is short-circuited at the dispatcher with a structured
`PlanModeRefusal`, and the model is expected to call
`ExitPlanMode { final_plan }` once it has a coherent plan. The
operator approves (or rejects) the plan via the pairing channel, and
plan mode flips back to off so the agent can implement.

The feature ports two prior agent CLI tools
(`EnterPlanModeTool` + `ExitPlanModeV2Tool`) with three deliberate
diffs from the upstream CLI:

| Decision | upstream | nexo-rs (Phase 79.1) |
|----------|------|---------------------|
| Approval channel | Local TUI dialog; `KAIROS_CHANNELS` flag DISABLES plan mode under chat channels | Pairing-friendly: every approval flows through the chat channel itself via `[plan-mode] approve\|reject plan_id=…` |
| Refusal payload | Free-form string from `validateInput` | Structured `PlanModeRefusal { tool_name, tool_kind, hint, entered_at, entered_reason }` |
| Plan body | Read from disk via `getPlanFilePath(agentId)` | `final_plan: String` arg, capped at 8 KiB — disk fallback parked as a follow-up |

## YAML knobs

```yaml
agents:
  - id: cody
    plan_mode:
      enabled: true                    # tool registered + reachable
      auto_enter_on_destructive: false # opt-in pairing with Phase 77.8
      default_active: ~                 # role-aware default (see below)
      approval_timeout_secs: 86400      # 24 h; goal stops with ApprovalTimeout if exceeded
      require_approval: false           # default safe rollout — flip to `true` in production

    inbound_bindings:
      - plugin: whatsapp
        instance: ops
        role: coordinator               # used by the role-aware `default_active`
        plan_mode:                      # full per-binding override
          require_approval: true
```

`default_active` is `null` by default. The runtime resolves it
through `PlanModePolicy::compute_default_active(role)`:

| Binding role | Default `default_active` |
|--------------|--------------------------|
| `coordinator` | `true` (these bindings drive non-trivial work) |
| `worker` | `false` (workers receive sub-goals from a coordinator that already planned) |
| `proactive` | `false` (Phase 77.20 ticks would be disrupted by a blocking approval flow) |
| unset / unknown | `false` (safest opt-out) |

Operators can pin the value with `default_active: true | false` to
override the role-aware default.

## Tools

### `EnterPlanMode`

Zero parameters except an optional `reason: string`. Returns

```json
{
  "entered_plan_mode": true,
  "already_in_plan_mode": false,
  "entered_at": 1700000000,
  "reason": "explore auth flow",
  "instructions": "..."
}
```

Hard guard: rejects with `PermissionDenied` when called from a
sub-agent / cron / poller / heartbeat-spawned goal. Lift from
`upstream agent CLI`,
refined with OpenClaw
`research/src/acp/session-interaction-mode.ts:4-15` — only
chat-rooted goals qualify because only they have a path to deliver
the operator approval that ends plan mode.

### `ExitPlanMode { final_plan: String }`

Submits the plan and (when `require_approval: true`) waits for
operator decision. Returns

```json
{
  "exited_plan_mode": true,
  "unlocked_at": 1700000123,
  "entered_at": 1700000000,
  "plan_bytes": 412,
  "plan_chars": 411,
  "plan_id": "01J…",
  "approval_required": true
}
```

### `plan_mode_resolve { plan_id, decision: approve|reject, reason? }`

Operator-side resolver. The pairing parser (future wiring) calls
this when it sees `[plan-mode] approve plan_id=…` or
`[plan-mode] reject plan_id=… reason=…` on an inbound message.
Direct callable too — tests and CLI ops use it without the parser
layer.

`reject` requires `reason` (non-empty); the rejection causes the
awaiting `ExitPlanMode` to fail with the canonical follow-up prompt
(lift from
`research/src/agents/bash-tools.exec-approval-followup.ts:27-40`):

```
Plan rejected by operator. Reason: <reason>.
Do not call ExitPlanMode again with the same plan.
Adjust the plan based on the rejection reason and present a revised plan.
```

Plan-mode stays ON after a reject so the model must revise.

## Mutating tools blocked while plan mode is on

Canonical list lives in
`crates/core/src/plan_mode.rs::MUTATING_TOOLS`. Adding a tool to the
runtime registry without classifying it (mutating OR read-only) makes
`ToolRegistry::assert_plan_mode_classified()` panic at boot in strict
deployments — ensures no tool silently bypasses the gate.

Currently classified mutating:

- `Bash` (gated when the next-shipped Phase 77.8 destructive
  classifier returns `is_mutating: true`; until 77.8 lands, every
  `Bash` call refuses fail-safe)
- `FileWrite`, `FileEdit`, `NotebookEdit`
- `program_phase`, `delegate_to`, `dispatch_followup`
- `TeamCreate`, `TeamDelete`
- `ScheduleCron`, `RemoteTrigger`
- `Config` (the `apply` op only — `read` and `propose` stay
  read-only)
- Plugin outbound names following the `<channel>.<verb>`
  convention (e.g. `whatsapp.send`, `browser.click`)

Classified read-only (always callable):

- `FileRead`, `Glob`, `Grep`, `WebSearch`, `WebFetch`
- `ListMcpResources`, `ReadMcpResource`, `ToolSearch`
- `AskUserQuestion`, `Sleep`
- `EnterPlanMode`, `ExitPlanMode`
- Memory + observability tools (`memory_search`, `agent_query`,
  `agent_turns_tail`, `session_logs`, `what_do_i_know`,
  `who_am_i`, `my_stats`)

## Notify-line formats (frozen)

Every transition emits a canonical line via `tracing::info!`. The
formats are frozen — operator dashboards and parsers read them. Any
change here is a breaking change.

```
[plan-mode] entered at <RFC3339> — reason: <model[: <text>]|operator|auto-destructive: <check>>
[plan-mode] awaiting approval plan_id=<UUID> (resolve via plan_mode_resolve { plan_id, decision: approve|reject })
[plan-mode] approved plan_id=<UUID>
[plan-mode] rejected plan_id=<UUID> reason=<…>
[plan-mode] approval timed out plan_id=<UUID>
[plan-mode] exited — plan: <first 200 chars>… (full plan in turn log #<turn_idx>)
[plan-mode] refused tool=<name> kind=<bash|file_edit|outbound|delegate|dispatch|schedule|config|read_only>
```

Future work: pipe these to `notify_origin` so the pairing channel
sees them directly (today they live in stdout / structured logs).

## Persistence

`agent_registry.goals.plan_mode` is a `TEXT` column carrying the
JSON-serialised `PlanModeState`. It survives daemon restart via the
Phase 71 reattach path: a goal that was in plan mode when the
daemon died comes back with the same state, and the per-turn
system-prompt hint resumes.

## Follow-ups

Tracked in `proyecto/FOLLOWUPS.md::Phase 79.1`:

- Operator-approval scope check (port from OpenClaw
  `roleScopesAllow` pattern when 79.10 ships).
- `final_plan_path` variant for plans larger than 8 KiB.
- Acceptance retry policy for flaky test suites.

## References

- **PRIMARY**: `upstream agent CLI`,
  `upstream agent CLI`,
  `upstream agent CLI`
  (`prepareContextForPlanMode`).
- **SECONDARY**:
  `research/src/acp/session-interaction-mode.ts:4-15`
  (interactive vs background sessions),
  `research/src/agents/bash-tools.exec-approval-followup.ts:27-40`
  (canonical reject follow-up prompt).
- Plan + spec: `proyecto/PHASES.md::79.1`.
