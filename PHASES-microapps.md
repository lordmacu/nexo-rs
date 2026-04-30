# PHASES — Microapp Framework

Planning track separated from `proyecto/PHASES.md` to keep the
microapp framework work concentrated in one document. This file
owns Phase 82 (multi-tenant SaaS extension enablement +
control plane) and Phase 83 (microapp framework foundation).
The legacy `PHASES.md` carries a short pointer back to this
file.

## Workflow rule

When `/forge brainstorm | spec | plan | ejecutar` is invoked
on any topic prefixed with `82.x` or `83.x` (or any sub-phase
defined here), this file is the **source of truth**. Read it
before consulting the legacy `PHASES.md`. The legacy file
covers Phases 1-81; everything microapp-related lives here.

## Status

| Phase | Name | Sub-phases | Status |
|-------|------|-----------|--------|
| 82 | Multi-tenant SaaS extension enablement + control plane | 14 | 0/14 |
| 83 | Microapp framework foundation | 14 | 0/14 |

## Why a separate file

`PHASES.md` is ~9 K lines of legacy + roadmap mixed. The
microapp framework is a focused effort that benefits from a
dedicated document:

- Faster context: `/forge` reads only the relevant content.
- Cleaner PR review: changes here don't touch the giant
  legacy file.
- Easier to share with a future contributor working purely
  on the microapp ecosystem.
- Future-friendly: when the framework expands into
  Phase 84 (e.g., marketplace, multi-host, etc.), it lands
  here too without polluting `PHASES.md`.

## Repo topology

The microapp framework lives in this monorepo (`nexo-rs/`).
Reference microapps that exercise the framework also live here
(under `extensions/`). **Operator-owned microapps that are
products** live in **separate git repositories** with their own
release cadence and history.

**In-tree (this repo)**:

- `crates/core/`, `crates/microapp-sdk-rust/`,
  `crates/microapp-sdk-react/`, `crates/compliance-primitives/`,
  `crates/webhook-receiver/` — framework + reusable libraries
- `extensions/template-microapp-{rust,python,typescript,rust-with-ui}/`
  — reusable scaffolds (Phase 83.7, 83.12, 83.13)
- `extensions/template-saas/` — SaaS reference (Phase 82.9)
- `extensions/ventas-etb/` — sales reference microapp
  (Phase 83.8). Lives in-tree because it ports the
  pre-existing `ana` agent owned by this project.

**Out-of-tree (operator-owned repos)**:

- `agent-creator-microapp/` (Phase 83.10 validation case) —
  meta-microapp with React UI for managing agents, channels,
  LLM keys, conversations. Iterates independently of nexo
  releases.
- Future operator microapps: same pattern. Each gets its own
  git repo, its own release cadence, its own customers /
  internal users.

**Dependency strategy** (see "Development methodology" below
for the migration timeline):

- **Target (post Phase 83.14)**: `agent-creator/Cargo.toml`
  declares `nexo-microapp-sdk-rust = "^0.2"` consuming the
  published crate from crates.io. Frontend declares
  `"@nexo/microapp-ui-react": "^0.1"` from npm. **No
  filesystem coupling between repos.**
- **Bridge (during Phase 82+83 active development)**: path
  dependency `nexo-microapp-sdk-rust = { path = "../nexo-rs/crates/microapp-sdk-rust" }`
  is acceptable because the SDK API churns rapidly. Same for
  the React lib via `"file:../nexo-rs/crates/microapp-sdk-react"`.

**Change-routing rule**:

- Bug or feature in **framework primitives, SDK libraries, or
  reference microapps (template-*, ventas-etb)** → this repo
  (`nexo-rs/`).
- Bug or feature in an **operator-owned microapp**
  (agent-creator and future siblings) → that microapp's own
  repo. Bumps the SDK dependency only when the framework drops
  a new release that fixes / adds something it needs.

## Development methodology

The microapp framework is built with a **discovery-driven core
development** loop, not a waterfall. Phase 82 + 83 sub-phases
listed in this file are the **starting point**, not the
ceiling — gaps surface as we use the framework on real
microapps and feed back into new sub-phases.

The expected flow:

```
agent-creator dev (~/projects/agent-creator-microapp/)
   ↓ (encounters a gap: missing primitive, friction, or pattern
      that should be reusable across microapps)
   ↓
back to nexo-rs/ → extend the framework primitive, add a new
                   sub-phase if needed, ship the change
   ↓ (commit lands in nexo-rs/)
   ↓
back to agent-creator/ → consume the new primitive, validate it
                          against the real use case
   ↓ (commit lands in agent-creator/)
   ↓
... loop continues until the microapp is production-ready and
    the framework has absorbed the lessons learned ...
```

**Each repo trackea its own git history**. Cross-repo coupling
exists only at the dependency edge (Cargo.toml + package.json),
not at the source-control edge.

**Sub-phase counts are NOT frozen**. Concrete evidence: this file
already grew from 9 sub-phases (Phase 82) at the initial draft
to 14 after a few hours of design conversation, because real
use cases (meta-microapp UI, operator takeover, agent escalation,
WhatsApp-Web-inspired components) surfaced gaps that justified
new sub-phases. Expect the same dynamic during ejecutar — new
sub-phases get appended as the agent-creator microapp matures.

**Order of execution is NOT strictly linear**. The roadmap below
suggests a critical path, but during ejecutar the work may
bounce between primitives (nexo-rs) and microapp (agent-creator)
as gaps emerge. This is intentional — building both in parallel
is what catches design mistakes early.

**Migration trigger from path-dep to published crates**: once
83.4 (microapp-sdk-rust) and 83.13 (microapp-ui-react) ship a
stable 0.1.0, run 83.14 to publish them and migrate
agent-creator from path/file dependencies to versioned published
ones. This is the official transition from "framework in
gestation" to "framework consumable by third parties".

## Cross-cutting guarantees

Across every Phase 82 + 83 sub-phase:

1. **Backward compatibility preserved**: extensions written
   before this work keep functioning. Adding fields to
   payloads is additive — extensions ignoring unknown
   fields keep going. Adding new JSON-RPC methods does not
   break extensions that don't call them.
2. **Provider-agnostic**: zero LLM-vendor assumption leaks
   into BindingContext, ExtensionConfig, HookDecision,
   admin payloads, or audit columns.
3. **Defense-in-depth**: capability gates required for
   privileged operations; core remains authoritative even
   when a subprocess votes; redactor runs before transcript
   exposure.
4. **Phase 11 strict**: NO Phase 81 in-process plugin work
   pulled in. Microapps remain subprocess (stdio JSON-RPC
   or NATS-transport).
5. **Cross-app universality**: every primitive is validated
   against at least 4 distinct microapp shapes
   (conversational, batch, event-driven, heartbeat-driven)
   before merge.
6. **Use-case agnostic by construction**: core primitives in
   82.x and SDK libraries in 83.x must support multiple agent
   shapes — chat, batch, event-driven, heartbeat, image
   generation, voice, custom — not just chat. Concrete rules
   that flow from this guarantee:
   - **Schemas use enum-typed scopes / kinds** that are
     extensible additively. New variants added later are
     non-breaking. Example: `ProcessingScope` (82.13) starts
     with `Conversation { ... }` for chat and grows
     `BatchQueue { ... }` / `EventStream { ... }` / `Custom
     { ... }` as new microapp shapes need them.
   - **Naming avoids chat-coupled terminology** in core types
     where a more general term works: `processing scope` not
     `conversation`, `intervention action` not `operator
     reply`, `agent event` not `transcript`, `summary` not
     `question`. Chat semantics live at the channel / binding
     layer, not at the framework primitive layer.
   - **Chat-specific helpers are OPT-IN extensions**, not part
     of the core contract. Example: 83.13 (WhatsApp-Web React
     component library) is explicitly opt-in. A microapp that
     processes batch jobs or webhook events imports the
     framework SDK without ever touching 83.13.
   - **A non-chat microapp must compile and ship** using only
     82.x + 83.x core primitives, without ever importing a
     chat-coupled helper. This is the litmus test for whether
     a primitive belongs in core (yes) or in a chat-specific
     opt-in module (then it belongs in 83.13 or a sibling
     opt-in lib, not in core).

---

### Phase 82 — Multi-tenant SaaS extension enablement + control plane   ⬜

Phase 82 closes the gaps in core nexo that prevent a Phase 11
extension (subprocess + `plugin.toml`) from implementing a
robust multi-tenant SaaS application without modifying the
core or breaking tenant isolation. Driving use case: a marketing
platform that hosts N customer accounts, each with M channels
(WhatsApp / Telegram / Email / Web form), where every channel
can resolve a different LLM API key per tenant. The same gaps
apply to any SaaS-shaped extension (sales agent, support
deflection, lead-qualification, inbound triage).

Phase 82 also covers the **control plane primitives** that any
microapp with its own UI / admin surface needs: bidirectional
admin RPC, transcripts firehose, and HTTP server hosting
convention. These are universal cross-app primitives, not
SaaS-specific.

Extensions in scope are stdio JSON-RPC and NATS-transport plugins
declared via `plugin.toml`. Extensions own their tenant table,
credential vault, scheduled flows (drips, reminders, win-back),
compliance state (opt-out, throttling, GDPR consent), lead
scoring, segments, and attribution. Core nexo owns the inbound
channel surface, the agent runtime, the webhook receiver, the
broker, and the audit log — and must expose enough binding
context to the extension that multi-tenant routing is possible
without compromising isolation.

Goal: an operator can install a SaaS-shaped extension and run
an N-tenant production system on a single nexo instance, with
billing-grade audit per tenant, fair-use rate limits per binding,
proactive outbound (drips at T-1h), and a self-hosted React UI
served by the microapp without writing any new core code.

Scope boundary: Phase 82 is the **smallest** core change that
unblocks Phase 11 SaaS use plus self-hosted-UI use. Anything
that mechanically requires in-process plugin access (advisors
mid-turn, custom channel adapters, persona-as-plugin) stays in
Phase 81 backlog.

#### 82.1 — Tool call enriched with `BindingContext`   ⬜

Hard blocker. Today when a channel delivers an inbound message
and the agent fires a tool call, the extension does not know
which `(channel, account_id)` binding the message came from.
Phase 17 has the resolver internally; the data is just never
propagated to the tool call payload. Without this, an extension
cannot route a tool call to the correct tenant.

Scope:
- New struct `BindingContext { channel, account_id, agent_id,
  session_id, binding_id }` in `nexo-core::agent::context`.
- `ToolCallRequest` (or equivalent dispatch payload) gains
  `binding_context: Option<BindingContext>`.
- Phase 17 resolver fills `BindingContext` during dispatch.
- `crates/extensions/src/runtime/wire.rs` includes
  `binding_context` as `params._nexo_context` in JSON-RPC
  `tools/call` so existing extensions ignore it but new
  extensions read it.
- Reuse the existing `inject_context_meta` helper in
  `extension_tool.rs` and the parallel `McpTool::call` path
  in `mcp_tool.rs` — extend both with the new fields under
  `_meta.nexo.binding`. Dual-write `_meta.agent_id` legacy
  for one release of grace.
- Reference manifest in `extensions/template-saas/` (delivered
  in 82.9) demonstrates reading the context.

Done criteria:
- `BindingContext` round-trips from inbound channel through
  Phase 17 resolver to JSON-RPC tool call payload.
- Existing extensions continue to function without change.
- 9+ unit tests + 1 integration test covering: fill from
  inbound binding, fill from delegation tool call, missing
  binding (None), serialization shape stability, all four
  binding types (WA/TG/Email/web), Phase 17 isolation.
- Docs page on extension contract documents the new field.

#### 82.2 — Webhook receiver wire-up (= existing 80.12.b)   ⬜

Hard blocker. Phase 80.12 shipped `WebhookHandler::handle` +
HMAC-SHA256/SHA1/RawToken signature verification + event-kind
extraction + subject rendering. The actual HTTP listener is
deferred. Without it, no Shopify/Calendly/Zendesk/Stripe
trigger reaches nexo.

Scope:
- Mount an axum/hyper route on the existing health-server port
  or a separate `webhook_receiver.bind` YAML field.
- Body cap, signature verify, event-kind extraction, subject
  rendering all reuse Phase 80.12 pure-fn helpers.
- Successful webhook → publish to NATS
  `webhook.<source_id>.<event_kind>`.
- Boot wire-up in `src/main.rs` gated by
  `webhook_receiver.enabled` YAML.
- Per-source rate limit (defense against runaway sender).

Done criteria:
- HTTP POST to mounted route with valid HMAC reaches NATS.
- Invalid signature → 401, no NATS publish.
- Body over cap → 413, no NATS publish.
- Subject pattern collision detection at boot.
- Integration test using axum test-client + signed payload.
- `webhook_receiver` YAML schema documented.

#### 82.3 — Outbound dispatch from extension (stdio path)   ⬜

For proactive sends — drips at T-1h, T-24h reminders, win-back
campaigns — the extension dispatches a message *without* any
agent turn happening. Today Phase 11 stdio extensions have no
channel for "tell nexo to send this text via WA to lead X".

Scope:
- New JSON-RPC method `nexo/dispatch` extension can call.
  Params: `{ channel, account_id, to, body, msg_kind:
  text|template|media, attachments?, reply_to_msg_id? }`.
- Core handler validates that `(channel, account_id)` is in
  the extension's declared bindings (per `plugin.toml`)
  before publishing to `plugin.outbound.{channel}`.
- Phase 26 reply adapter handles delivery as if the agent had
  produced the message.
- NATS-transport extensions can already publish directly to
  `plugin.outbound.{channel}`; this 82.3 work is specifically
  for stdio symmetry.

Done criteria:
- `nexo/dispatch` happy path delivers via WA / TG / Email.
- Unauthorized `(channel, account_id)` → JSON-RPC error
  -32000 with `unauthorized_binding`.
- Channel adapter not configured → error -32000 with
  `channel_unavailable`.
- Rate limit per extension respected.
- 5+ unit tests + 1 integration test using a mock channel
  adapter.

#### 82.4 — NATS event subject → agent turn binding   ⬜

Webhook events (82.2) land on NATS subjects but cannot today
trigger an agent turn directly. Pollers (Phase 19/20) can be
adapted but the wiring is repetitive.

Scope:
- New binding type `event_subscriber: { subject_pattern,
  agent_id, synthesize_inbound: true | false }` in
  `nexo-config::types::bindings`.
- When NATS event matches `subject_pattern`, the binding
  synthesizes an inbound message (payload as JSON text) and
  fires an agent turn.
- The agent receives the event in its turn context and decides
  what tool to call (e.g., `marketing_recover_cart`).
- `BindingContext` (82.1) reflects the event source so the
  extension knows the trigger origin.

Done criteria:
- Subject filter (`webhook.shopify.>`) matches and only matches
  the right events.
- Agent turn fires with synthesised inbound; payload is
  preserved verbatim under `inbound_msg_ref.payload_json`.
- Backpressure: if agent is busy, events queue with bounded
  buffer; overflow logged.
- 4+ unit tests + 1 integration test.

#### 82.5 — Inbound message metadata in `BindingContext`   ⬜

Tools need access to the original message metadata: `msg_id`
for marking-read, `channel_msg_id` for reply-to/quote, and
`attachment_urls` for media handling. Today the agent prompt
sees the text but tool args don't auto-include metadata.

Scope:
- Extend `BindingContext` (82.1) with `inbound_msg_ref:
  Option<InboundMsgRef>`.
- `InboundMsgRef { msg_id, channel_msg_id, sender_id,
  received_at, attachment_urls: Vec<String>,
  payload_json: Option<Value> }`.
- Phase 26 reply adapters read `channel_msg_id` from outbound
  ref to do native quote/reply where the channel supports
  it.
- Attachment URLs are signed/short-lived per Phase 6.5 media
  rules.

Done criteria:
- All four channel adapters (WA/TG/Email/web) populate
  `inbound_msg_ref` correctly.
- Quote/reply preserved in WA outbound when `reply_to_msg_id`
  matches an `inbound_msg_ref.channel_msg_id`.
- 5+ unit tests covering each channel + attachment round-trip
  + JSON payload preservation for `event_subscriber` (82.4)
  bindings.

#### 82.6 — Per-extension `state_root` convention + CLI   ⬜

Extensions need a stable directory to put their SQLite
databases, vault files, and per-tenant artifacts. The
convention exists informally; this subphase formalizes it.

Scope:
- Canonical state dir: `<XDG_STATE_HOME>/nexo/extensions/<id>/`
  (default `~/.local/share/nexo/extensions/<id>/state/`).
- New CLI `nexo extension state-dir <id>` prints the absolute
  path.
- `PluginInitContext`-equivalent for stdio extensions exposes
  the state dir via JSON-RPC `initialize` response (server
  reads `agent_state_root` from initialize params).
- Docs: SQLite usage example, backup procedure (cp + sqlite3
  `.backup`), isolation guarantees.

Done criteria:
- `nexo extension state-dir marketing` prints the right path.
- State dir is created idempotently at extension first start.
- 3+ unit tests + 1 integration test.
- Docs page in `docs/src/extensions/state-management.md`.

#### 82.7 — Per-binding tool rate-limit (multi-tenant fair-use)   ⬜

SaaS plans (free / pro / enterprise) need per-binding fair-use.
A free-tier tenant cannot fire `marketing_send_drip` at the
same rate as an enterprise tenant. Phase 16 has per-binding
overrides; this subphase extends them to per-tool rates.

Scope:
- `EffectiveBindingPolicy.tool_rate_limits: BTreeMap<String,
  ToolRateLimitConfig>`.
- Existing `ToolRateLimiter` (Phase 80.x) honours the per-
  binding override before falling back to global policy.
- Extension manifest `[[plugin.tools]]` can declare a default
  rate that operator overrides per-binding via `agents.yaml`.
- Telemetry: per-binding rate-limit hits logged to Phase 72
  audit log.

Done criteria:
- Free-tier binding hits 10/min cap on `marketing_send_drip`,
  pro tier hits 100/min, enterprise unlimited.
- Cap respected across multiple agent goals on the same
  binding.
- 5+ unit tests + 1 integration test using a fake clock.
- `agents.yaml` example documents the override shape.

#### 82.8 — Multi-tenant audit log filter   ⬜

Phase 72 turn-level audit log already records every tool call.
Tenants need to query *only their own* turns for billing or
support — a tenant must never see another tenant's audit row.

Scope:
- Add `account_id TEXT` column to `goal_turns` table
  (idempotent schema migration via Phase 77.17 system).
- `EventForwarder` populates `account_id` from
  `BindingContext` (82.1) on every `AttemptResult` write.
- `agent_turns_tail` tool gains optional `account_id: String`
  filter; absence returns rows scoped to the caller's
  binding account by default (defense-in-depth).
- Index on `(account_id, started_at DESC)` for tenant-scoped
  tail queries.

Done criteria:
- Existing audit rows backfill `account_id = NULL` (legacy)
  and are visible only to operator/admin queries.
- New rows always carry `account_id` when binding context is
  present.
- Tenant-scoped tail returns only matching rows; cross-tenant
  attempt → empty result, never error (avoid existence
  oracle).
- 4+ unit tests + 1 integration test verifying isolation.

#### 82.9 — Reference template + docs   ⬜

A new SaaS-shaped extension template + docs page so operators
can clone-and-adapt the marketing scaffold without reverse-
engineering 82.1–82.8.

Scope:
- `extensions/template-saas/` with: `plugin.toml` (declares
  multiple tools, MCP servers for HubSpot/Calendly/PostgreSQL,
  webhook source declarations), `Cargo.toml`, `src/main.rs`
  with JSON-RPC loop reading `BindingContext`, `migrations/`
  for tenants/leads/credentials/opt_outs/throttling,
  `src/tenants.rs` with isolation guard, `src/dispatcher.rs`
  using `nexo/dispatch` (82.3), `src/scheduler.rs` for drips,
  `README.md` + `config.example.yaml`.
- New docs page `docs/src/extensions/multi-tenant-saas.md`
  with end-to-end walkthrough: tenant onboarding flow, channel
  binding, credential vault pattern, drip scheduler,
  compliance hooks, audit queries.
- `admin-ui/PHASES.md` tech-debt entries: webhook receiver
  panel, per-binding rate limit panel, per-tenant audit
  filter, BindingContext-aware tool inspector.
- `crates/setup/src/capabilities.rs::INVENTORY` registers
  webhook receiver bind env + any new env toggles introduced
  by 82.x.

Done criteria:
- `cargo build -p template-saas` passes.
- `nexo extension install ./template-saas` smoke-tests
  end-to-end (initialize → tools/list → simulated tool call
  with BindingContext → simulated nexo/dispatch → simulated
  webhook event).
- Docs renders without broken links (`mdbook build docs`).
- admin-ui PHASES.md gains the four tech-debt entries.

#### 82.10 — `nexo/admin/*` bidirectional RPC + granular capability gates + admin domain methods   ⬜

Cross-app primitive. Today Phase 11 JSON-RPC is mostly
one-way: core → microapp via `tools/call`. Microapps that
author agents, manage credentials, initiate pairing, or rotate
LLM keys need to invoke admin operations on the core. Without
this, any microapp with its own UI (meta-microapp, settings
dashboard, monitoring tool) is blocked from CRUD on platform
state.

Scope:
- Bidirectional JSON-RPC: microapps invoke
  `nexo/admin/<domain>/<method>` on core.
- Granular capability gates declared in `plugin.toml`:
  ```toml
  [capabilities]
  admin = ["agents_crud", "credentials_crud",
           "pairing_initiate", "llm_keys_crud"]
  ```
- Admin domain methods:
  - `nexo/admin/agents/{list, get, upsert, delete}` — wraps
    `crates/setup/src/yaml_patch.rs` (Phase 69) helpers,
    triggers Phase 18 hot-reload after mutation
  - `nexo/admin/credentials/{list, register, revoke}` —
    wraps Phase 17 stores per channel. **Many-to-many by
    design**: `register` accepts `agent_ids: Vec<String>`
    (1 channel instance can serve N agents — e.g., a shared
    WhatsApp business line answered by both `ana` and
    `carlos`); the agent's `inbound_bindings` accepts a list
    of `{plugin, instance}` tuples (1 agent can answer N
    channel instances — e.g., `ana` on `whatsapp:personal` +
    `whatsapp:business` + `whatsapp:ventas`). Operator can
    edit the assignment in either direction via
    `credentials/register` (rebind from credential side) or
    `agents/upsert` (rebind from agent side). Framework is
    channel-agnostic; v1 meta-microapp UI scope is
    **WhatsApp-only** — Telegram / email / other channels
    are framework-supported but out of UI scope for v1.
  - `nexo/admin/pairing/{start, status, cancel}` — wraps
    Phase 26 pairing CLI logic; emits
    `nexo/notify/pairing_status_changed { challenge_id,
    state }` for live status streaming
  - `nexo/admin/llm_providers/{list, upsert, delete}` —
    manages provider registry; keys remain `${ENV_VAR}`
    references (operator owns the secret)
  - `nexo/admin/reload` — explicit manual hot-reload trigger
- Operator opt-in via
  `extensions.yaml.<id>.capabilities_grant: [agents_crud, ...]`.
- Audit log per call: new `microapp_admin_audit` SQLite
  table with `(microapp_id, method, args_hash, started_at,
  result, duration_ms)` columns, capped tail like
  `goal_turns`.
- INVENTORY env toggles per capability:
  `NEXO_MICROAPP_ADMIN_AGENTS_ENABLED`, etc.

Done criteria:
- Microapp with `agents_crud` can list/upsert/delete agents
  via JSON-RPC.
- Missing capability → JSON-RPC error -32004
  `capability_not_granted`.
- Audit log row per admin call with operator-visible tail
  (`nexo microapp admin audit tail [microapp_id]`).
- 12+ unit tests + 1 integration test using a mock microapp
  invoking each domain method.
- Capability declaration in `plugin.toml` validated at boot;
  declared but not granted → boot warn.

#### 82.11 — Agent event firehose + admin RPC (transcript-shaped events as one variant)   ⬜

Cross-app primitive. Microapps need a programmatic surface to
**stream and query agent activity** — but agent activity is
NOT only transcripts. Chat agents produce transcript events;
batch agents produce job-completion events; image generators
produce output-produced events; webhook routers produce
event-processed records. The firehose treats all of these as
discriminated variants of the same `AgentEventKind` enum and
lets microapps subscribe to the kinds they care about.

Use-case agnostic by construction (Cross-cutting #6). Chat is
ONE variant — `TranscriptAppended` — among N. Today only that
variant is emitted, but the schema is enum-typed so future
variants are non-breaking additive changes.

Used by any microapp consuming agent activity: meta-microapp
chat viewer, analytics-microapp funnel, audit / compliance
microapp, training-data-extractor, batch-job dashboard,
image-gen output gallery, webhook event log.

Scope:
- Discriminated event kind enum:
  ```rust
  #[serde(tag = "kind")]
  enum AgentEventKind {
      TranscriptAppended {
          agent_id, session_id, sender_jid: Option<String>,
          role, body, sent_at, transcript_offset,
      },
      BatchJobCompleted {
          agent_id, job_id, status, output_summary, finished_at,
      },
      BatchJobFailed {
          agent_id, job_id, error_summary, failed_at,
      },
      EventProcessed {
          agent_id, subject, event_summary, processed_at,
      },
      OutputProduced {
          agent_id, output_kind, summary, produced_at,
          output_ref: Option<String>,
      },
      Custom {
          agent_id, kind: String, payload: Value,
      },
  }
  ```
- Live firehose via JSON-RPC notification (no `id`, no
  response):
  - `nexo/notify/agent_event { kind: "TranscriptAppended" |
    "BatchJobCompleted" | ..., ...event-specific fields }`
  - Emitted on every agent event AFTER the redactor (Phase
    10.4) runs for variants that carry PII — defense-in-
    depth.
  - V0 emits ONLY `TranscriptAppended` (chat). Other variants
    are reserved enum slots; emitted when batch / event-
    driven / image-gen agents land in future work.
- Admin RPC for backfill (idempotent), all parameterised by
  optional `kind?` filter:
  - `nexo/admin/agent_events/list { agent_id, kind?, scope?,
    since?, limit? }` — `scope` is loosely typed
    (`session_id` for transcripts, `job_id` for batch,
    `subject` for events).
  - `nexo/admin/agent_events/read { agent_id, scope, kind?,
    since?, limit? }` — fetch sequence of events for one
    scope.
  - `nexo/admin/agent_events/search { agent_id, query, kind?,
    limit? }` — full-text via Phase 10.7 FTS5 (transcripts
    today; future kinds may add their own indexes).
- Backfill model: progressive — default `since = now - 30d`,
  `max_per_scope = 500`. Microapps needing more invoke `read`
  with explicit `since`.
- Capability gates (granular per kind):
  - `transcripts_subscribe` — receive only `TranscriptAppended`
    notifications (chat microapps). **Default for backward
    compat with the original 82.11 design** — a microapp that
    declared this capability before generalisation keeps
    receiving only chat events.
  - `transcripts_read` — invoke admin RPC for transcript
    queries.
  - `batch_events_subscribe` — receive only batch-related
    kinds (future, when batch agents land).
  - `output_events_subscribe` — receive only `OutputProduced`
    (image-gen / voice / file-producing agents).
  - `agent_events_subscribe_all` — receive every kind. Strong
    capability, used by audit / compliance microapps that
    need full visibility.
- Audit log: `microapp_admin_audit` row per RPC pull and per
  notification subscription — counts of events streamed per
  kind per microapp.
- INVENTORY toggle: `NEXO_MICROAPP_AGENT_EVENTS_ENABLED`
  (replaces the originally proposed
  `NEXO_MICROAPP_TRANSCRIPTS_ENABLED`).

Done criteria:
- Microapp with `transcripts_subscribe` receives
  `nexo/notify/agent_event { kind: "TranscriptAppended", ...
  }` for each new transcript append (already redacted).
- Microapp with `agent_events_subscribe_all` receives every
  emitted variant.
- Without any subscribe capability → error -32004.
- Defense-in-depth: redactor (Phase 10.4) runs before
  emission for all PII-carrying variants. Raw PII never
  exposed.
- Schema is forward-compatible: adding a new
  `AgentEventKind` variant is non-breaking for microapps
  using strict-mode JSON parsers (the discriminator is the
  `kind` field; unknown kinds are filterable).
- 8+ unit tests + 1 integration test (mock microapp consumes
  firehose, verifies redaction passthrough, verifies kind
  filter).

#### 82.12 — Plugin.toml `[capabilities.http_server]` + bind 127.0.0.1 + token auth + boot supervisor   ⬜

Cross-app primitive. Microapps with their own React UI
(meta-microapp, dashboard-microapp, settings-microapp) need
to expose an HTTP server on a known port. Operator points
browser at it. Today there's no convention; microapps would
have to roll their own bind / auth / health.

Scope:
- Plugin.toml declaration:
  ```toml
  [capabilities.http_server]
  port = 9001
  bind = "127.0.0.1"             # default — loopback only
  token_env = "AGENT_CREATOR_TOKEN"
  health_path = "/healthz"        # default
  ```
- Boot supervisor:
  - When microapp declares `http_server`, nexo waits up to
    30 s for `GET <bind>:<port>/<health_path>` to return 200
    before marking microapp `ready`.
  - Health check failure → microapp marked `unhealthy`,
    surfaced via `nexo extension status`.
  - Supervisor polls health every 60 s; transient failures
    logged as warn.
- Token-shared auth pattern:
  - Microapp reads `<token_env>` from env at boot (passed by
    core via `initialize` env block).
  - All HTTP requests must include `Authorization: Bearer
    <token>` (or `X-Nexo-Token: <token>`).
  - Token rotation: operator changes env + reload → core
    notifies microapp via
    `nexo/notify/token_rotated { old_hash, new }` so
    microapp can swap without restart.
- CORS default: only loopback origin allowed; operator opt-in
  for external origins (future reverse-proxy scenarios).
- Bind enforcement: bind != `127.0.0.1` requires explicit
  operator opt-in via
  `extensions.yaml.<id>.allow_external_bind: true` (avoids
  accidental world-exposed services).
- Audit hook: HTTP requests can opt into being logged
  (microapp's own concern; just facilitate).

Done criteria:
- Microapp with `[capabilities.http_server]` is started by
  nexo and the health endpoint is verified before `ready`.
- Health failure surfaced in `nexo extension status` +
  admin-ui (future).
- Token rotation works without restart of the microapp.
- External bind without `allow_external_bind: true` → boot
  warn + reject (defense-in-depth).
- 5+ unit tests + 1 integration test (mock microapp with
  axum spinning up a healthcheck).
- INVENTORY toggle: `NEXO_MICROAPP_HTTP_SERVERS_ENABLED`
  global killswitch.

#### 82.13 — Agent processing pause + operator intervention (chat-takeover is one variant)   ⬜

Cross-app primitive. Operators sometimes need to suspend the
agent's autonomous processing for a specific scope and step in
manually. For chat agents this looks like "pause this
conversation, type a human reply, resume". For batch agents
this looks like "skip this job, override its output".
For event-driven agents it looks like "stop processing this
NATS subject, inject a manual event". The framework primitive
generalises across all of these via enum-typed scopes and
intervention actions.

Use-case agnostic by construction (Cross-cutting #6). Chat-
takeover is ONE scope variant — `Conversation` — and ONE
action variant — `Reply` — among N. V0 implements only the
chat-takeover combination; future variants are additive enum
slots that ship when the relevant agent shapes (batch, event-
driven, image-gen, etc.) land.

Used by any microapp where human intervention per scope is
part of the workflow: chat agents (sales / support / legal /
medical), batch processors (override or skip jobs),
event-driven routers (manual event injection), image
generators (override output before publish), voice
transcribers (manual correction).

Scope:
- Generic processing scope enum:
  ```rust
  enum ProcessingScope {
      Conversation {
          agent_id, channel, account_id, contact_id,
      },                          // chat — V0 implementation
      AgentBinding {
          agent_id, channel, account_id,
      },                          // pause whole channel binding
      Agent {
          agent_id,
      },                          // pause whole agent
      EventStream {
          agent_id,
          subject_pattern: String,
      },                          // pause subset of NATS events
      BatchQueue {
          agent_id,
          queue_name: String,
      },                          // pause batch processing queue
      Custom {
          agent_id,
          scope_kind: String,
          scope_id: String,
      },                          // extension hook for new shapes
  }
  ```
- Generic intervention action enum:
  ```rust
  enum InterventionAction {
      Reply {
          channel, account_id, to, body,
          msg_kind: "text"|"template"|"media",
          attachments: Vec<...>,
          reply_to_msg_id: Option<String>,
      },                          // chat — V0 implementation
      SkipItem {
          item_id, reason,
      },                          // batch / queue
      OverrideOutput {
          value: Value,
      },                          // any agent producing output
      InjectInput {
          content: Value,
      },                          // batch / event injection
      Custom {
          kind: String,
          payload: Value,
      },                          // extension hook
  }
  ```
- Generic processing control state:
  ```rust
  enum ProcessingControlState {
      AgentActive,
      PausedByOperator {
          scope: ProcessingScope,
          paused_at,
          operator_token_hash,
          reason: Option<String>,
      },
  }
  ```
  Persisted in SQLite (idempotent migration via Phase 77.17).
  Storage layer indexes on `scope` discriminant.
- Inbound / event / job dispatch checks state:
  - `AgentActive` → normal flow (chat: binding → turn;
    batch: queue → job; event: subject → handler).
  - `PausedByOperator { scope: Conversation, ... }` → inbound
    logged via 82.11 firehose with `kind: TranscriptAppended,
    role: "user"` but NO agent turn fired.
  - `PausedByOperator { scope: BatchQueue, ... }` → batch
    items queued, not picked up. (Future implementation.)
  - `PausedByOperator { scope: EventStream, ... }` → events
    dropped or queued per subject_pattern. (Future.)
- Admin RPC methods (extend 82.10):
  - `nexo/admin/processing/pause { scope: ProcessingScope,
    reason? }` → set `PausedByOperator`.
  - `nexo/admin/processing/resume { scope: ProcessingScope }`
    → set `AgentActive`. For chat: next inbound triggers
    agent turn with full context including operator
    interventions marked + system prompt addendum: *"Operator
    was active from <X> to <Y>. Their interventions are
    tagged `<operator>...</operator>` in the transcript.
    Continue from there."*
  - `nexo/admin/processing/intervention { scope:
    ProcessingScope, action: InterventionAction }` → publish
    intervention. For chat `Reply` action: routes via
    Phase 26 reply adapter; transcript entry tagged with the
    new `role: "Operator"` variant + `operator_token_hash`.
    For batch / event variants: future.
  - `nexo/admin/processing/state { scope: ProcessingScope }`
    → returns current `ProcessingControlState`.
- New capability gate: `operator_intervention` (subset of
  admin capabilities — declared granularly per microapp).
  Sub-gates per scope kind possible later
  (`operator_intervention_conversation`,
  `operator_intervention_batch`, etc.) but v0 starts with
  one combined gate.
- Notification firehose:
  `nexo/notify/processing_state_changed { scope:
  ProcessingScope, state, by_operator_token_hash?, at }`
  via 82.11's `agent_event` firehose with new variant
  `ProcessingStateChanged` (or as a dedicated notification —
  decide in spec).
- 82.11 schema extension: `AgentEventKind` gains a `role:
  "Operator"` valid value for `TranscriptAppended` (for
  chat scope's `Reply` action); future variants of action
  emit their own event kinds.
- Race-condition safety:
  - pause + resume idempotent (last-write-wins, audit
    logged).
  - intervention rejected (-32004 `not_paused`) if state
    is not `PausedByOperator` for the matching scope.
  - concurrent pause from two operators on the same scope:
    idempotent, latest token hash wins.
- Auto-resolve hook for 82.14: pausing a scope with a
  pending escalation that targets the same scope auto-
  resolves the escalation to `OperatorTakeover`.

Done criteria:
- V0 (chat-takeover): pause/resume cycle works end-to-end
  for `ProcessingScope::Conversation` + `InterventionAction::
  Reply`. Operator pauses → contact's next inbounds logged
  via 82.11 firehose `kind: TranscriptAppended, role: User`
  but agent silent → operator types reply → reply lands in
  WhatsApp via Phase 26 reply adapter marked `role: Operator`
  → operator resumes → next contact inbound triggers agent
  turn with full context.
- Agent receives operator's reply in its system prompt
  context, knows it was human (visible via `<operator>` tags
  in transcript).
- Agent does not "double-respond" to a message the operator
  already replied to.
- Per-scope isolation: pausing scope A does not affect scope
  B (same agent, different contacts; or same agent,
  different queue; etc.).
- Audit log row per pause / resume / intervention with
  `scope` discriminant.
- 82.11 transcripts persist `role: "Operator"` correctly for
  chat scope.
- 10+ unit tests including: chat happy path; missing scope
  variants reject cleanly with `not_implemented` error; enum
  forward-compat (adding a Custom variant doesn't break
  existing chat flow); concurrent pause idempotency.
- 1 e2e test simulating full chat-takeover flow.

V1+ scope (additive, NOT v0):
- `ProcessingScope::BatchQueue` + `InterventionAction::SkipItem`
  / `OverrideOutput`.
- `ProcessingScope::EventStream` + `InterventionAction::
  InjectInput`.
- `ProcessingScope::Agent` (whole-agent pause for maintenance).

Out of scope:
- Multi-operator conflict UI (which operator owns the
  takeover; v0 first-come-first-serve).
- Operator typing indicators visible to the contact.
- Auto-resume timeout (could be future enhancement).
- Operator-to-operator handoff between humans.

#### 82.14 — Agent attention request: `escalate_to_human` tool + state + notification (chat-question is one example)   ⬜

Cross-app primitive. Agents sometimes encounter work items
they cannot complete autonomously and need human attention.
For chat agents this is "customer asked something I cannot
answer". For batch agents this is "I found 47 invalid records
I cannot resolve". For image generators this is "policy
check failed, need a moderator". For event-driven agents this
is "unknown event kind, need engineering". Today the only
recourse is natural-language deferral, which is invisible to
operators. This subphase adds an explicit "ask for help"
channel via a built-in tool that flags the work item, raises a
firehose event, and surfaces a badge in operator UI.

Use-case agnostic by construction (Cross-cutting #6). The
tool name `escalate_to_human` and the escalation state are
agent-shape-neutral. Chat is ONE consumer; batch / image /
event / voice agents consume the same primitive.

Used by any agent with bounded autonomy (sales / support /
marketing / batch / image-gen / event-router / specialist
domain-expert agents).

Scope:
- New built-in tool `escalate_to_human` (provider-agnostic,
  use-case agnostic, registered in core ToolRegistry):
  ```json
  {
    "name": "escalate_to_human",
    "description": "Flag this work item for human attention when you cannot complete it autonomously. Used by any agent shape — chat, batch, event-driven, image-gen — to surface 'I need help here'.",
    "input_schema": {
      "type": "object",
      "required": ["summary", "reason"],
      "properties": {
        "summary": {
          "type": "string", "maxLength": 500,
          "description": "Brief description of why human attention is needed. Free-form prose."
        },
        "reason": {
          "type": "string",
          "enum": ["out_of_scope", "missing_data",
                   "needs_human_judgment", "complaint",
                   "error", "ambiguity", "policy_violation",
                   "other"]
        },
        "urgency": {
          "type": "string",
          "enum": ["low", "normal", "high"],
          "default": "normal"
        },
        "context": {
          "type": "object",
          "description": "Free-form key-value map carrying additional context. Examples: chat agent emits `{question: '...', customer_phone: '...'}`; batch agent emits `{job_id: '...', invalid_rows: 47}`; image-gen emits `{prompt: '...', policy: 'nudity'}`."
        }
      }
    }
  }
  ```
  Schema deliberately uses generic `summary` (not
  `question`) and generic `context: object` (not
  `extra_context: string`) so non-chat agents can populate
  meaningfully.
- Per-scope escalation state (uses same `ProcessingScope`
  enum from 82.13):
  ```rust
  enum EscalationState {
      None,
      Pending {
          scope: ProcessingScope,        // chat conversation, batch job, etc.
          summary, reason, urgency,
          context: Map<String, Value>,
          requested_at,
      },
      Resolved {
          resolved_at,
          by: ResolvedBy,
      },
  }
  enum ResolvedBy {
      OperatorTakeover,                  // 82.13 pause auto-resolves
      OperatorDismissed { reason: String },
      AgentResolved,                     // rare — agent later resolved itself
  }
  ```
  Persisted in SQLite alongside 82.13's pause state.
  Indexed on `scope` discriminant.
- Tool dispatch reads the agent's current `BindingContext`
  (82.1) plus its current scope context (chat: contact_id;
  batch: current job_id; etc.) to derive the
  `ProcessingScope` automatically. Agent does not need to
  pass scope manually — framework does it from the dispatch
  context.
- Tool sets state to `Pending` and emits firehose event via
  82.11's `agent_event` notification with new variant:
  ```rust
  AgentEventKind::EscalationRequested {
      agent_id, scope: ProcessingScope, summary, reason,
      urgency, context, requested_at,
  }
  AgentEventKind::EscalationResolved {
      agent_id, scope: ProcessingScope, resolved_at, by,
  }
  ```
- New admin RPC methods (extend 82.10):
  - `nexo/admin/escalations/list { filter?:
    pending|resolved|all, agent_id?, scope_kind?, limit? }`
    — `scope_kind` filters by `Conversation` |
    `BatchQueue` | etc.
  - `nexo/admin/escalations/resolve { scope:
    ProcessingScope, by: "dismissed"|"takeover",
    dismiss_reason? }` — note `scope` parameter, not
    chat-specific tuple.
- Auto-resolve trigger: when 82.13's `processing/pause` is
  invoked on a scope with a pending escalation matching the
  same scope, the escalation auto-resolves with
  `ResolvedBy::OperatorTakeover` + emits
  `EscalationResolved` event.
- De-duplication: if `escalation_state == Pending` already,
  the agent's `escalate_to_human` call is a no-op (returns
  ok with `already_pending` flag, does not double-notify).
- Throttle: max 3 escalations per conversation per hour to
  prevent agent loops; further calls rejected with rate-limit
  error.
- Default behaviour post-escalation:
  - Agent continues processing other items normally
    (deferral for chat = generic prose; for batch = mark
    item as `flagged_for_review`; for image-gen = continue
    queue).
  - Scope NOT auto-paused (operator can manually pause via
    82.13 if takeover needed).
  - Configurable per agent: `escalation_auto_pause: bool`
    (default `false`). When `true`, the agent's current
    scope auto-pauses after escalating. Useful for legal /
    medical / policy-sensitive agents.
- De-duplication: if `escalation_state == Pending` already
  for the same scope, the agent's `escalate_to_human` call
  is a no-op (returns ok with `already_pending` flag, does
  not double-notify).
- Throttle: max 3 escalations per scope per hour to prevent
  agent loops; further calls rejected with rate-limit error.
- New capability gate: `escalations_subscribe` (subset of
  admin) for microapps that want the firehose. Sub-gates
  later possible per `scope_kind`.
- INVENTORY env toggle: `NEXO_AGENT_ESCALATION_ENABLED`
  global killswitch.

Done criteria:
- Chat agent calls `escalate_to_human(summary, reason)` with
  context auto-populated → state becomes `Pending` for the
  conversation scope → 82.11 firehose emits
  `EscalationRequested` variant.
- Batch agent (future) calls same tool with `context: {
  job_id, error_count }` → state becomes `Pending` for
  `ProcessingScope::BatchQueue` scope → same firehose,
  different scope.
- Microapp with `escalations_subscribe` receives
  notification → UI surface (chat: badge on conversation;
  batch: badge on job; etc.) updates.
- Operator triggers 82.13 `processing/pause` for the
  matching scope + `intervention` action → escalation
  auto-resolves to `OperatorTakeover`.
- Operator dismisses without takeover → escalation resolves
  to `OperatorDismissed`.
- Agent re-calls `escalate_to_human` while `Pending` →
  no-op, single notification only.
- Throttle: 4th call within an hour for the same scope →
  rate-limit error.
- Restart: pending escalations persist (state in SQLite),
  across all scope variants.
- Schema is forward-compatible: `context: object` takes
  arbitrary structured data per agent shape; the framework
  does not interpret it, microapps do.
- 10+ unit tests including: chat happy path; non-chat scope
  escalation (mock); throttle hit; auto-resolve via 82.13
  pause; restart preservation.
- 1 e2e test simulating chat-flavour escalation flow.
- Audit log row per escalation request + resolve.

Out of scope for v0:
- Auto-categorisation of escalations by secondary LLM
  (clustering of `missing_data` reasons into reusable
  categories).
- Priority queue / SLA tracking.
- Round-robin assignment to multiple operators.
- Metrics: escalation rate per agent / per reason.
- Auto-train: resolved escalations feeding back into the
  agent's knowledge base.

---

**Phase 82 effort estimate**: 82.1 (1.5 days, BindingContext
struct + EffectiveBindingPolicy extension + intake fill +
inject_context_meta dual-write), 82.2 (1-2 days, axum/hyper
listener wraps existing 80.12 helpers), 82.3 (2-3 days, the
dispatch authorization and reply adapter wire-up are the
tricky parts), 82.4 (1-2 days, mostly new binding type
plumbing), 82.5 (1 day on top of 82.1), 82.6 (0.5 day
formalisation + docs), 82.7 (1 day extending Phase 16),
82.8 (0.5 day schema + filter), 82.9 (1-2 days SaaS template
+ docs), 82.10 (3-4 days admin RPC + granular gates +
domain methods + audit), 82.11 (2 days transcripts admin
RPC + firehose + redactor passthrough), 82.12 (1.5 days
HTTP server convention + supervisor + token auth), 82.13
(2-3 days operator takeover state + admin RPC + transcript
role:Operator + system-prompt addendum on resume), 82.14
(2 days escalate_to_human tool + state + notification +
admin RPC + auto-resolve hook). Total ~21-28 dev-days.

**MVP slice for first meta-microapp v0**: 82.1 + 82.5 +
82.6 + 82.10 + 82.11 + 82.12 = ~10-13 dev-days. With
Phase 83.4 SDK (~2-3 days) on top: ~12-16 dev-days for a
fully working agent-creator microapp with admin RPC, live
transcript view, and self-hosted React UI.

**MVP slice for marketing-saas-v0**: 82.1 + 82.2 + 82.3 (or
skip 82.3 by using NATS-transport extension) + 82.5 + 82.6
+ 82.9 = ~6-9 dev-days. 82.4, 82.7, 82.8 land post-MVP.

---

### Phase 83 — Microapp framework foundation   ⬜

Phase 83 sits one level above the Phase 82 core primitives.
Where Phase 82 closes core gaps that any multi-tenant
extension needs (binding context, outbound dispatch, audit
isolation, webhook ingestion, admin RPC, transcripts,
HTTP hosting), Phase 83 ships the *reusable framework +
library + templates + contract* that turns "writing a
microapp" into a 1-2 day exercise instead of re-implementing
plumbing per app.

Phase 83 deliberately stays inside the **Phase 11 subprocess
extension shape**. No Phase 81 in-process plugin work is
pulled in. A microapp is a Phase 11 stdio (or NATS-transport)
extension that follows the documented JSON-RPC contract,
optionally importing the Rust SDK if implemented in Rust.
Python / Node / TypeScript / Go microapps are first-class —
they implement the same contract independently.

The driving validation case is `ventas-etb` (the first
reference microapp, extracting the existing `ana` agent from
its monolithic `agents.d/ana.yaml` system_prompt into a
persona-aware Phase 11 extension). A second small microapp
validates SDK reusability before the phase ships.

Goal: any operator can author a new microapp (sales agent,
marketing platform, support deflector, NPS surveyor, FAQ
bot, batch processor, webhook router, analytics reporter,
voice transcriber, image-gen queue, etc.) by cloning a
template, filling in the business logic, and dropping the
binary into the extensions directory. The framework
guarantees: persona-aware config propagation, hot-reloadable
state, defense-in-depth compliance hooks, structural inbound
metadata, audit isolation per agent, and a stable JSON-RPC
contract that does not rot across nexo releases.

#### 83.1 — Per-agent extension config propagation   ⬜

Universal primitive. Today extensions read a global config
file at boot. Microapps that serve multiple personas /
tenants need per-agent config without the operator dropping
files into per-extension state dirs.

Scope:
- Operator declares per-agent config in `agents.yaml`:
  ```yaml
  agents:
    - id: ana
      extensions: [ventas-etb]
      extensions_config:
        ventas-etb:
          regional: bogota
          asesor_phone: "573115728852"
  ```
- Core propagates `extensions_config[<ext_id>]` to the
  subprocess via the JSON-RPC `initialize` method, indexed
  by `agent_id` (a single subprocess receives configs for
  all agents that bind to it).
- New JSON-RPC method `agents/updated` is sent on Phase 18
  hot-reload so the extension refreshes its
  `HashMap<agent_id, Config>` without restart.
- Combined with 82.1 BindingContext, tool calls dispatch to
  the right per-persona config in O(1).

Done criteria:
- Multi-persona round-trip: 3 personas in `agents.yaml` map
  to 3 distinct configs visible to the same subprocess.
- Hot-reload: changing one agent's config in YAML refreshes
  the extension's map within 1 s without dispatch
  interruption.
- Backward compat: extensions ignoring the new fields keep
  working.
- 6+ unit tests + 1 integration test covering happy path,
  hot-reload, malformed config rejection, missing
  agent_id, isolation between agents.

#### 83.2 — Extension-contributed skills   ⬜

Today `agents.yaml.skills_dir` is a static path the operator
must point at the right folder. When the skill ships *with*
the extension, that path coupling forces operators to know
extension internals.

Scope:
- Extension declares `[capabilities] skills = ["ventas-flujo",
  ...]` in `plugin.toml`.
- Skill markdown lives in
  `extensions/<id>/skills/<name>/SKILL.md`.
- Core auto-discovers skills from loaded extensions and
  merges them into the agent's skill scope when the
  extension is in `agents.yaml.<id>.extensions: [...]`.
- `agents.yaml.<id>.skills: [<name>]` references skills by
  name without needing a path.
- Operator-declared `skills_dir` still works; extension
  skills *merge on top*.

Done criteria:
- Skill `ventas-flujo` shipped by `ventas-etb` is loadable
  via `agents.yaml.skills: [ventas-flujo]` without
  `skills_dir` pointing anywhere.
- Conflict resolution: operator-declared skill with the same
  name overrides the extension's.
- 5+ unit tests + 1 integration test.

#### 83.3 — Hook interceptor (vote-to-block)   ⬜

Phase 11 hooks today are observer-only — they see the
inbound message, return `{abort: false}`, and cannot influence
dispatch. Compliance enforcement (anti-loop, anti-manipulation,
opt-out) deserves deterministic hooks that can short-circuit.

Scope:
- Hook handler returns `HookDecision`:
  ```json
  {
    "decision": "allow" | "block" | "transform",
    "reason": "anti-loop pattern detected",
    "transformed_body": "Hasta luego",
    "do_not_reply_again": true
  }
  ```
- Core remains authoritative: subprocess can only *vote* to
  block; the core decides whether to apply (audit-logged).
- Defense-in-depth: even if the hook subprocess crashes or
  returns garbage, dispatch fails-open (never fails-closed
  silently — operator sees a warn).
- Audit log row written for every block / transform.

Done criteria:
- `block` decision short-circuits dispatch, agent never sees
  the inbound, operator-visible audit row written.
- `transform` decision rewrites body before agent sees it.
- Failed hook (timeout, crash, malformed response) triggers
  fail-open with warn log.
- 6+ unit tests + 1 integration test.

#### 83.4 — `crates/microapp-sdk-rust` reusable Rust helper   ⬜

New optional Rust crate that any Rust microapp can import to
avoid re-implementing plumbing. Python / Node / TS microapps
follow the contract document (83.6) instead.

Scope:
- `BindingContext` parser from JSON-RPC `_nexo_context`
  field.
- `PersonaConfigMap<T>` data structure with hot-reload via
  `agents/updated` handler.
- `MarkdownWatcher` (notify-rs based) for hot-reloadable
  content files (tarifarios, templates, knowledge bases).
- `LocalSqliteStore` helper with isolation guard
  (every query takes `agent_id` to prevent cross-tenant
  leak).
- `OutboundDispatcher` wrapper for `nexo/dispatch`
  (depends on Phase 82.3).
- `AdminClient` wrapper for `nexo/admin/*` (depends on
  Phase 82.10) — covers all admin domains (agents,
  credentials, pairing, llm_providers, processing,
  escalations, agent_events).
- `AgentEventFirehose` consumer for `nexo/notify/agent_event`
  (depends on 82.11) — typed `AgentEventKind` discriminator,
  per-kind filter, kind-aware deserialization. Replaces the
  earlier `TranscriptFirehose` naming so the SDK is
  use-case agnostic; `TranscriptAppended` is one variant
  the consumer handles, alongside future variants
  (`BatchJobCompleted`, `OutputProduced`,
  `EscalationRequested`, `ProcessingStateChanged`, etc.).
- `ProcessingControlClient` wrapper for 82.13's
  `nexo/admin/processing/*` — generic over
  `ProcessingScope` + `InterventionAction` enums; chat
  microapps use `ProcessingScope::Conversation` +
  `InterventionAction::Reply`, batch microapps use future
  variants without API changes.
- `EscalationClient` wrapper for 82.14's
  `nexo/admin/escalations/*` — accepts arbitrary
  `ProcessingScope` + free-form `context: serde_json::Value`
  in the request payload.
- `HttpServerHelper` for the `[capabilities.http_server]`
  pattern (depends on 82.12).
- `ToolHandler` trait that microapp tools implement.
- `JsonRpcLoop` runner that handles `initialize`,
  `tools/list`, `tools/call`, `agents/updated`,
  `hooks/<name>`, `shutdown`.

Done criteria:
- `cargo doc -p microapp-sdk-rust` produces complete API
  reference.
- 25+ unit tests covering each helper.
- Reference example `examples/hello-microapp.rs` ~50 LOC
  shows a fully functional microapp with one tool.

#### 83.5 — `crates/compliance-primitives` reusable library   ⬜

Anti-loop, anti-manipulation, opt-out, PII redaction, rate
limit per user, GDPR consent tracking — universal compliance
primitives that every conversational microapp needs.
Implemented once in a shared crate.

Scope:
- `AntiLoopDetector { threshold, window }` — detects same
  message ≥ N times within a window, or bot-reply
  signatures like "Recibido", "Mensaje automático", etc.
- `AntiManipulationMatcher` — detects "ignore previous",
  "act as", "new role", prompt-injection markers.
- `OptOutMatcher::with_phrases(...)` — language-aware
  opt-out detection.
- `PIIRedactor` — credit cards, SSN, phone numbers
  (configurable). Runs server-side before LLM sees the
  text.
- `RateLimitPerUser { tokens_per_window, max }` — token
  bucket per `sender_id`.
- `ConsentTracker` — opt-in store + audit, GDPR /
  CAN-SPAM / WhatsApp Business Policy compliant.

Each primitive plugs into the 83.3 hook interceptor;
microapps configure thresholds via `extensions_config`.

Done criteria:
- Each primitive has 5+ unit tests covering edge cases.
- Provider-agnostic: zero deps on a specific LLM.
- Used by both `ventas-etb` (83.8) and the second microapp
  (83.10) demonstrating reusability.

#### 83.6 — Microapp contract document   ⬜

Language-agnostic spec for "what is a nexo microapp". Docs
page `docs/src/microapps/contract.md`. Mandatory reading
for anyone authoring a microapp in any language.

Scope:
- JSON-RPC methods (core → microapp): `initialize`,
  `tools/list`, `tools/call`, `agents/updated`,
  `hooks/<name>`, `shutdown`.
- JSON-RPC methods (microapp → core): `nexo/dispatch`,
  `nexo/admin/*`, capability-gated.
- Notifications (core → microapp):
  `nexo/notify/transcript_appended`,
  `nexo/notify/pairing_status_changed`,
  `nexo/notify/token_rotated`.
- Shapes: `BindingContext`, `InboundMsgRef`,
  `ExtensionConfig`, `HookDecision`, `ToolCallRequest`,
  `ToolCallResponse`, error envelope.
- Conventions: tool name namespacing
  (`<extension_id>_<tool>`), error codes (-32000 to
  -32099 reserved), timeouts (default 30 s, configurable).
- Backward compat rules: additive fields always; deprecation
  requires nexo release N to warn + release N+1 to remove.

Done criteria:
- Doc renders without broken links.
- A volunteer can implement a hello-world microapp in
  Python following the doc alone, without reading nexo
  source.
- Examples for 3 languages embedded inline.

#### 83.7 — Microapp templates (Rust + Python + TypeScript)   ⬜

Three template scaffolds under `extensions/`. Each ships a
working hello-world microapp in its language.

Scope:
- `extensions/template-microapp-rust/` — depends on
  `microapp-sdk-rust`, ~150 LOC, 2 example tools, hook
  observer, README.
- `extensions/template-microapp-python/` — `asyncio` +
  `jsonrpc` library, Python 3.11+, ~200 LOC, README.
- `extensions/template-microapp-typescript/` — Node 20+,
  simple stdio JSON-RPC, ~250 LOC, README.

Each template demonstrates: BindingContext parsing,
persona config map, one tool with `_nexo_context` access,
one hook observer, `nexo/dispatch` outbound call.

Done criteria:
- Each template builds + runs the smoke test
  (`initialize → tools/list → tools/call → shutdown`).
- README explains the contract by reference to 83.6.

#### 83.8 — `ventas-etb` reference microapp   ⬜

The first real microapp built using 83.1 - 83.7. Validates
that the framework is usable for actual production work,
not just toy examples. Ports the existing `ana` agent
(currently a 174-line monolithic system_prompt in
`agents.d/ana.yaml`) into a persona-aware Phase 11
extension.

Scope:
- Tools: `etb_get_planes`, `etb_check_coverage` (stub for
  now), `etb_register_lead`, `etb_lead_summary`.
- Hot-reloadable tarifarios (markdown) per region/operator
  via `microapp-sdk-rust::MarkdownWatcher`.
- Local SQLite `leads.db` with isolation per `agent_id`.
- Skill `ventas-flujo` (the 7-step sales flow) shipped via
  83.2 mechanism.
- Compliance via `compliance-primitives` (anti-loop,
  anti-manipulation, opt-out, rate-limit) wired into 83.3
  hook interceptor.
- Persona-agnostic: same extension serves Ana (Bogotá
  residencial), Carlos (Bogotá empresarial), María (Cali)
  and any other persona declared in `agents.yaml`.

Done criteria:
- 3 personas (Ana / Carlos / María) running off the same
  extension binary.
- Tarifario edits in markdown propagate within 1 s without
  restart.
- `etb_lead_summary` returns KPIs filtered per `agent_id`.
- Defense-in-depth verified: even if the LLM is jailbroken,
  outbound to non-allowlisted phone is blocked by core.
- 15+ unit tests + 1 e2e test simulating a full sales
  conversation end-to-end.

#### 83.9 — `ana` cutover   ⬜

Production cutover of the existing `ana` agent from the
monolithic `agents.d/ana.yaml` to the new ventas-etb
microapp. Risk-controlled, reversible.

Scope:
- Simplify `config/agents.d/ana.yaml` from ~174 lines to
  ~30 lines: keep system_prompt as a short rule-set,
  remove inline tarifario, add `extensions: [ventas-etb]`
  + `extensions_config` block.
- Stage in shadow mode for 24 h: both old (yaml-only) and
  new (extension) variants run in parallel under different
  binding ids; compare lead capture quality + response
  time.
- Cutover atomic: switch the production WhatsApp binding
  from old to new at a low-traffic hour.
- Rollback plan documented: re-enable old binding within
  60 s if regressions detected.

Done criteria:
- Shadow mode shows ≥ 95 % parity in lead capture vs old
  flow (small drift tolerated for known bug fixes).
- Cutover completed without lead loss.
- 7-day post-cutover monitoring shows stable operation.
- Rollback steps tested in staging before production cut.

#### 83.10 — Second microapp validation   ⬜

Build a small second microapp in ~1 day using the templates
+ SDK + compliance-primitives. Goal: prove the framework is
truly reusable and surface any gaps the ventas-etb design
did not.

**Out-of-tree by default**: the second microapp lives in a
**separate git repository** owned by the operator (e.g.,
`~/projects/agent-creator-microapp/`), NOT under
`extensions/` of this monorepo. It depends on the SDK +
React component library via path / git / crates.io
dependency. This sub-phase covers scaffolding the repo,
verifying end-to-end that the framework primitives work
from outside the nexo monorepo, and documenting the
out-of-tree workflow.

Candidate microapps (operator picks):
- `agent-creator` — meta-microapp with React UI (uses the
  full Phase 82.10/82.11/82.12 stack: admin RPC, transcript
  firehose, HTTP server hosting). This is the most
  ambitious candidate; arguably the strongest validation.
  **Multi-WhatsApp-instance support (v1 scope is
  WhatsApp-only)**: the UI must surface the many-to-many
  assignment between WhatsApp credentials (multiple
  WhatsApp numbers) and agents — 1 WA number can serve N
  agents and 1 agent can answer N WA numbers. The
  `WhatsApp Numbers` page lists all WA credentials with
  their agent assignments + add/remove rows; the `Agent
  detail` page lists the agent's `inbound_bindings` with
  toggles to attach/detach existing WA numbers. Both
  directions hit `nexo/admin/credentials/*` and
  `nexo/admin/agents/upsert` from 82.10. The framework
  itself is channel-agnostic — Telegram / email / future
  channels are out of UI scope for v1 but not architectural
  blockers; a v2 of this microapp can add tabs for them
  without core changes.
- `nps-surveyor` — sends NPS surveys post-conversation,
  captures response, stores per-tenant in local SQLite.
- `faq-deflector` — Q&A bot trained on a markdown
  knowledge base, anti-spam compliance built in.
- `webhook-event-router` — receives a webhook event,
  classifies it, dispatches the right notification.
- `daily-digest` — heartbeat-driven, runs once a day,
  summarises yesterday's transcripts and sends to operator
  channel.

Done criteria:
- Microapp lives in its own git repository, not under
  `extensions/` of this monorepo.
- Microapp authored start-to-finish in ≤ 1 dev-day (or
  ≤ 3 dev-days if the candidate is `agent-creator` given
  the React UI scope, since 83.12 + 83.13 absorb most of
  the UI work).
- Uses ≥ 3 of the framework primitives (SDK helpers,
  compliance, hooks, persona config, admin RPC,
  transcripts firehose, HTTP server hosting, takeover,
  escalation).
- Cargo.toml + package.json declare the SDK as a path-dep
  during development; after 83.14 ships, migrate to
  versioned published deps.
- A retrospective lists every gap or rough edge that
  authoring surfaced — those become new sub-phases (Phase
  82+83 grows organically) or post-Phase-83 follow-ups.

#### 83.11 — Docs + admin-ui sync   ⬜

Mandatory docs + admin-ui closeout for any phase touching
operator surface.

Scope:
- New section `docs/src/microapps/`:
  - `contract.md` (from 83.6 — link, not duplicate).
  - `getting-started.md` — clone a template, ship in 1 hour.
  - `sdk-rust.md` — Rust SDK reference + cookbook.
  - `compliance-primitives.md` — when to use which primitive.
  - `templates.md` — language-by-language reference.
  - `ventas-etb-walkthrough.md` — annotated full source
    of the reference microapp, line-by-line.
  - `meta-microapp-walkthrough.md` — annotated source of
    the agent-creator microapp covering admin RPC,
    transcript firehose, HTTP server hosting.
- `admin-ui/PHASES.md` tech-debt entries:
  - Microapp registry panel — list installed microapps,
    their personas, last reload time.
  - Persona config inspector per agent — view +
    hot-edit `extensions_config` blocks.
  - Compliance event feed — live tail of hook decisions
    (block / transform / allow), audit-log linked.
  - Microapp doctor — pre-flight check for each microapp:
    contract version, declared tools, declared skills,
    config schema, missing env vars.
  - Microapp admin audit viewer — tail of
    `microapp_admin_audit` rows per microapp.
  - Microapp HTTP health dashboard — per-microapp
    `[capabilities.http_server]` health timeline.

Done criteria:
- `mdbook build docs` passes locally.
- 6 admin-ui tech-debt entries registered.
- `crates/setup/src/capabilities.rs::INVENTORY` updated
  for any new env toggles introduced by 82.x or 83.x.

#### 83.12 — Meta-microapp React UI reference scaffold   ⬜

Reusable pattern for any microapp that ships its own
graphical interface. Backend hosting is covered by 82.12
(`[capabilities.http_server]` + boot supervisor + token
auth between core and microapp). 83.12 covers the
**browser-side** of the stack: how the React app
authenticates with the microapp's own backend, how the
backend serves the SPA, and how live updates from the
transcripts firehose stream into the UI.

This subphase produces a single, opinionated reference
scaffold that any future microapp with a UI can clone or
study. It is NOT a fixed framework — operators are free
to use any frontend stack — but having one canonical
scaffold removes the "what auth flow / SPA serving /
WebSocket pattern do I use?" decision overhead.

Scope:
- Browser → backend auth flow:
  - HttpOnly + Secure + SameSite=Strict cookie carrying
    the same token that core injects via `token_env`
    (Phase 82.12).
  - Login page that POSTs `Authorization: Bearer <token>`
    once, backend sets the cookie, browser keeps it.
  - Token rotation via `nexo/notify/token_rotated` (Phase
    82.12) clears the cookie and forces re-login.
- SPA serving convention in axum (or equivalent backend
  framework):
  - Static `dist/` served from `/` with gzip + brotli
    compression.
  - Fallback for SPA routes: any unmatched `GET /*` returns
    `index.html` (so React Router deep-links work).
  - `/api/*` reserved for the microapp's REST endpoints.
  - `/ws/*` reserved for WebSocket subscriptions.
- TypeScript types generated from admin RPC schemas:
  - `nexo_admin_types.ts` auto-generated at microapp build
    time using `serde_typescript` (or equivalent) from the
    Rust admin payload structs.
  - React UI imports `BindingContext`, `InboundMsgRef`,
    `HookDecision`, etc. from the generated module.
- Reference React app shipped in
  `extensions/template-microapp-rust-with-ui/frontend/`:
  - `pages/agents.tsx` — list + create + edit + delete
  - `pages/agent_detail.tsx` — persona config inspector +
    conversation viewer
  - `pages/channels.tsx` — list credentials + register
    new + revoke
  - `pages/pairings.tsx` — start pairing + QR display +
    live status (subscribed to
    `nexo/notify/pairing_status_changed`)
  - `pages/llm_keys.tsx` — list providers + upsert + delete
  - `pages/audit.tsx` — tail of `microapp_admin_audit`
    rows
- WebSocket subscriber pattern:
  - One WS connection from React per browser tab.
  - Backend multiplexes: subscribes to NATS firehose
    (`nexo/notify/transcript_appended`) and forwards
    relevant frames to the WS clients.
  - Client-side reconnect with exponential backoff.
- QR display for pairing flow:
  - Backend converts QR payload (from Phase 26 pairing
    `qr_payload` bytes) into a base64-encoded PNG via the
    `qrcode` crate.
  - React renders inline `<img src="data:image/png;base64,...">`.
- Build pipeline + Cargo integration:
  - `frontend/` has its own `package.json`, `vite.config.ts`,
    `tsconfig.json`.
  - `npm run build` produces `frontend/dist/`.
  - Backend `build.rs` checks for `dist/` and refuses to
    compile in release mode if missing (so we never ship a
    backend without its UI).
  - Bundle: `dist/` is `include_bytes!` into the binary OR
    served from `state_dir/ui/` (operator-configurable).

Done criteria:
- Reference scaffold builds end-to-end:
  `npm run build && cargo build --release` produces a
  single binary that, when launched by nexo, serves a
  working React UI on the configured port.
- Auth flow works: browser logs in once, cookie persists,
  token rotation invalidates the session.
- Live updates work: a tool call to `marketing_register_lead`
  on agent X causes the conversation viewer in the UI to
  show the new message within 1 s of the transcript
  append.
- QR pairing works: clicking "Link WhatsApp" in the UI
  triggers `nexo/admin/pairing/start`, the QR appears in
  the modal, status updates to "linked" via WebSocket
  when the user scans.
- 8+ unit tests on the backend SPA-serving + cookie auth
  middleware.
- 4+ playwright/cypress tests on the React app covering
  login, agents-CRUD, pairings, conversations.
- README documents how operators clone the scaffold and
  swap the React app for their own frontend stack.

Out of scope:
- Multi-operator auth (sharing the UI between several
  human operators with different roles). Single-operator
  trust model only — token-based local-only.
- Cloud-hosted variants (the UI is loopback-only by
  default; external bind requires explicit operator
  opt-in via 82.12's `allow_external_bind`).
- Marketplace-style microapp UI launcher that aggregates
  multiple microapp UIs in one tab.

#### 83.13 — `microapp-ui-react` reusable component library (WhatsApp Web-inspired) — OPT-IN chat helper, NOT core   ⬜

**Explicitly opt-in chat-specific helper.** Per Cross-cutting
guarantee #6 (use-case agnostic by construction), 83.13 is NOT
part of the core framework contract. Microapps that do not
have a conversational UI (batch dashboards, image-gen
galleries, webhook event logs, audit viewers) **must** be
able to ship their React UI using only 83.12 infrastructure +
their own components, without ever importing from 83.13.

Reusable **chat-flavoured** visual layer above the 83.12 React
UI infrastructure. Where 83.12 covers auth + SPA serving +
admin client + WebSocket + build pipeline (the plumbing that
lets a microapp ship its own React UI of any shape), 83.13
ships **chat-specific components** — a WhatsApp Web-inspired
layout consuming the chat-flavoured slices of the 82.x schemas
(`ProcessingScope::Conversation`, `InterventionAction::Reply`,
`AgentEventKind::TranscriptAppended`).

Used by any microapp with a conversational UI: meta-microapp
chat viewer, support-deflect operator dashboard,
marketing-saas lead inbox, ventas-etb leads viewer, and any
future microapp with a chat interface.

Sibling future opt-in libs that 83.13 does NOT preclude
(future Phase 84+ candidates):
- `microapp-ui-batch-dashboard-react` — for batch agent
  microapps (job grid + status timeline).
- `microapp-ui-event-log-react` — for event-driven router
  microapps (event stream viewer).
- `microapp-ui-output-gallery-react` — for image-gen / voice
  output viewing.
- `microapp-ui-audit-viewer-react` — for compliance / audit
  microapps.

Each future lib is its own opt-in package, mirroring 83.13's
pattern but consuming different schema slices. None of them
become "core" — core remains the framework primitives in 82.x.

The WhatsApp Web layout is chosen for operator familiarity in
the chat use case — operators interact with chat UIs for hours
per day, and reusing a mental model they already have reduces
cognitive load and training overhead. WhatsApp is the dominant
inbound channel for the v1 chat-flavoured microapps, so the
visual analogy is direct.

Scope:
- TypeScript package published as `@nexo/microapp-ui-react`
  (shipped under `crates/microapp-sdk-react/` mirroring
  `crates/microapp-sdk-rust/`).
- 3-column WhatsApp Web layout:
  - Left: chat list with search + filters + state badges
  - Centre: conversation thread with composer
  - Right: contact details panel with actions
- Core components (12+):
  - `<ChatList>` — sidebar list with search + filters.
  - `<ChatItem>` — individual chat preview with badges
    (escalation 🔴/🟡/🔵 by urgency, paused ⏸, unread
    count ✓).
  - `<ChatHeader>` — top of conversation: avatar + name +
    state text (`paused` / `escalation pending` / `active`).
  - `<MessageThread>` — scrollable area with bubbles +
    auto-scroll; loads paginated history via 82.11 admin
    RPC.
  - `<MessageBubble>` — role-aware styling:
    - `role: user` → left aligned, light grey.
    - `role: assistant` → right aligned, light green.
    - `role: operator` → right aligned, dark green + 👤
      tag (Phase 82.13 transcript role).
    - `role: tool` → centred, monospace.
  - `<MessageComposer>` — textarea + send + attachment +
    emoji picker; disabled when agent active without pause,
    enabled after 82.13 pause.
  - `<ContactSidebar>` — right panel with metadata +
    action buttons: `Pause` / `Resume` / `Escalate` / `Take
    over` / `Dismiss escalation`.
  - `<EscalationBanner>` — banner at top when escalation is
    `Pending`; clicking opens `<EscalationModal>`.
  - `<PauseBanner>` — banner when conversation paused, shows
    "Paused by operator at HH:MM".
  - `<EscalationModal>` — modal with question + reason +
    extra_context + `Take over` / `Dismiss` actions.
  - `<QrPairingModal>` — modal with QR for linking
    WhatsApp/etc + live status (subscribed to
    `nexo/notify/pairing_status_changed`).
  - `<TokenSwapToast>` — toast notification when token
    rotates (Phase 82.12 `nexo/notify/token_rotated`).
- Theme system:
  - WhatsApp green primary `#25D366` by default.
  - CSS variables: `--color-primary`,
    `--color-bubble-user`, `--color-bubble-agent`,
    `--color-bubble-operator`, `--color-bg`, etc.
  - Dark mode via CSS var switch.
  - Themeable per microapp without recompiling the library.
- Storybook docs:
  - Each component with isolated story.
  - Variants: paused chat, escalated chat, dark mode, etc.
  - Live demos without backend.
- Build artifact:
  - ESM + CJS exports.
  - TypeScript declarations bundled.
  - Tree-shakable per-component import.

Done criteria:
- 12+ core components implemented with TypeScript types
  derived from 82.10 / 82.11 / 82.13 / 82.14 schemas.
- Storybook published with stories for each component and
  each visual state.
- 15+ component tests (Vitest + Testing Library).
- 4+ E2E tests (Playwright) covering: load chat history,
  send message, pause/resume cycle, escalation flow.
- Theme system with CSS vars + dark mode functional.
- Packaged as TS package with `package.json` exporting
  named components.
- README with quick-start: "import + use, override theme
  via CSS vars".
- Reusable: 83.10 (agent-creator validation) and 83.8
  (ventas-etb) both import the lib in their respective
  React apps; ~3-5 days of UI work saved per future
  microapp.

Out of scope for v0:
- Voice messages UI (record button + waveform).
- Video calls.
- Stickers / GIFs.
- Message reactions.
- Visual read receipts (blue ticks per WhatsApp).
- i18n / RTL languages (CSS prep ready, content
  translation operator-side).

#### 83.14 — Publish SDK crates + npm package   ⬜

Migration milestone from path-dep development workflow to
published artifacts. Triggered when the SDK API stabilises
post 83.4 + 83.13 ejecutar — the framework drops a 0.1.0
release and out-of-tree consumers (agent-creator and future
operator-owned microapps) switch from path / file
dependencies to versioned published ones.

Until 83.14 ships, out-of-tree microapps use:
```toml
nexo-microapp-sdk-rust = { path = "../nexo-rs/crates/microapp-sdk-rust" }
```
```json
"@nexo/microapp-ui-react": "file:../nexo-rs/crates/microapp-sdk-react"
```

After 83.14 ships:
```toml
nexo-microapp-sdk-rust = "0.1"
nexo-compliance-primitives = "0.1"
```
```json
"@nexo/microapp-ui-react": "^0.1.0"
```

Scope:
- Cargo workspace metadata for `microapp-sdk-rust`,
  `compliance-primitives`, and any other framework crates
  marked publishable: `license`, `description`, `readme`,
  `repository`, `keywords`, `categories` fields complete.
- release-plz integration: per-crate CHANGELOG.md, per-crate
  semver bump on PR, auto-publish on tag (the existing
  Phase 27.x pipeline picks them up once metadata is
  complete).
- npm package for `@nexo/microapp-ui-react` shipped under
  `crates/microapp-sdk-react/`:
  - `package.json` with `name`, `version`, `main`, `types`,
    `exports`, `repository`, `license`.
  - GitHub Actions workflow with `NPM_TOKEN` secret that
    publishes on tag.
  - Versioning sincronizada con la lib Rust (same major /
    minor; patch independent OK).
- Migrate `agent-creator-microapp/` (and any other
  out-of-tree consumer) Cargo.toml + frontend/package.json
  from path / file deps to versioned published deps.
- Update SDK README with versioning policy: semver,
  deprecation cadence (1 release of grace), breaking-change
  comms via CHANGELOG.

Done criteria:
- `cargo publish -p nexo-microapp-sdk-rust` succeeds on a
  clean checkout (no path deps in published artifact).
- `cargo publish -p nexo-compliance-primitives` succeeds.
- `npm publish` for `@nexo/microapp-ui-react` succeeds.
- `agent-creator-microapp` builds against published versions
  ONLY (zero path/git deps in its `Cargo.toml` +
  `package.json`).
- Operator clone of `agent-creator-microapp` does NOT need
  the `nexo-rs` source tree to compile.
- Versioning policy documented in SDK READMEs.
- 1 dev-day total.

Out of scope:
- Pre-1.0 stability guarantee (still 0.x — breaking changes
  allowed with bump).
- Auto-changelog generation beyond what release-plz already
  handles.
- Mirror to private registries (operator concern).

---

**Phase 83 effort estimate**: 83.1 (1.5 days), 83.2 (1-1.5
days), 83.3 (1.5-2 days), 83.4 (3-4 days SDK, +1d for the
expanded surface covering admin client + firehose consumer
+ HTTP helper + 82.13/82.14 wrappers), 83.5 (2-3 days
compliance lib), 83.6 (1 day contract doc), 83.7 (2 days for
3 templates since most logic is in the SDK), 83.8 (4-5 days
reference microapp), 83.9 (1-2 days cutover), 83.10 (1-3
days second microapp; upper bound if `agent-creator` is the
validation case — note that 83.12 + 83.13 absorb most of the
React UI work that was previously implicit in 83.10's upper
bound), 83.11 (1.5 days docs + admin-ui), 83.12 (2-3 days
React UI infrastructure scaffold), 83.13 (2 days WhatsApp-
Web-inspired component library + Storybook + theme system),
83.14 (1 day publish SDK crates + npm package + migrate
out-of-tree consumers from path-dep to versioned). Total
~25-33 dev-days.

**Critical path**: 83.1 + 83.2 + 83.3 (core primitives) →
83.4 + 83.5 (libraries) → 83.6 + 83.7 (contract + templates)
→ 83.12 (React UI infrastructure) → 83.13 (component lib) →
83.8 (reference microapp) → 83.9 (cutover) + 83.10 (second
validation) → 83.14 (publish + migrate) → 83.11 (docs).
83.12 + 83.13 sit between 83.7 and 83.8/83.10 because the
agent-creator validation in 83.10
reuses the scaffold + the component library.

**Dependencies on Phase 82**: 83.3 hook interceptor reuses
patterns from 82.1 BindingContext; 83.4 SDK wraps Phase
82.3 outbound dispatch + 82.10 admin RPC + 82.11 firehose +
82.12 HTTP server pattern; 83.8 ventas-etb reuses 82.1 +
82.5 inbound metadata. Phase 82 should ship first, or at
minimum 82.1 must land before 83.1 ejecutar.

**Out of scope vs Phase 81**: Phase 83 is *strictly* Phase 11
subprocess. No `NexoPlugin` trait work, no in-process
plugin discovery, no in-process channel adapters, no
in-process advisor registration. Those stay in Phase 81
backlog.

---

## Roadmap of execution

Recommended order assuming the user prioritises the
agent-creator meta-microapp as the first product:

1. **82.1** BindingContext (foundation) — 1.5 days
2. **82.5** InboundMsgRef metadata — 1 day
3. **82.6** state_root convention — 0.5 day
4. **82.10** admin RPC + capabilities + domain methods —
   3-4 days
5. **82.13** operator chat takeover (pause + resume +
   operator_reply, depends on 82.10) — 2-3 days
6. **82.14** agent escalation tool + state + notification
   (depends on 82.10; auto-resolves via 82.13) — 2 days
7. **82.11** transcripts firehose — 2 days
8. **82.12** HTTP server pattern — 1.5 days
9. **83.1** per-agent extension config — 1.5 days
10. **83.4** microapp-sdk-rust (with all wrappers
    including 82.13 / 82.14 clients) — 3-4 days
11. **83.6** contract doc — 1 day
12. **83.12** React UI infrastructure scaffold — 2-3 days
13. **83.13** WhatsApp Web-inspired component library
    (depends on 83.12) — 2 days
14. **83.10** build `agent-creator` in its **own git
    repository** (not under `extensions/`); consume the
    framework via path-deps during dev — 1-3 days
15. **83.14** publish SDK crates + npm package; migrate
    `agent-creator` from path-deps to versioned published
    deps — 1 day

Total: ~27-33 dev-days for the agent-creator meta-microapp
fully working with React UI (chat-list / conversation /
contact panel), live transcript view, escalation badges,
operator takeover with manual reply, and admin-driven CRUD
of agents/credentials/pairing/LLM keys, plus published SDK
artifacts so future operator microapps consume the framework
from crates.io / npm without filesystem coupling to nexo-rs.

The marketing-saas-v0 path can be interleaved later via
82.2 + 82.3 + 82.4 + 82.7 + 82.8 + 82.9 (~7-9 dev-days
extra) plus Phase 83.5 + 83.7 + 83.8 + 83.9 (~10-14 dev-days
extra) once meta-microapp is shippeable.

## How to add a sub-phase to this file

1. Identify whether the work is core enablement (Phase 82)
   or framework foundation (Phase 83) — if it's a new
   primitive in core, Phase 82; if it's a library /
   template / reference / docs, Phase 83.
2. Append a new `#### 82.X — Title   ⬜` or
   `#### 83.X — Title   ⬜` section under the appropriate
   phase, following the existing template (Scope + Done
   criteria).
3. Update the **Phase X effort estimate** paragraph.
4. Update the status table at the top of this file.
5. Bump the corresponding `0/N` count in
   `proyecto/CLAUDE.md` table.
6. Add brainstorm/spec/plan/ejecutar tasks via TaskCreate.

## Documentation policy

- All Markdown content in this file is in English.
- Code identifiers in English.
- Spanish-language comments NOT permitted (project rule).
