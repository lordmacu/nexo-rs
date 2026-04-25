# nexo-driver-types

Types and trait surface for the Nexo **driver subsystem** — Phase 67 of
the agent framework.

The driver subsystem runs another process (a CLI such as `claude` or a
local agent runtime) under a verifiable goal. This crate is a *leaf* —
no `nexo-core` dep, no runtime — so the contract can travel through
NATS, get re-imported by extensions, and be consumed by admin-ui
without dragging in the full daemon.

What lives here:

- `AgentHarness` trait — drives one attempt against a goal
- `Goal`, `BudgetGuards`, `BudgetUsage` — what we're trying to achieve, and how much it can cost
- `AcceptanceCriterion`, `AcceptanceVerdict` — objective verification (Claude says "done"; we still check)
- `Decision`, `DecisionChoice` — record of every allow / deny taken during an attempt
- `AttemptOutcome` — terminal states an attempt can land in

What does NOT live here:

- Concrete harness implementations — `nexo-driver-claude` (Phase 67.1)
- Driver loop / scheduler — Phase 67.4
- Acceptance evaluator — Phase 67.5
- MCP `permission_prompt` — Phase 67.3
