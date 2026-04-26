# nexo-driver-types

The contract layer of the **programmer agent** subsystem. Pure
data — no runtime, no async, no IO. Every other driver crate
imports types from here so the surface a tool sees is the same
shape the orchestrator persists, and the wire between them is
JSON-stable.

## Why it exists

The programmer agent is a stack:

```
nexo-driver-types       ← you are here (leaf, no deps)
   ↑
nexo-driver-claude      ← Claude subprocess + session bindings
   ↑
nexo-driver-permission  ← MCP `permission_prompt` tool
   ↑
nexo-driver-loop        ← run_goal orchestrator + replay/compact
   ↑
nexo-dispatch-tools     ← agent-loop tools (program_phase, hooks)
```

If a type travels across that stack — through SQLite, NATS, or
across an `Arc<dyn Trait>` boundary — it lives here. Keeping it
all in a leaf crate means a config change can never accidentally
pull a runtime dep into a code path that has no business booting
one.

## What's in here

| Module | Type | Purpose |
|---|---|---|
| `goal` | `Goal { id, description, acceptance, budget, workspace, metadata }` | What the harness is trying to ship. `metadata` carries `origin_channel` + `dispatcher` JSON for the chat origin (B1). |
| `goal` | `GoalId` | Newtype around `Uuid`. |
| `goal` | `BudgetGuards` | Five-axis cap: turns / wall_time / tokens / consecutive_denies / consecutive_errors. |
| `goal` | `BudgetUsage`, `BudgetAxis` | Observed totals + which axis killed the run. |
| `attempt` | `AttemptParams { goal, turn_index, usage, cancel, extras }` | Per-turn input. `extras` is a `serde_json::Map` so the orchestrator can stamp ad-hoc fields (compact_turn, operator_messages, prior_failures) without bumping the contract. |
| `attempt` | `AttemptResult { goal_id, turn_index, outcome, usage_after, acceptance, final_text, harness_extras }` | Per-turn output. |
| `attempt` | `AttemptOutcome { Done, NeedsRetry, Continue, BudgetExhausted, Cancelled, Escalate }` | Why the turn ended. |
| `attempt` | `CompactParams`, `CompactResult` | Inputs/outputs for the compact policy (Phase 67.9). |
| `acceptance` | `AcceptanceCriterion { Shell, FileMatch, Custom }` | The cargo build / regex / custom verifier the goal must pass. `AcceptanceCriterion::shell("cmd")` is the shorthand. |
| `acceptance` | `AcceptanceVerdict { met, failures, evaluated_at, elapsed_ms }`, `AcceptanceFailure` | Result of running the criteria. |
| `decision` | `Decision { goal_id, recorded_at, request, outcome, rationale }` | One MCP-permission verdict, persisted for replay-policy + LLM decider grounding. |
| `cancel` | `CancellationToken` | Opaque wrapper. `child_token()` lets the orchestrator give each goal its own cancel knob (Phase 67.G.2). |

## What is NOT here

- `AgentHarness` trait — runtime-shaped, lives in `nexo-driver-loop`.
- `SessionBinding` schema — persistence-shaped, in `nexo-driver-claude`.
- Any tokio task / channel / mutex.

## Stability

Treat this as a public API: every new optional field is
`#[serde(default)]`, never breaks the wire. The `metadata` /
`extras` maps exist precisely so feature work stays out.

## See also

- `architecture/project-tracker.md` (mdBook) — programmer agent overview.
- `architecture/driver-subsystem.md` — Phase 67.0–67.9 driver loop walkthrough.
