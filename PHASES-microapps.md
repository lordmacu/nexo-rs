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
| 83 | Microapp framework foundation | 13 | 0/13 |

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
    wraps Phase 17 stores per channel
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

#### 82.11 — Transcripts admin RPC + `nexo/notify/transcript_appended` firehose   ⬜

Cross-app primitive. Microapps with conversation views
(meta-microapp's chat viewer, analytics-microapp's funnel,
audit/compliance microapp, training-data-extractor) need
access to transcripts. Today transcripts live in
`data/transcripts/<agent>/*.jsonl` filesystem with no
programmatic surface.

Scope:
- Admin RPC for backfill (idempotent):
  - `nexo/admin/transcripts/list_contacts { agent_id, since? }`
    → `[{ phone_number, display_name, session_id,
    first_seen, last_seen, message_count }]`
  - `nexo/admin/transcripts/read_session { agent_id,
    session_id, since?, limit? }` → `[{ role, body, sent_at,
    transcript_offset }]`
  - `nexo/admin/transcripts/search { agent_id, query, limit }`
    → `[{ snippet, session_id, sent_at }]` (uses
    Phase 10.7 FTS5 index)
- Live firehose via JSON-RPC notification (no `id`, no
  response):
  - `nexo/notify/transcript_appended { agent_id,
    session_id, sender_jid?, role, body, sent_at,
    transcript_offset }`
  - Emitted on every transcript append AFTER the redactor
    (Phase 10.4) runs — PII redacted before exposure
- Backfill model: progressive — default `last_seen_at >
  now - 30d`, `max_messages_per_session = 500`. Microapps
  needing more invoke `read_session` with explicit `since`.
- Capability gates:
  - `transcripts_subscribe` — to receive the live firehose
  - `transcripts_read` — to invoke admin RPC backfill
- Audit log: `microapp_admin_audit` row per pull — counts of
  contacts / messages / agent_id queried.
- Outbound flag: `messages` table mirrored by microapps
  carries `is_proactive: bool` distinguishing tick/drip from
  reply-to-inbound (auto-mapped when 82.3 outbound dispatch
  ships).
- INVENTORY toggle: `NEXO_MICROAPP_TRANSCRIPTS_ENABLED`.

Done criteria:
- Microapp with `transcripts_subscribe` receives
  `nexo/notify/transcript_appended` for each new message
  (already redacted).
- Microapp with `transcripts_read` can call list_contacts /
  read_session / search.
- Without capability → error -32004.
- Defense-in-depth: even with capability, the redactor
  (Phase 10.4) runs first; raw PII is never exposed.
- 8+ unit tests + 1 integration test (mock microapp consumes
  firehose + verifies redaction passthrough).

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

#### 82.13 — Operator chat takeover (per-conversation pause + manual reply + resume)   ⬜

Cross-app primitive. Operators sometimes need to step into a
single conversation to handle questions the agent cannot answer
or scenarios where a human voice is required. Today nexo runs
every inbound through the agent — there is no way to suspend
agent processing for one chat while keeping all other chats
active.

Used by any conversational microapp (sales / marketing / support
/ legal / medical / education / e-commerce) where human-handoff
per conversation is part of the workflow.

Scope:
- Per-conversation control state stored in the session manager:
  ```rust
  enum ConversationControlState {
      AgentActive,         // default — agent processes inbounds
      PausedByOperator {
          paused_at,
          operator_token_hash,
          reason: Option<String>,
      },
  }
  ```
  Indexed by `(agent_id, channel, account_id, contact_id)` tuple.
  Persisted in SQLite (idempotent migration via Phase 77.17).
- Inbound dispatch checks state:
  - `AgentActive` → normal flow (binding match → agent turn)
  - `PausedByOperator` → message logged to transcript with
    `role: "user"` flag but NO agent turn fired. Messages
    accumulate, do not get lost, do not trigger agent
    processing.
- New admin RPC methods (extend 82.10):
  - `nexo/admin/conversations/pause { agent_id, channel,
    account_id, contact_id, reason? }` → state to
    PausedByOperator.
  - `nexo/admin/conversations/resume { agent_id, channel,
    account_id, contact_id }` → state to AgentActive. On
    next inbound, agent turn fires with full context
    including operator's manual replies marked clearly +
    system prompt addendum: *"Operator was active from <X>
    to <Y>. Their replies are tagged `<operator>...</operator>`
    in the transcript. Continue the conversation."*
  - `nexo/admin/conversations/operator_reply { agent_id,
    channel, account_id, to, body, msg_kind?,
    attachments? }` → publishes via Phase 26 reply adapter;
    transcript entry tagged `role: "operator"` with
    `operator_token_hash`.
  - `nexo/admin/conversations/state { agent_id, channel,
    account_id, contact_id }` → returns current state.
- New capability gate: `operator_takeover` (subset of admin
  capabilities — declared granularly per microapp).
- Notification firehose:
  `nexo/notify/conversation_state_changed { agent_id,
  channel, account_id, contact_id, state,
  by_operator_token_hash?, at }`
- Transcript schema extension: `role` enum gains `Operator`
  variant (in addition to existing `User` / `Assistant` /
  `Tool`).
- Race-condition safety:
  - pause + resume idempotent (last-write-wins, audit logged)
  - operator_reply rejected (-32004 `not_paused`) if state
    is not `PausedByOperator`
  - concurrent pause from two operators: idempotent, latest
    token hash wins
- Auto-resolve hook for 82.14: pausing a conversation with a
  pending escalation auto-resolves the escalation to
  `OperatorTakeover`.

Done criteria:
- pause/resume cycle works end-to-end: operator pauses →
  contact's next inbounds logged but agent silent → operator
  types reply → reply lands in WhatsApp marked correctly →
  operator resumes → next contact inbound triggers agent turn
  with full context.
- Agent receives operator's reply in its system prompt
  context, knows it was human (visible via `<operator>` tags
  in transcript).
- Agent does not "double-respond" to a message the operator
  already replied to.
- Per-conversation isolation: pausing chat A does not affect
  chat B (same agent, different contacts).
- Audit log row per pause / resume / operator_reply.
- Transcripts persist `role: "operator"` correctly.
- 8+ unit tests + 1 e2e test simulating full takeover flow.

Out of scope:
- Multi-operator conflict UI (which operator owns the
  takeover; v0 first-come-first-serve).
- Operator typing indicators visible to the contact.
- Auto-resume timeout (could be future enhancement).
- Operator-to-operator handoff between humans.

#### 82.14 — Agent escalation: tool + notification + badge state   ⬜

Cross-app primitive. Agents sometimes encounter questions
outside their preloaded knowledge (tarifario, skill, persona
context). Today they fall back to natural-language deferral
("el asesor te lo confirma") — invisible to operators, no flag
raised. This subphase adds an explicit escalation channel:
agent calls a tool, core flags the conversation, operator UI
surfaces a badge, human follows up.

Used by any agent with bounded knowledge (sales / support /
marketing / specialist / domain-expert agents).

Scope:
- New built-in tool `escalate_to_human` (provider-agnostic,
  registered in core ToolRegistry):
  ```json
  {
    "name": "escalate_to_human",
    "description": "Flag this conversation for human operator follow-up when you cannot answer the customer's question with your preloaded knowledge.",
    "input_schema": {
      "type": "object",
      "required": ["question", "reason"],
      "properties": {
        "question": {"type": "string", "maxLength": 500},
        "reason": {"type": "string",
                   "enum": ["out_of_scope", "missing_data",
                            "needs_human_judgment",
                            "complaint", "other"]},
        "urgency": {"type": "string",
                    "enum": ["low", "normal", "high"],
                    "default": "normal"},
        "extra_context": {"type": "string", "maxLength": 1000}
      }
    }
  }
  ```
- Per-conversation escalation state:
  ```rust
  enum EscalationState {
      None,
      Pending {
          question, reason, urgency, requested_at,
          extra_context: Option<String>,
      },
      Resolved { resolved_at, by: ResolvedBy },
  }
  enum ResolvedBy {
      OperatorTakeover,                  // 82.13 pause auto-resolves
      OperatorDismissed { reason: String },
      AgentResolved,                     // rare — agent later found answer
  }
  ```
  Persisted in SQLite alongside 82.13's pause state.
- Tool dispatch sets state to `Pending` and emits firehose
  event:
  `nexo/notify/escalation_requested { agent_id, channel,
  account_id, contact_id, question, reason, urgency,
  extra_context?, requested_at }`
- New admin RPC methods (extend 82.10):
  - `nexo/admin/conversations/escalations/list { filter?:
    pending|resolved|all, agent_id?, limit? }`
  - `nexo/admin/conversations/escalations/resolve {
    agent_id, channel, account_id, contact_id, by:
    "dismissed"|"takeover", dismiss_reason? }`
- Auto-resolve trigger: when 82.13's `pause` is invoked on a
  conversation with pending escalation, the escalation
  auto-resolves with `ResolvedBy::OperatorTakeover` + emits
  `nexo/notify/escalation_resolved`.
- De-duplication: if `escalation_state == Pending` already,
  the agent's `escalate_to_human` call is a no-op (returns
  ok with `already_pending` flag, does not double-notify).
- Throttle: max 3 escalations per conversation per hour to
  prevent agent loops; further calls rejected with rate-limit
  error.
- Default behaviour post-escalation:
  - Agent continues responding with deferral language.
  - Conversation NOT auto-paused (operator can manually
    pause via 82.13 if takeover needed).
  - Configurable per agent: `escalation_auto_pause: bool`
    (default `false`). When `true`, agent auto-pauses after
    escalating; useful for legal / medical / sensitive bots.
- New capability gate: `escalation_subscribe` (subset of
  admin) for microapps that want the firehose.
- INVENTORY env toggle: `NEXO_AGENT_ESCALATION_ENABLED`
  global killswitch.

Done criteria:
- Agent calls `escalate_to_human(question, reason)` → state
  becomes `Pending` → notification fires.
- Microapp with `escalation_subscribe` receives notification →
  React UI shows badge on chat.
- Operator triggers 82.13 pause + operator_reply →
  escalation auto-resolves to `OperatorTakeover`.
- Operator dismisses without takeover → escalation resolves
  to `OperatorDismissed`.
- Agent re-calls `escalate_to_human` while `Pending` →
  no-op, single notification only.
- Throttle: 4th call within an hour → rate-limit error.
- Restart: pending escalations persist (state in SQLite).
- 8+ unit tests + 1 e2e test.
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
  Phase 82.10).
- `TranscriptFirehose` consumer for
  `nexo/notify/transcript_appended` (depends on 82.11).
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

Candidate microapps (operator picks):
- `agent-creator` — meta-microapp with React UI (uses the
  full Phase 82.10/82.11/82.12 stack: admin RPC, transcript
  firehose, HTTP server hosting). This is the most
  ambitious candidate; arguably the strongest validation.
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
- Microapp authored start-to-finish in ≤ 1 dev-day (or
  ≤ 3-4 dev-days if the candidate is `agent-creator`
  given the React UI scope).
- Uses ≥ 3 of the framework primitives (SDK helpers,
  compliance, hooks, persona config, admin RPC,
  transcripts firehose, HTTP server hosting).
- A retrospective lists every gap or rough edge that
  authoring surfaced — those become Phase 83 v2 candidates
  or post-Phase-83 follow-ups.

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

#### 83.13 — `microapp-ui-react` reusable component library (WhatsApp Web-inspired)   ⬜

Reusable visual layer above the 83.12 React UI infrastructure.
Where 83.12 covers auth + SPA serving + admin client +
WebSocket + build pipeline (the plumbing that lets a microapp
ship its own React UI), 83.13 ships the **components themselves**
— a WhatsApp Web-inspired layout with state-aware components
that consume the 82.10 + 82.11 + 82.13 + 82.14 schemas.

Used by any microapp with a conversational UI: meta-microapp,
support-deflect operator dashboard, marketing-saas lead inbox,
ventas-etb leads viewer, and any future microapp with a chat
interface.

The WhatsApp Web layout is chosen for operator familiarity —
operators interact with the UI for hours per day, and reusing
a mental model they already have reduces cognitive load and
training overhead. WhatsApp is the dominant inbound channel for
the v1 framework, so the visual analogy is direct.

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
Web-inspired component library + Storybook + theme system).
Total ~24-32 dev-days.

**Critical path**: 83.1 + 83.2 + 83.3 (core primitives) →
83.4 + 83.5 (libraries) → 83.6 + 83.7 (contract + templates)
→ 83.12 (React UI infrastructure) → 83.13 (component lib) →
83.8 (reference microapp) → 83.9 (cutover) + 83.10 (second
validation) → 83.11 (docs). 83.12 + 83.13 sit between 83.7
and 83.8/83.10 because the agent-creator validation in 83.10
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
14. **83.10** build agent-creator microapp as the
    validation case (reuses 83.12 + 83.13) — 1-3 days

Total: ~26-32 dev-days for the agent-creator meta-microapp
fully working with React UI (chat-list / conversation /
contact panel), live transcript view, escalation badges,
operator takeover with manual reply, and admin-driven CRUD
of agents/credentials/pairing/LLM keys.

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
