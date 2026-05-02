# Agent event firehose (Phase 82.11)

Operator UIs (and any microapp with the right capability) need
real-time visibility into what agents are doing: chat lines, pause
state changes, escalations to humans, batch-job results, future
custom kinds. The *agent event firehose* is the single
architectural seam that delivers them.

The wire format — `AgentEventKind` — is a `#[non_exhaustive]`
discriminated enum on `nexo/notify/agent_event` ([admin RPC
reference](../microapps/admin-rpc.md#agent-events-firehose-phase-8211)).
This page documents the *runtime composition* that gets a frame
from a producer (transcript writer, processing handler,
escalation handler) onto every interested subscriber.

## Trait

Every producer holds a single `Arc<dyn AgentEventEmitter>`:

```rust
#[async_trait]
pub trait AgentEventEmitter: Send + Sync + Debug {
    async fn emit(&self, event: AgentEventKind);
}
```

Implementations are best-effort: failures log and drop. The
contract is that `emit` MUST NOT block the producer. Boot is free
to swap in any composition without touching emit sites.

Source: `crates/core/src/agent/agent_events.rs`.

## Implementations

### `BroadcastAgentEventEmitter` — live in-process

A `tokio::sync::broadcast::Sender<AgentEventKind>` with a 256-frame
ring buffer. Subscribers that lag past the buffer get
`RecvError::Lagged(n)` rather than panic — they are expected to
call `nexo/admin/agent_events/list` to resync.

Single-daemon installs run happily with just this. No durability,
no cross-host.

### `SqliteAgentEventLog` — durable backfill

Append-only log keyed by autoincrement `id`. Denormalised columns
(`kind`, `agent_id`, `tenant_id`, `at_ms`) so the common filter
axes hit indexed paths; full `AgentEventKind` round-trips as JSON
in `payload_json` so future enum variants land non-breaking.

Doubles as `AgentEventEmitter` so it slots into the composition
without a separate wiring path. `sweep_retention(retention_days,
max_rows)` mirrors the admin-audit sweep so a single boot
scheduler runs both.

Read API (`AgentEventLog::list_recent`) supports `agent_id` +
`kind` + `tenant_id` + `since_ms` + `limit` filters with
parameterised SQL.

Source: `crates/core/src/agent/admin_rpc/agent_events_sqlite.rs`.

### `NatsAgentEventEmitter` — multi-host bridge

Publishes serialised `AgentEventKind` frames to
`<prefix>.<agent_id>.<kind>` (default prefix
`nexo.agent_events`). Subscribers route per-agent
(`nexo.agent_events.ana.>`), per-kind
(`nexo.agent_events.*.processing_state_changed`), or both at the
broker.

The pure helper `agent_event_subject(prefix, &event)` exposes the
routing key without a live client — useful for boot-time
validation and for tests. `agent_id` is sanitised at emit-site
(`.`/`*`/`>`/whitespace → `_`) so a malformed config can't break
wildcard subscriptions.

Failure mode is best-effort: publish errors log and drop. The
broker crate's circuit breaker + disk queue protect the daemon
when NATS is unreachable.

Source: `crates/core/src/agent/agent_events.rs`
(`NatsAgentEventEmitter`, `agent_event_subject`).

### `TeeAgentEventEmitter` — fan-out

Composes several inner emitters into a single
`Arc<dyn AgentEventEmitter>`. Boot wires:

```text
Tee([
    BroadcastAgentEventEmitter,   // live JSON-RPC notifications
    SqliteAgentEventLog,          // durable backfill across restart
    NatsAgentEventEmitter,        // multi-host bridge for SaaS
])
```

Per-sink failures stay isolated by trait contract. Tee preserves
that guarantee — `emit` returns after every inner has been polled
sequentially. Production keeps each inner non-blocking (broadcast
`try_send`, NATS publish, async SQLite append) so a slow sink
cannot throttle the whole tee.

### `NoopAgentEventEmitter`

Default for headless installs and tests. Useful as an explicit
"no-op, by design" instead of `None` plumbed through every emit
site.

## Subscribe paths

Subscribers reach events through three doors:

| Door | When | How |
|------|------|-----|
| Live JSON-RPC notifications | Operator UI online during the emit | Microapp holds `transcripts_subscribe` / `agent_events_subscribe_all`; daemon delivers `nexo/notify/agent_event` frames automatically. |
| Backfill RPC | Operator UI starts after the emit | `nexo/admin/agent_events/list` reads from the `MergingAgentEventReader` — transcripts JSONL for `transcript_appended`, durable SQLite log for non-transcript kinds, merged by `at_ms` desc. |
| External NATS subscriber | Operator dashboard runs off-daemon | Subscribe directly at `<prefix>.<agent_id>.<kind>`. |

`MergingAgentEventReader` (in
`crates/core/src/agent/admin_rpc/domains/agent_events.rs`)
respects the `kind` filter:

- `kind=Some("transcript_appended")` → transcripts JSONL only.
- `kind=Some(other)` → durable log only.
- `kind=None` → both, merged by `at_ms` desc, capped at the
  caller's `limit`.

Boot wires the SQLite log as a Tee sink alongside the broadcast
emitter — meaning the log captures `TranscriptAppended` too. The
merger drops those on the log side so the JSONL reader stays
canonical for chat history; subscribers never see duplicates.

## Variants today

| Variant | Producer | Notes |
|---------|----------|-------|
| `TranscriptAppended` | `TranscriptWriter::append_entry` (Phase 82.11) | Body already-redacted at emit. |
| `PendingInboundsDropped` | inbound dispatcher under `processing/pause` (Phase 82.13.b.3) | Fired only on cap eviction. |
| `EscalationRequested` | (deferred) `escalate_to_human` built-in tool | Variant + emit shape pinned in 82.14.b.firehose. |
| `EscalationResolved` | `escalations::resolve` + `auto_resolve_on_pause` (Phase 82.14.b.firehose) | Same shape from both call sites so subscribers can't tell paths apart. |
| `ProcessingStateChanged` | `processing::pause` + `processing::resume` (Phase 82.13.b.firehose) | Carries `prev_state` + `new_state` so subscribers render correct deltas. Idempotent retries skip the emit. |

## Adding a new kind

`AgentEventKind` is `#[non_exhaustive]` with `#[serde(tag = "kind")]`,
so a new variant lands non-breaking in three steps:

1. Add the variant in `crates/tool-meta/src/admin/agent_events.rs`.
   Mirror the conventions: `agent_id` denormalised, optional
   `tenant_id` (skip-when-None on the wire), an `at_ms` field
   for ordering.
2. Wire the producer to call `emitter.emit(...)` from the place
   the event becomes true. Pre-fetch any "previous" state before
   the mutation so the wire frame carries both ends of the
   transition.
3. Extend `agent_events_sqlite::extract_metadata` and
   `agent_events::event_at_ms` so the durable log + the merger
   know how to project the new variant. Unknown future variants
   fall through to a warn-skip on the durable side and the live
   broadcast still surfaces them — failure stays graceful.

No FTS schema change is required: search remains
`TranscriptAppended`-only today. Future revs that want full-text
search over non-transcript kinds add an
`AgentEventLog::search_events` method without touching existing
emit sites.
