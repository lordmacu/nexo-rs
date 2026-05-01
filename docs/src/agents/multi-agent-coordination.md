# Multi-agent coordination

Two LLM tools — `list_peers` and `send_to_peer` — let in-process
agents discover each other and exchange messages without going
through a delegate-style RPC. Pairs cleanly with [assistant mode](./assistant-mode.md):
the agent's plain text output is NOT visible to other agents, so
peer messaging IS the only way to communicate.

## Subject contract

Per-goal NATS subject:

```
agent.inbox.<goal_id>
```

Wire format is JSON. Payload is `InboxMessage`:

```rust
pub struct InboxMessage {
    pub from_agent_id: String,
    pub from_goal_id: GoalId,
    pub to_agent_id: String,
    pub body: String,
    pub sent_at: DateTime<Utc>,
    pub correlation_id: Option<Uuid>,
}
```

`correlation_id` is omitted from the wire when `None` so
request/response patterns can re-use the channel without
accidentally injecting noise on one-shots.

Constants exposed via `nexo_core::agent::inbox`:

- `INBOX_SUBJECT_PREFIX = "agent.inbox"`
- `MIN_BODY_CHARS = 1`
- `MAX_BODY_BYTES = 64 * 1024`

## `list_peers` LLM tool

Read-only enumeration. No-arg shape; returns peer summaries
excluding the calling agent. Each entry includes a `reachable`
flag based on the binding's `allowed_delegates` filter (Phase 16).

```json
{
  "peers": [
    {
      "agent_id": "researcher",
      "description": "research agent",
      "reachable": true
    },
    {
      "agent_id": "writer",
      "description": "writer agent",
      "reachable": false
    }
  ]
}
```

When the binding has no `PeerDirectory` configured, returns:

```json
{
  "peers": [],
  "note": "this agent has no PeerDirectory configured"
}
```

Use this BEFORE `send_to_peer` to discover valid `to:` targets.

## `send_to_peer` LLM tool

Fire-and-forget. Resolves `to:` to a peer agent_id, looks up the
peer's live goals, publishes the `InboxMessage` to each goal's
inbox subject, returns the per-goal delivery report.

### Tool shape

```json
{
  "to": "researcher",
  "message": "task #1 ready for handoff",
  "correlation_id": "7a3b2f00-..."   // optional UUID
}
```

### Output

```json
{
  "delivered_to": ["b91c2d3a-...", "f88e1100-..."],
  "unreachable_reasons": []
}
```

Or, on failures:

```json
{
  "delivered_to": [],
  "unreachable_reasons": [
    "unknown agent_id `ghost`"
  ]
}
```

### Validation gates

The handler walks 6 gates:

1. `to` must be present + non-empty after trim
2. `to != ctx.agent_id` (self-sends rejected with explicit error)
3. `message` must be present + non-empty
4. Body ≤ `MAX_BODY_BYTES` (64 KB cap; rejected with explicit limit)
5. `to` must exist in `PeerDirectory` (fast-path "unknown agent_id"
   unreachable when not — fail-fast before broker round-trip)
6. Lookup must return at least one live goal id (empty → "no live
   goals" unreachable)

When all 6 pass, the handler iterates the live goals, builds an
`InboxMessage` per goal with `from_goal_id =
ctx.session_id.map(GoalId).unwrap_or_else(GoalId::new)` (best-
effort; provenance preserved via `from_agent_id`), publishes via
the broker, and accumulates per-goal results.

### Per-goal fan-out

A peer with multiple live goals (Bg + Daemon + Interactive) gets
the message at every goal's inbox. Per-goal failures don't cancel
the whole call — `unreachable_reasons` accumulates while
`delivered_to` records successful publishes.

## Receive side (Phase 80.11.b)

Peer messages are queued in a per-goal in-memory FIFO buffer with
a 64-message cap (FIFO eviction on overflow). The receiving
goal's runtime drains the buffer at next turn start and renders
the messages as a `<peer-message from="...">` system block:

```
# PEER MESSAGES

<peer-message from="researcher" sent_at="2026-04-30T14:00:00+00:00">
task #1 ready for handoff
</peer-message>
```

`correlation_id` attribute is added when `Some` so the receiver
can correlate replies back through the same channel.

### Buffer-on-demand

Messages addressed to a goal that hasn't `register()`'d yet
queue in a fresh buffer. When the goal eventually registers, it
sees the buffered messages — race-safe under fast-spawn-then-
immediate-send.

### Wiring

```rust
use nexo_core::agent::inbox_router::{InboxRouter, render_peer_messages_block};

// Boot — single per-process spawn.
let router = InboxRouter::new(broker.clone());
let _handle = router.spawn(cancel.clone());

// Per-goal startup.
let buf = router.register(goal_id);
ctx.inbox_buffer = Some(buf);

// Per-turn loop, adjacent to assistant addendum push site.
let drained = ctx.inbox_buffer.as_ref().map(|b| b.drain()).unwrap_or_default();
if let Some(block) = render_peer_messages_block(&drained) {
    channel_meta_parts.push(block);
}

// Goal terminal.
router.forget(goal_id);
```

## Defense-in-depth

| Edge case | Behaviour |
|-----------|-----------|
| Self-send | Rejected by handler |
| Body > 64 KB | Rejected with explicit limit in error |
| Empty body | Rejected |
| Unknown agent_id | `unreachable_reasons: ["unknown agent_id ..."]` |
| No live goals for peer | `unreachable_reasons: ["no live goals ..."]` |
| Per-goal publish fails | Recorded in `unreachable_reasons`, others continue |
| Buffer full (64 messages) | FIFO eviction with `tracing::warn!` |
| Subject malformed | Dropped with `tracing::debug!` |
| Payload garbage | Dropped with `tracing::debug!` |
| Race: peer terminates between `list_peers` and `send_to_peer` | Falls through `unreachable_reasons` not panic |

## Deferred follow-ups

- **80.11.b.b** — Hook `InboxRouter` drain + render into the
  per-turn loop in `llm_behavior.rs` (1-line snippet).
- **80.11.b.c** — main.rs router spawn + per-goal `register` /
  `forget` on goal lifecycle hooks.
- **80.11.c** — Broadcast `to: "*"` with cap (linear in team
  size, marked expensive in tool description).
- **80.11.d** — Cross-machine inbox via NATS cluster (works
  automatically with NATS, documents the operator's broker
  config requirement).
- **80.11.e** — Bridge protocol responses (`shutdown_request` /
  `plan_approval_request` JSON shapes — niche, defer).
- **80.11.f** — main.rs tool registration wire.

## See also

- [Coordinator mode (Phase 84)](./coordinator-mode.md) —
  role-aware system prompt for `role: coordinator` bindings,
  `<task-notification>` envelope, `SendMessageToWorker`
  continuation tool, continue-vs-spawn matrix.
- [Worker mode (Phase 84.4)](./worker-mode.md) — sister persona
  for `role: worker` bindings.
- [Assistant mode](./assistant-mode.md)
- [Background agents (`agent run --bg` / ps / attach)](../cli/agent-bg.md)
- [Auto-approve dial](./auto-approve.md)
