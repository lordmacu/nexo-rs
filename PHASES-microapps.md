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

### Phase 82 — Multi-tenant SaaS extension enablement + control plane   🔄  (2 / 14 + 82.2.b ✅)

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

#### 82.1 — Tool call enriched with `BindingContext`   ✅

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
- **Phase 80.9 MCP channels integration**: when the inbound
  arrived via an MCP channel server (slack / telegram / etc.
  via `notifications/nexo/channel`), `BindingContext` carries
  the MCP server name as a separate field so the extension
  can distinguish "telegram-binding via MCP slack server"
  from "telegram-binding via native Telegram plugin". Add
  `mcp_channel_source: Option<String>` to `BindingContext`
  alongside the existing `(channel, account_id, agent_id,
  session_id, binding_id)` tuple. Match the `goal_turns.source
  = "channel:slack"` audit column convention shipped with
  Phase 80.9. `None` for non-MCP-channel inbounds.

Done criteria:
- `BindingContext` round-trips from inbound channel through
  Phase 17 resolver to JSON-RPC tool call payload.
- MCP channel inbounds populate `mcp_channel_source` with
  the server name (e.g., "slack"); native channel inbounds
  leave it `None`.
- Existing extensions continue to function without change.
- 10+ unit tests + 1 integration test covering: fill from
  inbound binding, fill from delegation tool call, missing
  binding (None), serialization shape stability, all four
  binding types (WA/TG/Email/web), Phase 17 isolation,
  Phase 80.9 MCP channel source preservation.
- Docs page on extension contract documents the new field.

#### 82.2 — Webhook receiver wire-up (= existing 80.12.b)   ✅

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

#### 82.2.b — Extract `nexo-tool-meta` + Tier A publish-readiness   ✅

Follow-up to 82.2. Extracts the wire-shape types
(`BindingContext`, `WebhookEnvelope`, `_meta` builder/parser
helpers) into a slim, framework-free crate so any third-party
Rust microapp can `cargo add nexo-tool-meta` without pulling
the agent runtime. Ships pre-publish posture for the Tier A
surface (READMEs, `[package.metadata.docs.rs]`, doctests,
forward-compat policy).

Scope:
- New crate `crates/tool-meta/` (~600 LOC + tests + doctests)
  with the four-dependency wire-only surface (serde +
  serde_json + uuid + chrono).
- Re-exports from `nexo-core::agent::context::BindingContext`
  and `nexo-webhook-receiver::dispatcher::{WebhookEnvelope,
  format_webhook_source}` keep every internal call-site
  working unchanged.
- Microapp-side validation: `agent-creator-microapp`
  consumes `nexo-tool-meta` via path-dep and the
  `parse_binding_via_sdk_helper` test locks the public surface.
- Operator docs `docs/src/microapps/rust.md` document the
  Tier A surface and the SDK's forward-compat policy.

Deferred to follow-ups:
- Tier A `#![deny(missing_docs)]` + `#[non_exhaustive]` sweep
  on `nexo-resilience`, `nexo-driver-permission`,
  `nexo-webhook-receiver`, `nexo-webhook-server`.
  - **82.2.b.b ✅** — sweep landed for `nexo-resilience`,
    `nexo-webhook-receiver`, `nexo-webhook-server`. Refined the
    rule: `#[non_exhaustive]` is mandatory on operator-facing
    diagnostic enums (`RejectReason`, `EvictionReason`,
    `WebhookRouterError`) but rejected on caller-populated
    config / wire-shape / utility-error wrappers. `scripts/
    tier-a-doc-check.sh` ships as the manual smoke gate.
    `nexo-driver-permission` deferred to 82.2.b.b.b due to
    surface size (68 pub items, 16 modules) + a pre-existing
    failing test unrelated to this sweep.
  - **82.2.b.b.b ⬜** — driver-permission sweep with the
    refined ruleset; needs the pre-existing test fix first.
- CI tier-a-gate workflow (build / test / doc / smoke per
  crate matrix).
- Actual `cargo publish` (operator-gated, post-review).
- `nexo-test-fixtures` extraction (Tier B).

#### 82.3 — Outbound dispatch from extension (stdio path)   🔄  (foundations ✅, runtime + wire + boot deferred)

**Foundations shipped (82.3 MVP):**
- `nexo-tool-meta::format_dispatch_source(extension, channel,
  account_id) -> "dispatch:<ext>:<chan>:<acct>"` Phase 72
  turn-log marker. 2 tests.
- `nexo-extensions::manifest::OutboundBindings` newtype +
  `[outbound_bindings]` TOML schema. `allows(channel,
  account_id)` lookup helper + `validate()` rejecting empty
  segments. `ExtensionManifest.outbound_bindings` field with
  `#[serde(default)]`. 4 tests.
- Operator can declare the outbound allowlist today even though
  the runtime gate isn't wired yet (so operators stage YAML
  before `82.3.b` ships).

**Deferred to 82.3.b (runtime + wire + boot):**
- `nexo-extensions::runtime::wire::decode_message` distinguishing
  request vs response vs notification on stdout.
- `ExtensionRequestHandler` trait + `MethodNotFound` typed
  error + `StdioRuntime::with_request_handler` builder.
- Reader-loop request branch dispatching `nexo/dispatch` to the
  attached handler.
- `nexo-core::agent::dispatch_from_extension::DispatchFromExtensionHandler`
  with auth gate (channel/account_id allowlist) + per-extension
  rate limit + broker publish + audit row.
- Boot wiring in `src/main.rs` (per-extension handler attached
  to each `StdioRuntime`).
- E2E test with mock channel adapter.
- Operator docs `docs/src/ops/extension-dispatch.md` +
  admin-ui tech-debt entry.

**Deferred to 82.3.c**: idempotency dedup (field accepted, not
yet enforced).
**Deferred to 82.3.d**: hot-reload of `outbound_bindings`.

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

#### 82.4 — NATS event subject → agent turn binding   ✅

**Foundations shipped (82.4 MVP):**
- `nexo-tool-meta::template` mustache-lite renderer
  (`{{path.to.field}}` → JSON value, `<missing>` placeholder
  for missing/null/non-leaf paths). 11 tests verde.
- `nexo-tool-meta::event_source::EventSourceMeta` typed
  wire-shape struct + `format_event_subscriber_source(id) ->
  "event:<id>"` Phase 72 turn-log marker. 4 tests verde.
- `BindingContext.event_source: Option<EventSourceMeta>`
  field re-exported through `nexo-core`. Old microapps ignore
  the additive key via serde tolerance.
- `nexo-config::types::event_subscriber::EventSubscriberBinding`
  schema + `SynthesisMode { Synthesize, Tick, Off }` +
  `OverflowPolicy { DropOldest, DropNewest }` enums + 6-variant
  `EventSubscriberConfigError` typed enum + `validate()` +
  `check_event_subscribers_unique()` + `pattern_loops_to_inbound()`
  defensive guard against `plugin.inbound.>` patterns. 10
  schema tests verde.
- `AgentConfig.event_subscribers: Vec<EventSubscriberBinding>`
  field with `#[serde(default)]` and ~25-site workspace
  sweep. Backward compat with every existing fixture.

**Runtime shipped (82.4.b ✅):**
- `crates/core/src/agent/event_subscriber.rs` runtime task with
  `Arc<Mutex<VecDeque<Event>>>` + `Notify` bounded buffer +
  `Arc<Semaphore>` concurrency cap + per-event template render
  + re-publish to `plugin.inbound.event.<id>`.
- `EventSubscriberError` `#[non_exhaustive]` typed error.
- `build_synthesised_payload` builder + `extract_nexo_event_source`
  resolver helper + `synthesize_event_inbound_bindings` auto-synth.
- 20 unit tests verde — synthesise / tick / off / DropOldest /
  DropNewest / cancel-token shutdown / payload shape /
  resolver helper / auto-synth idempotency.

**Boot wiring shipped (82.4.b.b ✅):**
- `src/main.rs` boot supervisor wires `EventSubscriber` tasks
  under shared `event_subscribers_shutdown` cancel token, with
  per-binding validation (skip on error, log + keep daemon up).
- Auto-synth mutation of `agent_cfg.inbound_bindings` at
  YAML-load time so the existing inbound resolver matches
  `plugin.inbound.event.<id>` re-publishes without operator
  having to declare the binding twice.
- Resolver populator hook in `runtime.rs:761` reads
  `_nexo_event_source` extension field and chains
  `with_event_source(meta)` on `plugin.inbound.event.*` topics
  (gated on `binding.is_some()` to avoid hot-path debug spam).
- Producer loop self-event guard — runtime drops events whose
  `topic` matches the binding's own re-publish topic, even if
  schema validate was bypassed.
- `id` reserved-character validation rejects `.`, `*`, `>`,
  whitespace at boot.
- Operator docs `docs/src/ops/events.md` covers schema, modes,
  template syntax, overflow policies, auto-synth behaviour,
  hot-reload limitation, worked GitHub example.
- admin-ui tech-debt entry registered.

**Deferred to 82.4.c**: hot-reload `reevaluate` integration.
**Deferred to 82.4.d**: operator CLI `nexo events list/test`.
**Deferred to 82.4.e**: post-receipt JSON filter expressions.
**Deferred to 82.4.f**: JetStream durable subscribers /
replay-on-restart.

The schema + types + helpers shipped here unblock anyone
prototyping the runtime task: the wire shape is frozen, the
validation rules are codified, and the `BindingContext`
field is in place ready for the runtime to populate.

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

#### 82.5 — Inbound message metadata in `_meta.nexo.inbound`   ✅  (shipped 2026-05-01)

Provider-agnostic per-turn metadata stamped as a peer of
`_meta.nexo.binding` so microapps see who sent what, when,
replying to which earlier message, with media or not — without
having to re-derive it from each tool's payload.

**Shape (`crates/tool-meta/src/inbound.rs`):**
- `InboundKind` 3-way enum: `external_user | internal_system |
  inter_session`.
- `InboundMessageMeta { kind, sender_id, msg_id, inbound_ts,
  reply_to_msg_id, has_media, origin_session_id }`. All fields
  except `kind` optional. `#[non_exhaustive]` for forward-compat.

**Producers wired:**
- WhatsApp inbound (provider-agnostic helper
  `runtime::extract_inbound_meta` reads `from`/`msg_id`/
  `timestamp`/`reply_to` from the flattened plugin payload —
  same shape extends to telegram/email/future channels day-1).
- Event-subscriber binding (yaml-declared
  `inbound_kind: external_user | internal_system`, default
  `external_user`). Synthesizer stamps it on the synthesised
  payload; intake helper honors it.
- Webhook receiver (transitive — publishes to operator-declared
  topic, EventSubscriberBinding subscribes and republishes
  through the same meta path).
- Delegation receive (`InterSession`, `origin_session_id =
  msg.correlation_id`).
- Proactive ticks + email-followup ticks (`InternalSystem`).

**Architecture detail:** AgentContext is per-session-stable, but
inbound meta is per-turn. Solution: `InboundMessage` carries the
meta as `Option<InboundMessageMeta>` (built at intake), and the
per-turn dispatch loop layers it on the cloned `turn_ctx` via
`AgentContext::with_inbound_meta` before invoking tools.
`ctx.build_meta_value()` then stamps the *current* turn's meta on
outgoing extension/MCP tool calls.

**SDK integration (`nexo-microapp-sdk`):**
`ToolCtx::inbound() -> Option<&InboundMessageMeta>` and
`HookCtx::inbound()` accessors. Test harness gains
`call_tool_with_inbound` / `call_tool_with_binding_and_inbound`.

**Tests:** 11 unit tests in `nexo-tool-meta` (round-trip, strict
unknown-kind reject, provider-agnostic id strings) + 4 SDK tests
+ 7 core runtime tests (helper extraction, kind override,
delegation/heartbeat/internal-system shapes).

**Deferred follow-ups:**
- Telegram / email / browser / google plugin producers — framework
  shape supports them; wire-up when a microapp future requires.
- PII redact env var (`NEXO_PII_REDACT=full|tail4|none`) for
  `sender_id` in tracing logs.
- `chat_type` (private/group/channel) field — defer until demand.
- 82.3.b synergy: `OutboundDispatcher::send_text` should read
  `ctx.inbound().reply_to_msg_id` for native quote/reply
  threading when the runtime ships.

#### 82.6 — Per-extension `state_root` convention + CLI   ✅  (shipped 2026-05-01)

Two atomic steps:
- **82.6.1** — `nexo_extensions::state` module:
  `state_dir_for(id)` (pure path), `ensure_state_dir(id)`
  (idempotent mkdir), `extension_state_root()`,
  `EXTENSION_STATE_ROOT_ENV` constant for the env passthrough.
  Layout: `$NEXO_HOME/extensions/<id>/state/`. 4 unit tests
  covering NEXO_HOME override, idempotent creation, pure path
  computation, HOME fallback. Tests serialise via a
  `Mutex` since `NEXO_HOME` is a process-global.
- **82.6.2** — `nexo ext state-dir <id> [--ensure]` CLI
  subcommand. `--ensure` flips to `ensure_state_dir`;
  default is pure path print. Smoke-tested against
  `NEXO_HOME=/tmp/...` — directory created on `--ensure`,
  not created on the bare form.

**Tests:** 4 unit + 1 manual smoke. mdbook +
`docs/src/extensions/state-management.md` shipped with the
backup procedure (`sqlite3 .backup`).

**Deferred:**
- `initialize` env block injection of
  `NEXO_EXTENSION_STATE_ROOT` for the spawned extension's
  process environment. Constant pinned today; the daemon-side
  call lands in the same operator wire-up commit as the rest
  of 82.x's deferreds.


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

#### 82.7 — Per-binding tool rate-limit (multi-tenant fair-use)   ✅  (shipped 2026-05-01)

Same agent + same tool, two bindings → two independent buckets
with independent caps. Free-tier WhatsApp binding hits 10/min
on `marketing_send_drip`; enterprise binding stays unlimited;
neither starves the other.

**Shipped:**
- Config-side: `InboundBinding.tool_rate_limits:
  Option<ToolRateLimitsConfig>` field (yaml-readable). Driver +
  fully-replace semantic — per-binding overrides do NOT fall
  through to global. New `essential_deny_on_miss: bool` opt-in
  on `ToolRateLimitSpec` adapts upstream-CLI fail-open default
  + ESSENTIAL deny pattern to LRU eviction context.
- Discovery cleanup: `ToolRateLimitConfig` was duplicated
  between `nexo-config` (yaml-readable) and `nexo-core`
  (runtime). Unified — `nexo-core::agent::rate_limit` now
  re-exports + aliases the `nexo-config` types. The translation
  block in `src/main.rs` collapsed from ~16 lines to a single
  `Arc::new(ToolRateLimiter::new(rl_cfg))` pass-through.
- Runtime: `ToolRateLimiter::try_acquire_with_binding(agent,
  binding_id, tool, per_binding_override)` — bucket key triple
  `(agent, Option<binding_id>, tool)`. Legacy
  `try_acquire(agent, tool)` keeps working as a thin wrapper.
- LRU eviction at `DEFAULT_MAX_BUCKETS = 10_000` (mirrors
  `SenderRateLimiter`). `with_cap` constructor for tests.
- `essential_deny_on_miss` plumbed end-to-end: evicted
  essentials stamp the key into `evicted_essential` set, next
  acquire denies once before reallocating fresh bucket.
- Admin RPC support: `drop_buckets_for_agent(agent: &str)` so
  Phase 82.10 delete-agent path won't leak buckets.
- Wire-up: `LlmBehavior::on_message_control` consults
  `ctx.binding.binding_id` + `ctx.effective.tool_rate_limits`
  via `try_acquire_with_binding`. On denial, emits Phase 72
  marker `rate_limited:tool=X,binding=Y,rps=Z` (helper
  `nexo_tool_meta::format_rate_limit_hit`).

**Tests:** 13 new (4 config yaml round-trip + 2 effective
resolver + 7 limiter behaviour + 2 marker format). Workspace
build verde excluding the unrelated `nexo-memory-snapshot`
in-progress crate.

**Docs:** `docs/src/ops/per-binding-rate-limits.md` — yaml
example, free/pro/enterprise scenario, glob semantics, LRU
behaviour, audit marker format, hot-reload caveats.

**Deferred follow-ups:**
- 82.7.g — Telemetry side-channel (`RateLimitTelemetry
  { utilization, resets_in, recently_denied }`) consultable via
  admin RPC (Phase 82.10) + statusline. Adapts the upstream CLI
  `claudeAiLimits` shape.
- 82.7.h — Combined `(agent, binding_id, sender_id, tool)`
  quadruple cap leveraging 82.5's `inbound.sender_id`.
- 82.7.i — Yaml validator detects overlapping per-binding
  patterns at boot.
- 82.7.j — Circuit-breaker overlay on top of rate-limit:
  cooldown after N consecutive `rate_limit` hits (mirrors the
  upstream CLI auth-profiles cooldown pattern).

#### 82.8 — Multi-tenant audit log filter   ✅  (shipped 2026-05-01)

Single atomic step:
- `goal_turns.account_id` column added via idempotent ALTER
  (same pattern as the 80.9.h `source` migration). Index on
  `(account_id, recorded_at DESC)` for tenant-scoped tail
  queries.
- `TurnRecord.account_id: Option<String>` field threaded
  through the insert UPSERT + every `SELECT`/tuple in
  `SqliteTurnLogStore`. Legacy callers (event_forwarder,
  away_summary, agent_query tests) default to `None`.
- New `SqliteTurnLogStore::tail_for_account(account_id,
  since, limit)`. Filters strictly by `account_id = ?`;
  legacy `NULL` rows are excluded so operator/admin tails
  remain the only path that sees them. Cross-tenant probes
  return an empty list (no error) — defense in depth
  against existence oracles.

**Tests:** 5 new (round-trip, idempotent on replay,
tenant filter, unknown tenant returns empty, legacy NULL
rows excluded from tenant view but visible operator-side).
14 turn_log total verde.

**Deferred:**
- BindingContext (Phase 82.1) → `account_id` plumbing in
  `event_forwarder` so live writes carry the tenant id.
  Today the field is store-correct but the writer always
  emits `None`. Same boot-order refactor as the rest of
  82.x's deferreds; folded with main.rs operator wire-up.


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

#### 82.9 — Reference template + docs   ✅  (shipped 2026-05-01)

**Shipped:** `docs/src/extensions/multi-tenant-saas.md` — Phase 82
walkthrough connecting all primitives (BindingContext, webhook
receiver, NATS event subscribers, tool rate-limits, state dir,
multi-tenant audit filter, admin RPC, firehose, HTTP server,
operator takeover, escalations). Linked from SUMMARY.md.

INVENTORY for new env toggles (82.10/82.11/82.12) was registered
in their respective sub-phase commits.

**Deferred** (logged in FOLLOWUPS.md):
- `extensions/template-saas/` repo scaffold — out-of-tree
  `agent-creator` (Phase 83.10) is the working reference;
  in-tree scaffold lands when 83.x microapp build starts.
- `admin-ui/PHASES.md` tech-debt entries — defer until
  admin-ui repo touch.

#### 82.10 — `nexo/admin/*` bidirectional RPC + granular capability gates + admin domain methods   ✅  (shipped 2026-05-01)

**Shipped (steps a-g, 7 atomic commits):**

Framework:
- SDK `AdminClient` + `AdminSender` trait (gated by `admin`
  cargo feature) — bidirectional JSON-RPC over existing stdio
  with `app:<uuid>` ID prefix correlation.
- `AdminError` typed enum mapping JSON-RPC error codes
  (-32004/-32602/-32601/-32603/transport).
- Core `AdminRpcDispatcher` with method routing + capability
  gate + audit log per-call.
- `CapabilitySet` lock-free runtime check + boot-time validator
  diffing plugin.toml declarations vs extensions.yaml grants
  (required → fail-fast, optional → warn, orphan → warn).
- `AdminAuditWriter` trait + `InMemoryAuditWriter` default;
  SQLite writer + retention sweep deferred to 82.10.h.

5 admin domains (17 methods + reload):
- `agents` — list / get / upsert / delete via `YamlPatcher` trait
  abstraction (avoids cycle vs nexo-setup yaml_patch).
- `credentials` — list / register / revoke many-to-many (1
  channel cred → N agents).
- `pairing` — start / status / cancel + `nexo/notify/pairing_status_changed`
  notification stream wire shape.
- `llm_providers` — list / upsert / delete; upsert validates
  api_key_env exists in process env, delete refuses when any
  agent uses the provider.
- `channels` — list / approve / revoke / doctor for Phase 80.9
  MCP-channel servers in `agents.yaml.<id>.channels.approved`.

INVENTORY env toggles (5 entries) — per-domain operator-global
kill switches: `NEXO_MICROAPP_ADMIN_AGENTS_ENABLED`,
`_CREDENTIALS_ENABLED`, `_PAIRING_ENABLED`, `_LLM_KEYS_ENABLED`,
`_CHANNELS_ENABLED`. Capability grants are the per-microapp
check; INVENTORY is the operator-global hard disable.

**Tests:** 60 admin_rpc + 14 tool-meta admin = **74 verde**.
Workspace excl. `nexo-memory-snapshot` (parallel workstream).

**Docs:** `docs/src/microapps/admin-rpc.md` — operator +
microapp-developer reference with wire shape, layered grant
model, all 18 endpoints, many-to-many credentials flow, async
pairing diagram, INVENTORY toggles, SDK example.

#### 82.10.h — SQLite audit + production adapters   ✅  (shipped 2026-05-01)

Three steps shipped, two deferred to 82.10.h.b:

- **82.10.h.1** — `SqliteAdminAuditWriter` (`crates/core/src/agent/admin_rpc/audit_sqlite.rs`).
  Idempotent inline DDL + WAL + 2 indices on `(microapp_id,
  started_at_ms DESC)` and `(method, started_at_ms DESC)`.
  Boot-time `sweep_retention(retention_days, max_rows)` enforces
  age + cap limits via `NEXO_MICROAPP_ADMIN_AUDIT_RETENTION_DAYS`
  / `_MAX_ROWS` toggles.
- **82.10.h.2** — library-level `tail(&AuditTailFilter)` query +
  `format_rows_as_table` / `format_rows_as_json` helpers. CLI
  subcommand wire-up deferred to 82.10.h.b alongside the rest of
  main.rs work.
- **82.10.h.3** — three production adapters in
  `nexo_setup::admin_adapters`: `AgentsYamlPatcher` (wraps
  `yaml_patch` for `agents.yaml`), `LlmYamlPatcherFs` (mapping-
  style helpers for `llm.yaml.providers.<id>`),
  `FilesystemCredentialStore` (atomic writes under
  `secrets/<channel>/[<instance>/]payload.json`). 5 new helpers in
  `crate::yaml_patch` (`remove_agent_block`, 4× LLM provider
  helpers).

**Tests:** 15 audit + 7 yaml_patch helpers + 12 adapters =
**34 verde**.

#### 82.10.h.b — pairing adapters + main.rs wire path + audit-tail CLI   ✅  (shipped 2026-05-01)

Five steps shipped:
- **82.10.h.b.1** — `InMemoryPairingChallengeStore` (DashMap +
  TTL, mirrors OpenClaw QR-login pattern). 6 tests.
- **82.10.h.b.2** — `StdioPairingNotifier` + reusable
  `json_rpc_notification(method, params)` helper. 3 tests.
- **82.10.h.b.3** — `AdminRouter` trait in `nexo-extensions`
  + `DispatcherAdminRouter` impl in `nexo-core` +
  `reader_task` `app:`-prefix routing in
  `nexo_extensions::runtime::stdio` (id is now matched by
  shape: numeric → existing pending map, `app:*` → router,
  other strings → warn-drop). Inner branch extracted to
  `process_inbound_line` for direct unit-testing.
  4 + 4 = 8 tests.
- **82.10.h.b.4** — `nexo microapp admin audit tail` CLI
  subcommand. Maps every flag 1:1 to `AuditTailFilter`,
  default `--db` resolves to
  `nexo_state_dir().join("admin_audit.db")`. 3 integration
  tests via `Command::new(env!("CARGO_BIN_EXE_nexo"))`.
- **82.10.h.b.5** — `nexo_setup::admin_bootstrap` module +
  `StdioRuntime::outbox_sender()` accessor +
  `DeferredAdminOutboundWriter` (OnceLock-backed). main.rs
  `run_extension_discovery` accepts an
  `Option<&AdminRpcBootstrap>` and threads the per-microapp
  router through `StdioSpawnOptions::admin_router`,
  post-spawn binding the deferred writer to
  `runtime.outbox_sender()`. Caller passes `None` for now —
  activating admin RPC is a one-line operator change once
  the manifest source-of-truth refactor lands. 3 bootstrap
  unit tests.

**Tests:** 6 + 3 + 8 + 3 + 3 = **23 verde**.

**Deferred (still pending):**
- Pairing notifier wire-up — the notifier is built but not
  fed into the dispatcher because the chicken-and-egg between
  `outbox_tx` (created inside `spawn_with`) and the
  pre-spawn `with_pairing_domain(_, Some(notifier))` call
  needs a separate notification queue. v1 microapps poll
  `pairing/status` instead.
- main.rs operator wire-up — passing `Some(&bootstrap)`
  instead of `None` to `run_extension_discovery`. Needs a
  refactor to surface per-extension `[capabilities.admin]`
  declarations from a single source of truth (today re-parsed
  inside discovery).

**Deferred to 82.10.i (out-of-scope follow-up):**
- Interactive operator approval (`ask` behavior). v1 is binary
  yaml-static grant.
- Per-method rate-limit on admin calls.
- Telegram / email / other channel domains beyond WhatsApp v1
  UI scope.
- Streaming admin notifications beyond `pairing_status_changed`
  (covered in spirit by 82.11 firehose).
- Capability inheritance / group-grants / role-templates.

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
  - `nexo/admin/channels/{list, approve, revoke, doctor}` —
    **Phase 80.9 MCP channels CRUD**: list configured MCP
    channel servers from `agents.yaml.<id>.channels.approved`,
    approve a new MCP server (writes the entry via Phase 69
    `yaml_patch`), revoke an existing one, run the static
    half of `nexo channel doctor` against the current YAML
    + return the gate verdict per (agent, binding, server)
    triple. Distinct from `nexo/admin/credentials/*` because
    MCP channels are MCP-server adapters (slack / telegram /
    iMessage relay) declared in YAML, not Phase 17
    credentials. Capability gate granular `channels_crud`.
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

#### 82.11 — Agent event firehose + admin RPC (transcript-shaped events as one variant)   ✅  (shipped 2026-05-01)

Six steps shipped:
- **82.11.1** — `nexo_tool_meta::admin::agent_events` wire
  shapes (`AgentEventKind` enum `#[non_exhaustive]` with
  `TranscriptAppended` variant, list/read/search params +
  responses, `AGENT_EVENT_NOTIFY_METHOD` constant). 4 tests.
- **82.11.2** — `agent_events` admin domain handlers in
  `nexo-core` + `TranscriptReader` async trait + capability
  routing (`transcripts_read`) + `with_agent_events_domain`
  builder. 7 tests.
- **82.11.3** — `TranscriptReaderFs` adapter in
  `nexo-setup::admin_adapters` wrapping `TranscriptWriter` +
  `TranscriptsIndex`. Entry-only `seq` enumeration so
  backfill + live agree on values. 4 tests.
- **82.11.4** — `nexo_core::agent::agent_events` (new module):
  `AgentEventEmitter` trait + `BroadcastAgentEventEmitter`
  (`tokio::sync::broadcast`, default cap 256) +
  `NoopAgentEventEmitter`. `TranscriptWriter` gains
  `event_emitter` field + `with_emitter` builder; emit hook
  in `append_entry` post-redaction tracks per-session
  monotonic `seq` via `DashMap<Uuid, AtomicU64>`. 5 tests.
- **82.11.5** — INVENTORY env toggle
  `NEXO_MICROAPP_AGENT_EVENTS_ENABLED` (default `1`).
  `AdminRpcBootstrap` gains `transcript_reader` input field +
  `event_emitter()` accessor + per-microapp subscribe loop
  (`firehose_subscriber_loop`) that drains the broadcast and
  forwards filtered frames as `nexo/notify/agent_event`
  through the deferred outbound writer.
  `transcripts_subscribe` / `agent_events_subscribe_all`
  capabilities gate which microapps spawn a subscribe task.
  `RecvError::Lagged` → single `warn` + re-sync; microapps
  that miss frames re-issue `agent_events/read`. 4 tests.
- **82.11.6** — integration test (`tests/agent_events_firehose.rs`)
  spawns a real bootstrap + `TranscriptWriter` with redactor,
  appends a PII-bearing entry, asserts the microapp's
  outbound queue receives a JSON-RPC notification with
  `kind=transcript_appended` + redacted body + monotonic
  `seq`. Second test asserts a microapp without
  `transcripts_subscribe` receives nothing. 2 tests.

**Tests:** 4 + 7 + 4 + 5 + 4 + 2 = **26 verde**.

**Deferred (still pending):**
- Pairing notifier wire — same chicken-and-egg as 82.10.h.b;
  carried forward in FOLLOWUPS.md.
- Operator wire-up `None → Some(&bootstrap)` in main.rs —
  same as 82.10.h.b deferred.
- `BroadcastAgentEventEmitter` is in-process; future
  multi-host deployments will need a NATS bridge variant
  behind the same `AgentEventEmitter` trait.

---

#### 82.11 — original scope notes (kept for context)

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
          source: Option<String>,  // Phase 80.9 — matches
                                   // goal_turns.source column,
                                   // e.g., "channel:slack" for
                                   // MCP-channel inbounds, None
                                   // for native channel inbounds.
                                   // Lets firehose subscribers
                                   // filter by inbound origin.
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

#### 82.12 — Plugin.toml `[capabilities.http_server]` + bind 127.0.0.1 + token auth + boot supervisor   ✅  (shipped 2026-05-01)

Five steps shipped:
- **82.12.1** — `HttpServerCapability` in `nexo-plugin-manifest`
  (`port` + `bind` default 127.0.0.1 + `token_env` +
  `health_path` default `/healthz`) and `TokenRotated` +
  `TOKEN_ROTATED_NOTIFY_METHOD` literal in
  `nexo-tool-meta::http_server`. 6 tests.
- **82.12.2** — `HttpServerSupervisor::probe(decl)` + 60 s
  `spawn_monitor_loop(decl)` in `nexo-setup::http_supervisor`.
  Typed `HealthProbeError::{Timeout, BadStatus}` so boot
  wiring can distinguish hung microapps from misbehaving
  ones; reqwest client built with 2 s per-request timeout.
  6 tests.
- **82.12.3** — `ExtensionEntry.allow_external_bind: bool`
  field + `AdminBootstrapInputs::http_server_capabilities` +
  bootstrap validates the bind policy before spawn.
  `AdminBootstrapError::ExternalBindNotAllowed { microapp_id,
  bind }` surfaces operator misconfigurations. 3 tests.
- **82.12.4** — `NEXO_MICROAPP_HTTP_SERVERS_ENABLED` INVENTORY
  toggle (`Risk::High`) + `token_hash(token)` helper
  (sha256-hex truncated to 16 chars) for the
  `TokenRotated.old_hash` field. 1 test.
- **82.12.5** — docs (`admin-rpc.md` "HTTP server capability"
  section: declaration, supervisor, bind policy, token
  rotation, INVENTORY) + PHASES ✅ + FOLLOWUPS deferreds.

**Tests:** 6 + 6 + 3 + 1 = **16 verde**.

**Deferred:**
- main.rs operator wire-up: thread the supervisor + the
  `http_server_capabilities` map into `AdminRpcBootstrap`.
  Same boot-order refactor as 82.10.h.b / 82.11 — folded
  into the shared follow-up.
- Token rotation emit site: the daemon needs a Phase 18
  reload-coordinator hook that detects `<token_env>` change
  and calls `dispatcher.notify(...token_rotated...)`. v0
  framework has the wire shape + helper; the trigger
  belongs to the boot wire-up follow-up.


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

#### 82.13 — Agent processing pause + operator intervention (chat-takeover is one variant)   ✅  (shipped 2026-05-01)

Four steps shipped (v0 = wire shapes + store + admin RPC
routing; inbound-dispatcher hook + reply adapter + transcript
Operator role deferred to 82.13.b):

- **82.13.1** — `nexo_tool_meta::admin::processing` wire
  shapes (`ProcessingScope` `#[non_exhaustive]`,
  `InterventionAction`, `ProcessingControlState`,
  pause/resume/intervention/state params + responses,
  `PROCESSING_STATE_CHANGED_NOTIFY_METHOD` literal).
  4 round-trip tests.
- **82.13.2** — `ProcessingControlStore` async trait + 4
  admin RPC handlers (`pause` / `resume` / `intervention` /
  `state`) + dispatcher routing on the
  `operator_intervention` capability.
  6 handler tests + 5 store tests = 11.
- **82.13.3** — `InMemoryProcessingControlStore` (DashMap,
  drop-row-on-AgentActive semantics so the implicit default
  doesn't hold slots). 5 tests (folded into Step 2 commit).
- **82.13.4** — docs (`admin-rpc.md` "Operator processing
  pause + intervention" section: wire shapes, methods, v0
  surface, notification literal) + PHASES ✅ + FOLLOWUPS
  deferreds.

**Tests:** 4 + 6 + 5 = **15 verde**.

**Deferred to 82.13.b**:
- Inbound dispatcher hook: skip the agent turn for paused
  conversations + replay the queued inbounds via 82.11
  firehose. Same boot-order refactor as 82.10.h.b /
  82.11 / 82.12 — all microapp-side wire-ups land together.
- `InterventionAction::Reply` outbound: route through the
  Phase 26 reply adapter, transcript-stamp with `role:
  Operator`, emit a `nexo/notify/processing_state_changed`
  frame on the firehose.
- Auto-resolve hook for 82.14 (escalation): pausing a scope
  with a pending escalation auto-flips the escalation to
  `OperatorTakeover`.
- SQLite-backed durable store — v0 is in-memory, daemon
  restart drops every paused state. Not a scope drop:
  `ProcessingControlStore` trait already exists, the new
  impl drops in alongside the in-memory variant.


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
          mcp_channel_source: Option<String>,
                                  // Phase 80.9 — populated when
                                  // the conversation arrived via
                                  // an MCP channel server
                                  // (e.g., "slack", "telegram")
                                  // so operators can pause
                                  // conversations sourced from
                                  // a specific MCP server
                                  // separately from native
                                  // channel conversations on the
                                  // same binding. None for
                                  // native-channel inbounds.
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

#### 82.14 — Agent attention request: `escalate_to_human` tool + state + notification (chat-question is one example)   ✅  (shipped 2026-05-01)

Three steps shipped (read + resolve admin surface + auto-
resolve hook on 82.13 pause; the `escalate_to_human` built-in
tool that PRODUCES Pending rows is deferred to 82.14.b):

- **82.14.1** — `nexo_tool_meta::admin::escalations` wire
  shapes (`EscalationReason` closed enum,
  `EscalationUrgency`, `ResolvedBy` `#[non_exhaustive]`,
  `EscalationState` `#[non_exhaustive]` with `Pending`
  carrying `BTreeMap<String, Value>` context for
  per-agent-shape details, list/resolve params + responses,
  `escalation_requested` / `escalation_resolved` notify-kind
  literals). 4 round-trip tests.
- **82.14.2** — `EscalationStore` async trait + 2 admin RPC
  handlers (`list`, `resolve`) + dispatcher routing on
  `escalations_read` / `escalations_resolve` (granular caps
  for read-only dashboards) + `auto_resolve_on_pause` cross-
  cut hook called from the `processing/pause` arm.
  `InMemoryEscalationStore` (DashMap, newest-first list
  ordering by `requested_at_ms` / `resolved_at_ms`).
  7 handler tests + 5 store tests = 12.
- **82.14.3** — docs (`admin-rpc.md` "Agent escalations"
  section: wire shapes, methods, auto-resolve semantics,
  notification literals) + PHASES ✅ + FOLLOWUPS
  deferreds.

**Tests:** 4 + 12 = **16 verde**.

**Deferred to 82.14.b**:
- `escalate_to_human` built-in tool registration in core
  ToolRegistry. Production needs the BindingContext (82.1)
  + the agent's current scope context (chat: contact_id;
  batch: current job_id) to derive `ProcessingScope`
  automatically — the agent passes only `summary` /
  `reason` / `urgency` / `context`, framework fills in
  scope from dispatch context.
- `EscalationRequested` / `EscalationResolved` event
  variants on the 82.11 firehose. The notify-kind literals
  are pinned today; emit site lands with the boot wire-up
  refactor.
- Throttle: max 3 escalations per scope per hour to prevent
  agent loops. Token-bucket from Phase 82.7 reused.
- SQLite-backed durable store — v0 is in-memory; daemon
  restart drops every escalation row. Trait + handler are
  store-agnostic so the new impl drops in alongside
  `InMemoryEscalationStore`.


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

#### 83.1 — Per-agent extension config propagation   ✅  (shipped 2026-05-02 — JSON-RPC propagation deferred to 83.1.b)

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

#### 83.2 — Extension-contributed skills   ✅  (shipped 2026-05-02 — SkillLoader merge deferred to 83.2.b)

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

#### 83.3 — Hook interceptor (vote-to-block)   ✅  (shipped 2026-05-02 — dispatch enforcement deferred to 83.3.b)

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

#### 83.4 — `crates/microapp-sdk-rust` reusable Rust helper   🔄  (core SDK ✅, agent-creator migration ✅ — 83.4.b shipped 2026-05-01; Phase 82.x-dependent helpers deferred to 83.4.c)

**Core SDK shipped (83.4 MVP):**
- `crates/microapp-sdk/` new crate (~900 LOC + 36 tests).
  Cargo features: `outbound`, `test-harness`, `webhook`.
- `Microapp` chained builder (`with_tool`, `with_hook`,
  `on_initialize`, `on_shutdown`, `run_stdio`, `run_with`).
- `ToolHandler` async trait + blanket impl for plain async
  fns matching `(Value, ToolCtx) -> Result<ToolReply, ToolError>`.
- `HookHandler` + `HookOutcome { Continue, Abort { reason } }`.
- `ToolCtx` with `binding()` accessor parsed from
  `_meta.nexo.binding` (Phase 82.1 wire shape) +
  `outbound()` accessor gated by `outbound` feature.
- `ToolReply` constructors (`ok`, `ok_json`, `empty`).
- `ToolError` `#[non_exhaustive]` enum mapping to JSON-RPC
  codes (-32601, -32602, -32000) + symbolic codes for
  `data.code`.
- `init_logging_from_env(crate_name)` env-var-driven
  `tracing-subscriber` setup.
- Full JSON-RPC dispatch loop (`initialize`, `tools/list`,
  `tools/call`, `hooks/<name>`, `shutdown`, parse-error
  recovery).
- `MicroappTestHarness` (feature `test-harness`) for
  in-process `call_tool`, `call_tool_with_binding`,
  `fire_hook` testing without spawning the binary.
- `OutboundDispatcher` (feature `outbound`) v0 stub
  returning `DispatchError::Transport` until Phase 82.3.b
  daemon runtime ships.

**83.4.b shipped (agent-creator migration, 2026-05-01):**
- `agent-creator-microapp` 0.0.1 → 0.0.2 — `src/main.rs`
  collapsed from ~200 LOC to ~30 LOC using
  `Microapp::new(...).with_hook("before_message", ...).run_stdio()`.
- Inline tests refactored to `MicroappTestHarness` — 4
  redundant SDK-covered tests deleted, 2 microapp-specific
  retained (`before_message_hook_returns_continue`,
  `unknown_tool_returns_method_not_found`).
- `tests/binding_wire_contract.rs` integration test passes
  unchanged — wire contract identical pre/post migration.
- Direct deps removed: `anyhow`, `thiserror`,
  `tracing-subscriber` (subsumed by SDK).
- Single atomic commit `feat(0.0.2): migrate runtime to
  nexo-microapp-sdk (Phase 83.4.b)`.

**Deferred to 83.4.c (advanced helpers, Phase 82.x-dependent):**
- `PersonaConfigMap<T>` data structure with hot-reload via
  `agents/updated` handler (depends on Phase 82.10 admin
  RPC).
- `MarkdownWatcher` (notify-rs) for hot-reloadable content
  files.
- `LocalSqliteStore` helper with per-`agent_id` isolation
  guard.
- `AdminClient` wrapper for `nexo/admin/*` (depends on
  82.10).
- `AgentEventFirehose` consumer for
  `nexo/notify/agent_event` (depends on 82.11).
- `ProcessingControlClient` (depends on 82.13).
- `EscalationClient` (depends on 82.14).
- `HttpServerHelper` (depends on 82.12).
- `JsonRpcLoop` standalone runner (already implicit in
  `Microapp::run_stdio`; breaks out as a separate utility
  if a microapp wants the loop without the dispatch table).

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

#### 83.5 — `crates/compliance-primitives` reusable library   ✅  (shipped 2026-05-02)

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

#### 83.6 — Microapp contract document   ✅  (shipped 2026-05-01)

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

#### 83.7 — Microapp template (Rust only)   ✅  (shipped 2026-05-01)

> **Curation 2026-05-01**: scope reduced from 3 stacks
> (Rust + Python + TypeScript) to **Rust only**. The microapp
> contract document (83.6) is the language-agnostic interface;
> authors targeting Python/TS implement the JSON-RPC stdio loop
> from the contract spec. Maintaining 3 reference stacks is
> scope creep that ties nexo to ecosystems we do not control.

One template scaffold under `extensions/`. Ships a working
hello-world microapp in Rust.

Scope:
- `extensions/template-microapp-rust/` — depends on
  `microapp-sdk-rust`, ~150 LOC, 2 example tools, hook
  observer, README.

Template demonstrates: BindingContext parsing, persona config
map, one tool with `_nexo_context` access, one hook observer,
`nexo/dispatch` outbound call.

Done criteria:
- Template builds + runs the smoke test
  (`initialize → tools/list → tools/call → shutdown`).
- README explains the contract by reference to 83.6, including
  a "porting to Python/TypeScript" section pointing back to the
  contract document.

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

  **Phase 80.9 MCP channels in v1**: the framework supports
  MCP channel servers (slack / telegram / iMessage / custom
  via `notifications/nexo/channel`) — these are
  framework-supported and exposed via `nexo/admin/channels/*`
  (82.10). However, the v1 agent-creator UI scope is
  **WhatsApp-only** by explicit decision, so MCP channel
  management does NOT appear in the v1 UI. A future v2 of
  this microapp can add a dedicated "MCP Channels" page
  showing the live `ChannelRegistry` + per-binding
  allowlists — that work is purely UI, no core changes
  needed since 80.9 already shipped the runtime + admin RPC
  surface lands in 82.10.
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

#### 83.15 — Microapp testing harness (mock daemon + SDK helper)   ⬜ NEW 2026-05-01

> **Curation gap**: today a microapp author must spawn the real
> `nexo` daemon to exercise their handlers — friction so high that
> every author re-invents private mocks. This sub-phase ships the
> harness that lets a microapp ship its own unit + integration
> tests offline.

Scope:
- New module in `crates/microapp-sdk-rust/src/testing/` exposing:
  - `MockDaemon` — async stub that owns an in-memory JSON-RPC
    transport, drives `initialize → tools/list → tools/call →
    shutdown` lifecycle, lets the test push synthetic
    `agents/updated` / `hooks/<name>` notifications.
  - `MockBindingContext` builder — `MockBindingContext::new()
    .with_agent("ana").with_channel("whatsapp")
    .with_extension_config(yaml!{ ... }).build()`.
  - `MockAdminRpc` — programmable responses to
    `nexo/admin/*` requests so microapps that consume admin
    surfaces can assert their request shape + handle responses.
- Public API surface gated behind a `testing` Cargo feature on
  `microapp-sdk-rust` so production builds do not ship the
  mocks.
- Reference test in `extensions/template-microapp-rust/`
  (Phase 83.7) demonstrates the harness end-to-end: 1 unit
  test per tool + 1 integration test booting `MockDaemon` →
  asserting full lifecycle.
- 8 unit tests on the harness itself (covering each mock
  surface's edge cases — disconnect mid-call, malformed
  notification, capability denial).
- Docs page `docs/src/microapps/testing.md` shows a 50-line
  worked example.

Done criteria:
- `cargo test -p template-microapp-rust` exercises the harness
  without spawning the daemon.
- `agent-creator` migrates ≥ 1 of its handler tests to the
  harness, removing private mock code as evidence the harness
  covers a real use case.

#### 83.16 — Microapp error → operator path   ✅  (shipped 2026-05-02 — supervisor emit + admin-ui + counter deferred to 83.16.b)

> **Curation gap**: today an uncaught exception inside a
> microapp handler stays in the microapp's stderr — operator
> never knows unless they are tailing logs. Boot failures are
> equally invisible. This sub-phase wires structured error
> reporting back to the operator surface (admin-ui badge +
> Prometheus counter + audit log entry).

Scope:
- New JSON-RPC notification `nexo/notify/microapp_error`
  emitted by the daemon's stdio supervisor when the microapp:
  1. Fails to complete `initialize` within the timeout, or
  2. Returns a `tools/call` error with `data.unhandled = true`,
     or
  3. Exits with a non-zero code, or
  4. Triggers an admin-RPC capability denial.
- Notification payload: `{ microapp_id, kind:
  init_timeout|handler_panic|exit|capability_denied,
  correlation_id, occurred_at, summary,
  stack_trace?: string }`.
- Admin-ui consumer renders a per-microapp health badge that
  flips red on the first error in the last 5 min and shows
  the summary on hover.
- Counter `microapp_errors_total{microapp_id, kind}` emitted
  via Phase 28 metrics infra.
- Audit log entry `nexo.audit.microapp_error{...}` published
  on the broker so external observers (Phase 39 stable admin
  API) see the same signal.
- Operator-side rate limit: > 50 errors / 5 min → daemon
  emits a single `MicroappBackoff` event and stops respawning
  the microapp until operator clears the badge.

Done criteria:
- 4 unit tests covering the 4 error kinds.
- 1 integration test: synthetic microapp returns a panic →
  notification reaches the test admin-ui mock + counter
  increments + audit row appears.
- Reference doc `docs/src/microapps/error-handling.md` lists
  the 4 kinds + payload shape + operator runbook.

#### 83.17 — Microapp config schema validation at install   ✅  (shipped 2026-05-02 — CLI integration + derive macro deferred to 83.17.b)

> **Curation gap**: Phase 81.4 plans a plugin-scoped config
> dir loader; today nothing validates the shape of the YAML
> the microapp expects. A misconfigured config fails at
> runtime with a cryptic error, often inside the microapp.
> This sub-phase shifts validation to install time (or daemon
> boot) so the operator gets immediate, actionable feedback.

Scope:
- Microapp ships a JSON Schema (or equivalent Rust
  `schemars`-derived schema) alongside its `plugin.toml` at
  `extensions/<id>/config.schema.json`.
- Daemon `nexo extensions install <id>` (and equivalent boot
  pre-flight) validates the operator-supplied
  `extensions_config.<id>` against the schema before
  spawning. Validation failures abort install with a structured
  error: which field failed, expected vs got, JSON-pointer
  to the bad value.
- For Rust microapps using `microapp-sdk-rust`, a derive
  macro `#[derive(MicroappConfig)]` auto-generates the schema
  from the typed config struct so authors do not write JSON
  Schema by hand.
- Capability inventory: NO new env toggle (the validator runs
  unconditionally at install / boot).
- Operator override env `NEXO_MICROAPP_SKIP_SCHEMA=<microapp_id>`
  bypasses validation for one-off debugging — gated behind the
  existing `*_ALLOW_*` capability inventory entry pattern.
- Reference: the agent-creator microapp ships its schema first,
  validating the existing operator config it expects.

Done criteria:
- 5 unit tests on the validator (missing required field /
  extra unknown field with `additionalProperties: false` /
  type mismatch / valid config / override env honored).
- 1 integration test: `nexo extensions install` fails clean
  on a deliberately-bad config, succeeds on a corrected one.
- Reference doc `docs/src/microapps/config-schema.md` covers
  authoring a schema + the derive macro shortcut.

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
