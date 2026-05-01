# Building a multi-tenant SaaS microapp (Phase 82 walkthrough)

This page connects the dots across Phase 82's primitives so a
microapp author can ship a multi-tenant SaaS extension without
re-deriving the architecture from each sub-phase doc. Every
section maps directly to a primitive that's already built; the
work is wiring them together for your specific shape.

## What you get from Phase 82

| Primitive | Doc |
|---|---|
| `BindingContext` propagation (per-call agent + binding identity) | [`agents.md`](../config/agents.md) |
| Webhook receiver (single HTTP entry, YAML-routed to NATS) | [`ops/webhook-receiver.md`](../ops/webhook-receiver.md) |
| Outbound dispatch from extension (`nexo/dispatch`) | [`extensions/stdio.md`](./stdio.md) |
| NATS event subject → agent turn binding | [`config/agents.md`](../config/agents.md) |
| Per-binding tool rate-limit | [`ops/per-binding-rate-limits.md`](../ops/per-binding-rate-limits.md) |
| Per-extension state directory | [`extensions/state-management.md`](./state-management.md) |
| Multi-tenant audit log filter (Phase 82.8) | inline below |
| Admin RPC (CRUD agents/credentials/pairing/llm/channels) | [`microapps/admin-rpc.md`](../microapps/admin-rpc.md) |
| Agent events firehose | [`microapps/admin-rpc.md`](../microapps/admin-rpc.md#agent-events-firehose-phase-8211) |
| HTTP server capability | [`microapps/admin-rpc.md`](../microapps/admin-rpc.md#http-server-capability-phase-8212) |
| Operator chat takeover | [`microapps/admin-rpc.md`](../microapps/admin-rpc.md#operator-processing-pause--intervention-phase-8213) |
| Agent escalation | [`microapps/admin-rpc.md`](../microapps/admin-rpc.md#agent-escalations-phase-8214) |

## Reference scaffold

`agent-creator` is the reference SaaS-shaped microapp (out-of-tree
repo: see your operator's microapp registry for the URL). It uses
every primitive in this list and is the recommended starting
point for clone-and-adapt. The rest of this page assumes you've
checked it out alongside the daemon source.

## Tenant onboarding flow

1. Operator creates a row in your microapp's `tenants` table
   (see `migrations/0001_tenants.sql`). Each tenant carries an
   `account_id: TEXT PRIMARY KEY` that becomes the cross-cutting
   identifier through:
   - `BindingContext.account_id` on every inbound + tool call
   - `goal_turns.account_id` for audit isolation (Phase 82.8)
   - `ProcessingScope::Conversation { account_id, … }` for
     pause/resume (Phase 82.13)
   - `EscalationEntry { agent_id, scope, … }` where `scope`
     carries the `account_id` (Phase 82.14)
2. The microapp creates per-tenant artifacts under
   `state_dir_for(extension_id)/tenants/<account_id>/`:
   ```text
   ~/.nexo/extensions/agent-creator/state/tenants/acme/
     ├── leads.sqlite
     ├── opt_outs.sqlite
     └── credentials.json    # encrypted at rest
   ```
3. Operator binds the tenant to a channel via
   `nexo/admin/credentials/register` (Phase 82.10.d) — the same
   bearer token gets both the channel's outbound write capability
   AND the per-tenant audit scope.

## Channel binding

`agents.yaml.<id>.inbound_bindings` lists which channels the
agent answers. Each binding inherits the tenant's `account_id`
via the channel plugin's inbound shape (Phase 82.5
`InboundMessageMeta`). Provider plugins (`whatsapp`, `telegram`,
`email`, `slack-mcp`) are responsible for stamping `account_id`
onto the inbound — this is what threads tenancy through to
the audit log + rate-limit buckets + escalation scopes.

## Credential vault pattern

Credentials are filesystem-backed (Phase 82.10.h.3
`FilesystemCredentialStore`):

```text
secrets/<channel>/<instance>/payload.json
```

For multi-tenant, use `<instance> = <account_id>` so the
operator UI can rotate one tenant's bearer without touching
others. The Phase 82.12 `token_hash` helper lets the daemon
notify a microapp of rotation without putting the cleartext
old token on the wire.

## Drip scheduler (or whatever cron-like flow you need)

Phase 82.4 + 82.4.b ships the NATS event subscriber runtime —
extensions subscribe to a NATS subject and the daemon binds
each event to an agent turn. For a per-tenant drip:

1. Microapp publishes `marketing.drip.fire.<account_id>` on
   NATS at the cron tick.
2. `agents.yaml.<agent_id>.event_subscribers` includes
   `marketing.drip.fire.*` (glob).
3. Per-binding rate-limit (Phase 82.7,
   `tool_rate_limits.<binding_id>.send_drip = 10/min`) caps
   the per-tenant outbound velocity so a runaway tenant
   doesn't starve the others.

## Compliance hooks

- **Redactor** (Phase 10.4) runs inside
  `TranscriptWriter::append_entry` BEFORE persistence. Body
  bytes that hit disk are already redacted; the firehose
  emits the same redacted body. Microapps don't have to
  implement their own redaction — operator config in
  `transcripts.yaml` is the single point of control.
- **Audit retention** (Phase 82.10.h.1) — operators set
  `NEXO_MICROAPP_ADMIN_AUDIT_RETENTION_DAYS` /
  `NEXO_MICROAPP_ADMIN_AUDIT_MAX_ROWS`. Boot sweep enforces
  both.
- **Operator takeover** (Phase 82.13) — pause a single
  conversation with `nexo/admin/processing/pause`; agent
  goes silent while operator types a manual reply via
  `nexo/admin/processing/intervention`. Compliance teams
  use this for high-risk tenants.

## Audit queries

For per-tenant billing / support, query the audit log scoped
to one tenant:

```rust
use nexo_agent_registry::SqliteTurnLogStore;
use chrono::{Duration, Utc};

let rows = store
    .tail_for_account("acme", Utc::now() - Duration::days(30), 500)
    .await?;
```

The store filters strictly by `account_id` and excludes legacy
`NULL` rows. Cross-tenant probes return an empty list (not an
error) — defense in depth against existence oracles. Operator-
scoped tools (`tail`, `tail_since`) keep returning every row
including legacy NULL.

For admin RPC audit (Phase 82.10.h SQLite writer):

```bash
nexo microapp admin audit tail \
    --microapp-id agent-creator \
    --since-mins 60 \
    --format json | jq '.[] | select(.method | startswith("nexo/admin/agents/"))'
```

## Live event firehose

Microapps that need a real-time UI (chat, dashboard) hold the
`transcripts_subscribe` capability and receive
`nexo/notify/agent_event` notifications on their stdio. The
boot subscriber loop (Phase 82.11) handles fan-out, lag
recovery, and per-microapp filtering — the microapp just
reads JSON-RPC frames as they arrive. See
[`microapps/admin-rpc.md`](../microapps/admin-rpc.md#agent-events-firehose-phase-8211)
for the wire shape.

## Going to production

1. Ship the microapp binary alongside its `plugin.toml`.
2. Operator drops it into `extensions/<id>/` and runs
   `nexo ext install <path>`.
3. Operator grants capabilities in
   `extensions.yaml.entries.<id>.capabilities_grant`. Common
   shape for a multi-tenant chat SaaS:
   ```yaml
   extensions:
     entries:
       agent-creator:
         capabilities_grant:
           - agents_crud
           - credentials_crud
           - pairing_initiate
           - llm_keys_crud
           - transcripts_read
           - transcripts_subscribe
           - operator_intervention
           - escalations_read
           - escalations_resolve
   ```
4. Operator runs `nexo doctor capabilities` to confirm every
   INVENTORY toggle is on.
5. Boot — the daemon validates the grants, spawns the
   microapp, threads the admin RPC dispatcher into the
   extension's stdio, and starts the firehose subscribe
   tasks for every microapp that holds the capability.

## What's NOT in v0

These are framework-supported but not wired in main.rs yet
(see [`FOLLOWUPS.md`](https://github.com/lordmacu/nexo-rs/blob/main/proyecto/FOLLOWUPS.md)
under the 82.x sections):

- Pairing notifier wire — microapps poll `pairing/status`
  instead of receiving live `pairing_status_changed` frames.
- `EventForwarder` thread `account_id` from `BindingContext`
  on live writes (audit reader is correct; the writer always
  emits `None` today).
- `escalate_to_human` built-in tool registration in
  ToolRegistry — microapps that want escalations today have
  to call the admin RPC directly.
- `processing_state_changed` / `escalation_requested` /
  `escalation_resolved` event variants on the firehose.

All of these are framework-level deferreds, not microapp-level
work. They land in the same boot-order refactor that's
tracked across the FOLLOWUPS entries.
