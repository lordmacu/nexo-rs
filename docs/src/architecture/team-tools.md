# Team tools (Phase 79.6)

Five LLM tools that let an agent form a *named team* of up to
8 sub-agents that operate in parallel, share a Phase 14 TaskFlow
task list, and communicate via broker DMs. Distinct from the
existing 1-to-1 `delegate` (Phase 8) which is request/reply
between named agents — teams are coordinated multi-member work
rooted under one lead.

## Five tools

| Tool | Op kind | Plan-mode |
|------|---------|-----------|
| `TeamCreate` | mutating | Delegate (refused) |
| `TeamDelete` | mutating | Delegate (refused) |
| `TeamSendMessage` | mutating | Delegate (refused) |
| `TeamList` | read-only | always callable |
| `TeamStatus` | read-only | always callable |

`MUTATING_TOOLS` and `READ_ONLY_TOOLS` in
`crates/core/src/plan_mode.rs` are the source of truth.

## Per-agent YAML

```yaml
agents:
  - id: cody
    team:
      enabled: true            # default false; opt-in
      max_members: 8           # clamped at 8
      max_concurrent: 4        # clamped at 4
      idle_timeout_secs: 3600  # 1 h stale-team threshold
      worktree_per_member: false  # default for TeamCreate;
                                   # per-call override accepted
```

When `enabled: false` (the default) the 5 tools are not
registered for this agent — the model never sees them advertised.

## SQL store

Three tables in `<state_dir>/teams.db` (idempotent CREATE):

```sql
CREATE TABLE teams (
    team_id              TEXT PRIMARY KEY,
    display_name         TEXT NOT NULL,
    description          TEXT,
    lead_agent_id        TEXT NOT NULL,
    lead_goal_id         TEXT NOT NULL,
    flow_id              TEXT NOT NULL,
    worktree_per_member  INTEGER NOT NULL,
    created_at           INTEGER NOT NULL,
    deleted_at           INTEGER,
    last_active_at       INTEGER NOT NULL
);

CREATE TABLE team_members (
    team_id        TEXT NOT NULL,
    name           TEXT NOT NULL,        -- human-readable handle
    agent_id       TEXT NOT NULL,        -- internal UUID
    agent_type     TEXT,
    model          TEXT,
    goal_id        TEXT NOT NULL,
    worktree_path  TEXT,
    joined_at      INTEGER NOT NULL,
    is_active      INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL,
    PRIMARY KEY (team_id, name)
);

CREATE TABLE team_events (
    event_id          TEXT PRIMARY KEY,
    team_id           TEXT NOT NULL,
    kind              TEXT NOT NULL,
    actor_member_name TEXT,
    payload_json      TEXT NOT NULL,
    created_at        INTEGER NOT NULL
);
```

`team_id` is the sanitised name (lowercase + non-alnum → `-`,
matching `claude-code-leak/src/utils/swarm/teamHelpers.ts:100-102`).
Composite PK on `(team_id, name)` enforces unique member names
within a team. `deleted_at IS NOT NULL` ⇒ soft-deleted.

## Broker topics

| Topic | Direction | Payload |
|-------|-----------|---------|
| `team.<team_id>.dm.<member_name>` | point-to-point | `DmFrame` JSON |
| `team.<team_id>.broadcast` | fan-out (lead only) | `DmFrame` JSON |

```rust
pub struct DmFrame {
    pub team_id: String,
    pub to: String,         // member_name or "broadcast"
    pub from: String,
    pub body: serde_json::Value,
    pub correlation_id: Option<String>,
}
```

The router subscribes once per process to `team.>`; per-team
in-memory `tokio::sync::broadcast::Sender` channels deliver to
member runtimes. When a member's goal is `Pending` (idle
between turns), Phase 67's wake-on hook flips it to `Running`
on the first DM.

## Lifecycle

```
+-----------+     TeamCreate      +-----------+
| no team   |  ---------------->  | team      |
+-----------+                     | (1 lead)  |
                                  +-----+-----+
                                        |
                                        | (operator/79.6.b adds members)
                                        v
                                  +-----------+
                                  | team      |
                                  | (N>=2)    |
                                  +-----+-----+
                                        |
              +-------------------------+--------------------------+
              |                         |                          |
              v                         v                          v
      TeamSendMessage          TeamSendMessage             TeamDelete
      to: <member>             to: "broadcast"             (zero running)
      DM                       fan-out                     soft delete
      ↻ wake idle              ↻ wake all                  + drop router
```

## Caps

- `TEAM_MAX_MEMBERS = 8` (incl. lead).
- `TEAM_MAX_CONCURRENT_DEFAULT = 4` per agent.
- `TEAM_NAME_MAX_LEN = 64`, `MEMBER_NAME_MAX_LEN = 32`.
- `TEAM_IDLE_TIMEOUT_SECS = 3600` — reaper marks teams stale.
- `SHUTDOWN_DRAIN_SECS = 30` — TeamDelete drain budget.
- `DM_BODY_MAX_BYTES = 64 * 1024` per `TeamSendMessage`.

Per-agent YAML can lower `max_members` / `max_concurrent` but
never raise above the constants.

## Error kinds

`{ ok: false, kind: "...", error: "..." }` shape:

- `TeamingDisabled` — `team.enabled = false` for this agent.
- `InvalidName` / `InvalidMemberName`.
- `TeamNameTaken { existing_team_id }`.
- `TeamNotFound` / `TeamDeleted`.
- `MemberNotFound`.
- `TeamFull { count, cap }`.
- `ConcurrentCapExceeded { count, cap }`.
- `BodyTooLarge { actual, max }`.
- `NotLeader` — only the team lead can `TeamDelete` /
  broadcast.
- `OnlyLeadCanBroadcast` — non-lead member tried `to: "broadcast"`.
- `NotMember` — caller is neither the lead nor a current
  member; `TeamStatus` refuses without confirming team
  existence.
- `BlockedByActiveMembers { names: [...] }` — `TeamDelete`
  while members are still `Running`.
- `TeammateCannotSpawnTeammate` — caller's `AgentContext`
  has `team_member_name = Some(...)`. Mirrors the leak's
  `claude-code-leak/src/tools/AgentTool/prompt.ts:279-282`.
- `Wire` — missing required arg or malformed shape.

## Plan-mode behaviour

- `TeamCreate`, `TeamDelete`, `TeamSendMessage` are in
  `MUTATING_TOOLS`. Under an active plan-mode they refuse with
  `PlanModeRefusal { tool_kind: Delegate }`.
- `TeamList`, `TeamStatus` are in `READ_ONLY_TOOLS`. Always
  callable.

## Comparison vs `delegate` (Phase 8)

| | `delegate` (Phase 8) | Team* (Phase 79.6) |
|--|----------------------|---------------------|
| Topology | 1 → 1 | 1 lead + N parallel members |
| Lifecycle | request/reply | persistent team + audit log |
| Storage | none (broker only) | SQLite (3 tables) |
| Comms | `agent.route.{target}` | `team.{id}.dm.{name}` + `team.{id}.broadcast` |
| Idle/wake | n/a | goal `Pending` ↔ `Running` |
| Capability gate | `allowed_delegates` | `team.enabled` + caps |
| Best for | quick request to known peer | research fan-out, multi-source verify, full-stack work |

## Deferred to 79.6.b

- Spawn-as-teammate via Phase 67 dispatch (`AgentContext.team_id`
  injection is wired; the actual sub-goal spawn that registers
  in `team_members` is the missing piece).
- Phase 14 FlowFlow link (`flow_id` is currently a placeholder
  equal to `team_id`).
- `nexo team list / status / drop` operator CLI.
- Force-kill drain in TeamDelete (today blocks; 79.6.b cancels
  in-flight goals after `SHUTDOWN_DRAIN_SECS`).
- MCP server mode (`run_mcp_server`) exposes the 5 tools — part
  of the 79.M MCP exposure parity sweep.

## References

- **PRIMARY**: `claude-code-leak/src/tools/TeamCreateTool/`,
  `claude-code-leak/src/tools/TeamDeleteTool/`,
  `claude-code-leak/src/tools/SendMessageTool/`,
  `claude-code-leak/src/utils/swarm/{teamHelpers.ts,constants.ts}`.
- **Spec**: `proyecto/PHASES.md::79.6`.
