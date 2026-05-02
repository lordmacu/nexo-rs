# Admin RPC

Phase 82.10 ships a bidirectional JSON-RPC layer that lets
microapps perform admin operations on the daemon without leaving
the existing stdio transport. Today the daemon → microapp
direction is `tools/call` + `hooks/<name>`; the inverse is
`nexo/admin/<domain>/<method>`.

A microapp with an operator UI (e.g. `agent-creator-microapp`)
uses this surface to:

- CRUD agents (`agents.yaml.<id>`)
- Register / revoke channel credentials (many-to-many)
- Initiate WhatsApp QR pairing flows
- Manage LLM provider entries (`llm.yaml.providers.*` global,
  `llm.yaml.tenants.<id>.providers.*` per-tenant — Phase
  83.8.12.5)
- Approve / revoke MCP-channel servers per agent
- CRUD tenants (`config/tenants.yaml`) for SaaS deployments
  hosting N empresas / workspaces from one daemon (Phase
  83.8.12 — `nexo/admin/tenants/{list,get,upsert,delete}`)
- Force a hot-reload after batch mutations

## Layered grant model

Admin RPC uses two layers of opt-in:

1. **`plugin.toml [capabilities.admin]`** — what the microapp
   needs:
   ```toml
   [capabilities.admin]
   required = ["agents_crud", "credentials_crud", "pairing_initiate"]
   optional = ["llm_keys_crud", "channels_crud"]
   ```
   - **`required`** — boot fails if operator did not grant.
   - **`optional`** — boot OK; runtime calls return
     `-32004 capability_not_granted` until granted.

2. **`extensions.yaml.entries.<id>.capabilities_grant`** — what
   the operator allows:
   ```yaml
   extensions:
     entries:
       agent-creator:
         capabilities_grant:
           - agents_crud
           - credentials_crud
           - pairing_initiate
           # llm_keys_crud not granted → calls return -32004
   ```

Boot diff produces a `CapabilityBootReport`:

| Diff outcome | Severity | Behaviour |
|--------------|----------|-----------|
| Required not granted | error | Boot fails |
| Optional not granted | warn | Runtime returns -32004 |
| Granted but not declared | warn | Allowed (forward-compat) |
| All matched | ok | No log |

## Wire shape

**Microapp → daemon request** (over the existing stdio):

```json
{
  "jsonrpc": "2.0",
  "id": "app:01HXXX...",
  "method": "nexo/admin/agents/list",
  "params": { "active_only": true }
}
```

**Daemon → microapp response:**

```json
{
  "jsonrpc": "2.0",
  "id": "app:01HXXX...",
  "result": {
    "agents": [
      { "id": "ana", "active": true, "model_provider": "minimax", "bindings_count": 2 }
    ]
  }
}
```

ID prefix `app:` distinguishes microapp-initiated requests from
daemon-initiated `tools/call`. Daemon-initiated IDs use random
UUIDs without that prefix; the runtime asserts the invariant at
boot.

## Capability denial

When the capability gate refuses a call:

```json
{
  "jsonrpc": "2.0",
  "id": "app:01HXXX...",
  "error": {
    "code": -32004,
    "message": "capability_not_granted",
    "data": {
      "capability": "agents_crud",
      "microapp_id": "agent-creator",
      "method": "nexo/admin/agents/upsert"
    }
  }
}
```

SDK side maps this to `AdminError::CapabilityNotGranted { capability, method }`.

## Domains + methods

| Method | Capability | Domain | Wraps |
|--------|-----------|--------|-------|
| `nexo/admin/agents/list` | `agents_crud` | agents | yaml read |
| `nexo/admin/agents/get` | `agents_crud` | agents | yaml read |
| `nexo/admin/agents/upsert` | `agents_crud` | agents | yaml mutate + reload |
| `nexo/admin/agents/delete` | `agents_crud` | agents | yaml remove + reload |
| `nexo/admin/credentials/list` | `credentials_crud` | credentials | filesystem + yaml join |
| `nexo/admin/credentials/register` | `credentials_crud` | credentials | filesystem write + yaml mutate (many-to-many) |
| `nexo/admin/credentials/revoke` | `credentials_crud` | credentials | filesystem unlink + yaml mutate |
| `nexo/admin/pairing/start` | `pairing_initiate` | pairing | session_store insert + plugin trigger |
| `nexo/admin/pairing/status` | `pairing_initiate` | pairing | session_store read |
| `nexo/admin/pairing/cancel` | `pairing_initiate` | pairing | session_store mutate + notification |
| `nexo/admin/llm_providers/list` | `llm_keys_crud` | llm_providers | llm.yaml read |
| `nexo/admin/llm_providers/upsert` | `llm_keys_crud` | llm_providers | env var validation + llm.yaml mutate |
| `nexo/admin/llm_providers/delete` | `llm_keys_crud` | llm_providers | refuse if agent uses + llm.yaml remove |
| `nexo/admin/channels/list` | `channels_crud` | channels | yaml read |
| `nexo/admin/channels/approve` | `channels_crud` | channels | yaml mutate (idempotent) |
| `nexo/admin/channels/revoke` | `channels_crud` | channels | yaml mutate |
| `nexo/admin/channels/doctor` | `channels_crud` | channels | static yaml verdicts |
| `nexo/admin/reload` | `agents_crud` | meta | force Phase 18 hot-reload |

= 18 endpoints (17 spec methods + reload).

## Many-to-many credentials

A single channel credential can serve N agents simultaneously:

```yaml
# agents.yaml — both agents bind to the shared credential
agents:
  - id: ana
    inbound_bindings:
      - { plugin: whatsapp, instance: shared }
  - id: carlos
    inbound_bindings:
      - { plugin: whatsapp, instance: shared }
```

Operators rebind from either side:

- Credential side — `nexo/admin/credentials/register {channel,
  instance, agent_ids: ["ana","carlos"], payload: {...}}` writes
  the credential file and appends `{plugin: channel, instance}`
  to each agent's `inbound_bindings` (skipping duplicates).
- Agent side — `nexo/admin/agents/upsert {id, inbound_bindings:
  [...]}` replaces the binding list directly.

`nexo/admin/credentials/revoke {channel, instance}` removes the
binding from every agent that was using it AND deletes the
credential file.

Framework is channel-agnostic; v1 microapp UIs scope to WhatsApp
only.

## Async pairing flow

```
Microapp                                   Daemon
   |--- pairing/start (agent_id, channel) ---->|
   |<-- {challenge_id, expires_at_ms, ...} ----|
   |                                            |
   | (out-of-band: channel plugin starts QR)    |
   |                                            |
   |<-- nexo/notify/pairing_status_changed -----|
   |    {challenge_id, state: "qr_ready", data: {qr_ascii, qr_png_base64}}
   |                                            |
   | (operator scans QR on phone)               |
   |                                            |
   |<-- nexo/notify/pairing_status_changed -----|
   |    {challenge_id, state: "linked", data: {device_jid}}
   |                                            |
   | (microapp calls credentials/register to    |
   |  complete the binding)                     |
```

Notification topic: `nexo/notify/pairing_status_changed` (no `id`
field — server-pushed).

States: `pending` → `qr_ready` → `awaiting_user` → `linked` |
`expired` | `cancelled`. Microapp may also poll
`nexo/admin/pairing/status` or cancel via
`nexo/admin/pairing/cancel`.

## Audit log

Every dispatched call appends one row regardless of outcome
(`ok` / `error` / `denied`):

```rust
struct AdminAuditRow {
    microapp_id: String,
    method: String,
    capability: String,
    args_hash: String,        // SHA-256 of canonicalized params
    started_at_ms: u64,
    result: AdminAuditResult,
    error_code: Option<i32>,
    duration_ms: u64,
}
```

`args_hash` lets operator audit pipelines detect repeated
identical calls (potential abuse) without storing PII payloads.

Two writer implementations:

- **`InMemoryAuditWriter`** — default, used in tests and as a
  fallback when no on-disk path is configured. Resets on restart.
- **`SqliteAdminAuditWriter`** (Phase 82.10.h.1) — writes the
  `microapp_admin_audit` table (idempotent `CREATE TABLE IF NOT
  EXISTS` + WAL + 3 indices on `microapp_id`, `method`, and
  `tenant_id`). `sweep_retention(retention_days, max_rows)`
  runs at boot to enforce age + cap limits via the
  `NEXO_MICROAPP_ADMIN_AUDIT_RETENTION_DAYS` /
  `_MAX_ROWS` toggles. Library-level `tail(&AuditTailFilter)`
  query (Phase 82.10.h.2) backs the `nexo microapp admin
  audit tail` CLI — `format_rows_as_table` and
  `format_rows_as_json` helpers ship in the same module.

### Phase 83.8.12.6.runtime + .b — skills resolution chain + migration

The runtime `SkillLoader` resolves a skill name in this order:

1. `<root>/<tenant_id>/<name>/SKILL.md` (when the agent has
   `tenant_id` set)
2. `<root>/__global__/<name>/SKILL.md`
3. `<root>/<name>/SKILL.md` (legacy pre-83.8.12.6 layout — logs
   a deprecation warning when used)

Per-tenant skills override the global namespace, and the global
namespace fills in for tenants that don't have their own copy.
The legacy fallback keeps existing deployments working without
any migration; the deprecation log nudges operators toward the
new layout.

For a clean cutover, `nexo_setup::skills_migrate::migrate_legacy_skills_to_global`
moves every legacy `<root>/<name>/SKILL.md` into
`<root>/__global__/<name>/SKILL.md`. Idempotent, leaves
tenant-scoped layouts untouched, reports filename conflicts.

### Phase 83.8.12.4.b — per-tenant event firehose + escalations filter

`AgentEventKind::TranscriptAppended` events carry the agent's
`tenant_id` whenever the runtime knows it (`agent.tenant_id`
from `agents.yaml`). The framework writer
(`TranscriptWriter::with_tenant_id`) and reader
(`TranscriptReaderFs::with_tenant_id`) both stamp the field
on emit; firehose subscribers can filter per-tenant without
a per-event lookup against `agents.yaml`. Untagged
deployments (single-tenant) emit `tenant_id: null` — back
compat preserved.

`agent_events/list` and `escalations/list` honour
`filter.tenant_id` defense-in-depth: cross-tenant queries
return empty (no leak of existence). Agents lacking a
`tenant_id` field in `agents.yaml` are excluded from any
non-`null` tenant filter.

### Phase 83.8.12.7 — per-tenant audit scope

Every audit row carries an `Option<String> tenant_id` that the
dispatcher sniffs from `params.tenant_id` (string-typed only —
non-string values yield `None` defensively). Calls that lack a
tenant scope (`echo`, `pairing/*`, `credentials/*`) leave the
column `NULL` so existing pre-83.8.12.7 deployments keep
working. Operators can filter the tail by tenant for SaaS
billing or compliance reviews:

```bash
# CLI — restrict to one tenant scope
nexo microapp admin audit tail --tenant acme --limit 100

# combine with other filters
nexo microapp admin audit tail --tenant acme --result denied --since-mins 60

# library-side convenience: tail_for_tenant(tenant, since_ms?, limit)
let rows = writer.tail_for_tenant("acme", None, 50).await?;
```

Schema migrates forward-only on `open()`: the inline
`CREATE TABLE IF NOT EXISTS` adds `tenant_id` for fresh DBs, and
`ALTER TABLE ... ADD COLUMN tenant_id TEXT` runs idempotently
on legacy DBs (the duplicate-column-name error is the green
path). Existing audit rows keep `NULL` and are excluded from
any tenant-scoped tail.

## INVENTORY env toggles

Per-domain global kill switches in
`crates/setup/src/capabilities.rs::INVENTORY`:

| Env var | Default | Disable effect |
|---------|---------|----------------|
| `NEXO_MICROAPP_ADMIN_AGENTS_ENABLED` | `1` | All `agents/*` return `-32601` |
| `NEXO_MICROAPP_ADMIN_CREDENTIALS_ENABLED` | `1` | All `credentials/*` return `-32601` |
| `NEXO_MICROAPP_ADMIN_PAIRING_ENABLED` | `1` | All `pairing/*` return `-32601` |
| `NEXO_MICROAPP_ADMIN_LLM_KEYS_ENABLED` | `1` | All `llm_providers/*` return `-32601` |
| `NEXO_MICROAPP_ADMIN_CHANNELS_ENABLED` | `1` | All `channels/*` return `-32601` |

Capability grants are the **per-microapp** check; INVENTORY is
the **operator-global** kill switch (e.g. enterprise op disables
pairing entirely while keeping agents CRUD).

## SDK side

Microapp Rust code uses the SDK's `AdminClient` (gated by the
`admin` cargo feature):

```toml
[dependencies]
nexo-microapp-sdk = { version = "0.1", features = ["admin"] }
```

```rust
use nexo_microapp_sdk::admin::{AdminClient, AdminError};
use nexo_tool_meta::admin::agents::AgentsListFilter;

async fn list_active_agents(client: &AdminClient) -> Result<usize, AdminError> {
    let response: nexo_tool_meta::admin::agents::AgentsListResponse =
        client.call(
            "nexo/admin/agents/list",
            AgentsListFilter { active_only: true, plugin_filter: None },
        ).await?;
    Ok(response.agents.len())
}
```

Each call generates a fresh `app:<uuid-v7>` request id, registers
a oneshot receiver, writes the JSON-RPC frame, and awaits the
response (default 30 s timeout). Capability denial maps to the
typed `AdminError::CapabilityNotGranted { capability, method }`.

## Production wiring

Three production adapters ship in `nexo_setup::admin_adapters`
(Phase 82.10.h.3) — they close the cycle between core (which
declares the traits) and setup (which holds the concrete
`yaml_patch` + filesystem code):

```rust
use nexo_setup::admin_adapters::{
    AgentsYamlPatcher, FilesystemCredentialStore, LlmYamlPatcherFs,
};

let agents = AgentsYamlPatcher::new(config_dir.join("agents.yaml"));
let llm    = LlmYamlPatcherFs::new(config_dir.join("llm.yaml"));
let creds  = FilesystemCredentialStore::new(secrets_root);
let audit  = SqliteAdminAuditWriter::open(state_dir.join("admin_audit.db")).await?;

let dispatcher = AdminRpcDispatcher::new()
    .with_capabilities(capability_set)
    .with_audit_writer(audit)
    .with_agents_domain(agents.clone(), reload_signal.clone())
    .with_credentials_domain(agents, creds)
    .with_llm_providers_domain(llm);
```

`AgentsYamlPatcher` is `Clone` and feeds both the agents and the
credentials domain (the latter mutates `inbound_bindings` on each
agent). `serde_yaml::Value` ↔ `serde_json::Value` conversion
happens inside the adapter, so trait callers stay JSON-typed
(matching what microapps see on the wire).

### Bootstrap helper (Phase 82.10.h.b.5)

`nexo_setup::admin_bootstrap::AdminRpcBootstrap::build` wraps the
full wire path so operators don't hand-thread every adapter into
the dispatcher:

```rust
use nexo_setup::admin_bootstrap::{AdminBootstrapInputs, AdminRpcBootstrap};

let bootstrap = AdminRpcBootstrap::build(AdminBootstrapInputs {
    config_dir: &config_dir,
    secrets_root: &secrets_root,
    audit_db: std::env::var_os("NEXO_MICROAPP_ADMIN_AUDIT_DB")
        .as_ref()
        .map(std::path::Path::new),
    extensions_cfg: &extensions_cfg,
    admin_capabilities: &per_extension_admin_caps,
    reload_signal,
})
.await?;
```

`build` returns `Ok(None)` when no microapp declares
`[capabilities.admin]` so the daemon pays zero overhead in the
common case. When it returns `Some(bootstrap)`, the spawn loop
threads the per-microapp `AdminRouter` through
`StdioSpawnOptions::admin_router` and post-spawn binds the live
outbound writer:

```rust
let opts = bootstrap
    .spawn_options_for(&extension_id, default_opts)
    .unwrap_or(default_opts);
let runtime = StdioRuntime::spawn_with(&manifest, opts).await?;
bootstrap.bind_writer(&extension_id, runtime.outbox_sender());
```

A periodic 30 s task prunes the in-memory pairing store.

### In-memory pairing challenge store (Phase 82.10.h.b.1)

`InMemoryPairingChallengeStore` is a `DashMap<Uuid, …>` + TTL
adapter — same pattern as OpenClaw's `activeLogins` map.
`read_challenge` lazily flips entries past their TTL to
`PairingState::Expired` with an operator-readable
`data.error`, so polls converge to the terminal state without
waiting for the prune cadence. Daemon restart drops in-flight
challenges (the WhatsApp QR client-side expires in ~30 s
anyway, so a SQLite-backed store would be wasted work).

### Pairing notifier (deferred)

`StdioPairingNotifier` ships as a building block but is **not
yet wired** into `AdminRpcBootstrap`. Microapps fall back to
polling `pairing/status` until a follow-up exposes a separate
notification queue independent of the response writer.

## Agent events firehose (Phase 82.11)

`agent_events` is the cross-app surface microapps use to stream
and query agent activity. v0 emits one variant —
`TranscriptAppended` — but the wire shape is a
discriminated `#[non_exhaustive]` enum so future kinds (batch
job completion, image-gen output, custom) land non-breaking.

### Backfill RPC (`nexo/admin/agent_events/*`)

- `nexo/admin/agent_events/list { agent_id, kind?, since_ms?,
  limit? }` — newest-first window query, default
  `since_ms = now - 30d`, `limit = 500` clamped to 1000.
- `nexo/admin/agent_events/read { agent_id, session_id,
  since_seq?, limit? }` — one-scope ascending tail, exclusive
  `since_seq` (a microapp that received `seq=4` live re-issues
  `read` with `since_seq=4` and gets seq=5,6,7,…). Unknown
  scope returns `events: []`, NOT `-32601`.
- `nexo/admin/agent_events/search { agent_id, query, kind?,
  limit? }` — FTS5 query over the redacted body. Backed by the
  existing `transcripts_fts` virtual table.

All three require capability `transcripts_read`.

### Live notifications (`nexo/notify/agent_event`)

JSON-RPC notification frame, no `id`:

```json
{"jsonrpc":"2.0","method":"nexo/notify/agent_event",
 "params":{"kind":"transcript_appended","agent_id":"ana",
           "session_id":"…","seq":7,"role":"user",
           "body":"[REDACTED:phone] hola","sent_at_ms":…,
           "sender_id":"wa.55","source_plugin":"whatsapp"}}
```

Body is **always already-redacted** at emit time — the hook
fires inside `TranscriptWriter::append_entry` AFTER the
redactor (Phase 10.4) replaces secrets with
`[REDACTED:label]`. Defense-in-depth: a microapp without
`transcripts_read` cannot recover the raw body either.

### Subscribe semantics

There is no explicit `subscribe` RPC — `AdminRpcBootstrap`
inspects the operator's grant matrix at boot:

- Microapp granted `transcripts_subscribe` → receives every
  `TranscriptAppended` frame.
- Microapp granted `agent_events_subscribe_all` → receives
  every kind. Reserved for audit / compliance microapps that
  need full visibility (v0 emits only `TranscriptAppended` so
  the two caps are equivalent today; the slot future-proofs
  for batch / output kinds).
- Microapp without either cap → receives no frames; backfill
  RPC still gated on `transcripts_read`.

`seq` discipline: per-`session_id` monotonic counter that
advances by 1 per `TranscriptAppended` frame. Live + backfill
agree on `seq` values, so a microapp that misses live frames
(broadcast lag, transient stdin block) re-issues
`agent_events/read` with `since_seq = last_seen` to resync.

### INVENTORY toggle

`NEXO_MICROAPP_AGENT_EVENTS_ENABLED` (default `1`). Off →
broadcast emitter is replaced with a no-op AND no subscribe
tasks spawn. Backfill RPC continues to work (so a microapp
with `transcripts_read` keeps querying past sessions). Useful
for hardened deployments that want only on-demand history.

### Lag handling

`tokio::sync::broadcast` channel with default capacity 256.
Subscribers that fall behind get `RecvError::Lagged(n)` —
boot wires this as a single `warn` log and the receiver
re-syncs to the next surviving frame. Microapps that need
gap-free history call `agent_events/read` from
`last_seen_seq`.

## HTTP server capability (Phase 82.12)

Microapps that ship their own HTTP UI / API (meta-microapp,
dashboard, settings panel) declare it in `plugin.toml`:

```toml
[capabilities.http_server]
port = 9001
bind = "127.0.0.1"             # default — loopback only
token_env = "AGENT_CREATOR_TOKEN"
health_path = "/healthz"        # default
```

### Boot supervisor

`HttpServerSupervisor::probe(decl)` polls
`GET <bind>:<port><health_path>` every 250 ms until 200 OK or
the 30 s ready timeout. Typed errors:
- `Timeout { url }` — no listener after 30 s.
- `BadStatus { url, status }` — listener responds non-200.

Once probed, `spawn_monitor_loop(decl)` polls every 60 s.
Failures log at `warn` and flip a `watch::Receiver<bool>` so
`nexo extension status` / admin-ui can surface the live
health state. Monitor handle aborts on drop.

### Bind policy

`bind` defaults to `127.0.0.1`. Anything else (`0.0.0.0`,
public IP, …) requires the operator to flip
`extensions.yaml.<id>.allow_external_bind = true`. The
`AdminRpcBootstrap::build` validator checks this BEFORE
spawning the extension; mismatches surface as
`AdminBootstrapError::ExternalBindNotAllowed { microapp_id,
bind }`. Defense in depth against accidentally world-exposed
services.

### Shared bearer token

The microapp reads `<token_env>` at boot (the daemon passes
it through via the `initialize` env block). All inbound HTTP
requests must include `Authorization: Bearer <token>` or
`X-Nexo-Token: <token>`. Token rotation arrives as a JSON-RPC
notification — the daemon emits
`nexo/notify/token_rotated { old_hash, new }` after the
operator changes the env + reloads. Microapps compare
`old_hash` against `token_hash(<their current token>)`
(sha256-hex truncated to 16 chars) before swapping, so a
stale notification hitting an already-restarted microapp is
ignored.

### INVENTORY toggle

`NEXO_MICROAPP_HTTP_SERVERS_ENABLED` (default `1`). Off →
boot supervisor skips the probe + monitor loop entirely.
Microapps still spawn; the daemon just doesn't gate `ready`
on the HTTP endpoint. Useful for hardened deployments that
ban embedded HTTP servers or run them out-of-band.

## Operator processing pause + intervention (Phase 82.13)

Operators sometimes need to suspend agent autonomy on a
specific scope and step in manually. v0 ships chat-takeover
(per-conversation pause + manual reply); the wire shape is
generalised across every agent shape so future variants
(batch override, event injection, image-gen output edit)
plug in without breaking the surface.

### Wire shapes

```rust
#[non_exhaustive]
enum ProcessingScope {
    Conversation { agent_id, channel, account_id, contact_id, mcp_channel_source? },
    AgentBinding { ... },   // reserved
    Agent { ... },          // reserved
    EventStream { ... },    // reserved
    BatchQueue { ... },     // reserved
    Custom { ... },         // forward-compat
}

#[non_exhaustive]
enum InterventionAction {
    Reply { channel, account_id, to, body, msg_kind, attachments?, reply_to_msg_id? },
    SkipItem { ... },        // reserved
    OverrideOutput { ... },  // reserved
    InjectInput { ... },     // reserved
    Custom { ... },          // forward-compat
}

#[non_exhaustive]
enum ProcessingControlState {
    AgentActive,
    PausedByOperator { scope, paused_at_ms, operator_token_hash, reason? },
}
```

`operator_token_hash` is the Phase 82.12 `token_hash` shape
(sha256-hex truncated to 16 chars) — audits correlate without
storing the cleartext bearer.

### Methods

- `nexo/admin/processing/pause { scope, reason?, operator_token_hash }`
  → `ProcessingAck { changed, correlation_id }`. Idempotent.
- `nexo/admin/processing/resume { scope, operator_token_hash }`
  → ack.
- `nexo/admin/processing/intervention { scope, action,
  operator_token_hash }` → ack. Rejects calls on a
  non-paused scope (`-32004 not_paused`) so operators never
  double-respond.
- `nexo/admin/processing/state { scope }` →
  `ProcessingStateResponse { state }`.

All four gated on the `operator_intervention` capability.
Per-scope sub-gates (`operator_intervention_conversation`,
`_batch`, …) are a future-proofing slot.

### v0 surface

Only the `Conversation` + `Reply` combination routes
end-to-end. Non-v0 scopes / actions surface as `-32601
not_implemented` so callers can probe the wire shape today
without the daemon pretending to support unimplemented
shapes.

### Notification

`nexo/notify/processing_state_changed` (literal pinned in
`PROCESSING_STATE_CHANGED_NOTIFY_METHOD`) rides the agent
event firehose deferred wire-up — the inbound dispatcher
hook + transcript `role: Operator` integration land in
82.13.b alongside the actual reply-out adapter.

### Transcript stamping (Phase 82.13.b.1)

When the operator dispatches a reply via
`nexo/admin/processing/intervention`, the daemon optionally
stamps the reply onto the agent transcript so the agent sees
it on its next turn (after `resume`). To opt in, the
microapp passes the active `session_id` in the params:

```jsonc
{
    "method": "nexo/admin/processing/intervention",
    "params": {
        "scope": { "kind": "conversation", "agent_id": "ana", ... },
        "action": {
            "kind": "reply",
            "channel": "whatsapp",
            "account_id": "wa.0",
            "to": "wa.55",
            "body": "ya te resuelvo, dame 1 minuto",
            "msg_kind": "text"
        },
        "operator_token_hash": "abcdef0123456789",
        "session_id": "33333333-3333-4333-8333-333333333333"
    }
}
```

After the channel send acks, the daemon appends one entry to
the session transcript:

| Field | Value |
|-------|-------|
| `role` | `Assistant` (so the agent reads it as natural continuity on its next turn) |
| `content` | The reply body, run through the standard redactor |
| `source_plugin` | `intervention:<channel>` (e.g. `intervention:whatsapp`) — distinguishes operator stand-in from native LLM output |
| `sender_id` | `operator:<token_hash>` — identifies the operator without exposing PII |
| `message_id` | Channel-side provider id when the plugin acked one |

The same redactor + FTS index + Phase 82.11 firehose
pipeline as native agent appends — subscribers of
`nexo/notify/agent_event` see the operator's reply with
the discriminator above.

The ack includes a `transcript_stamped` hint:

| Value | Meaning |
|-------|---------|
| `Some(true)` | Reply persisted on transcript. Agent will see it on next turn. |
| `Some(false)` | Channel send happened, transcript was NOT modified. Either no `session_id` in params, no transcript appender wired in boot, or persistence failed (logged). |
| `None` (omitted) | Field not applicable (e.g. for non-Reply interventions). |

When `transcript_stamped: false` and the operator UI knows
the active session, prompt the operator to reopen the
conversation and retry — the agent will otherwise reanudar
"ciega" without seeing what was said during takeover.

The SDK helper threads this through fluently:

```rust
use nexo_microapp_sdk::admin::{HumanTakeover, SendReplyArgs};

let takeover = HumanTakeover::engage(&admin, scope, token_hash, None).await?;
takeover
    .send_reply(
        "whatsapp",
        "wa.0",
        "wa.55",
        SendReplyArgs::text("ya te resuelvo")
            .with_session(active_session_id),
    )
    .await?;
takeover.release(None).await?;
```

### Operator summary on resume (Phase 82.13.b.2)

The operator can hand the agent a free-text summary of what
happened during takeover. The daemon stamps it as a `System`
transcript entry just after the resume flip, so the agent
reads it as a system directive on its next turn:

```jsonc
{
    "method": "nexo/admin/processing/resume",
    "params": {
        "scope": { "kind": "conversation", "agent_id": "ana", ... },
        "operator_token_hash": "abcdef0123456789",
        "session_id": "33333333-3333-4333-8333-333333333333",
        "summary_for_agent": "cliente confirmó dirección, IA puede continuar con confirmación de envío"
    }
}
```

The stamped entry shape:

| Field | Value |
|-------|-------|
| `role` | `System` |
| `content` | `[operator_summary] <body>` (body trimmed; prefix added server-side) |
| `source_plugin` | `intervention:summary` |
| `sender_id` | `operator:<token_hash>` |
| `message_id` | `None` |

Validation (handler-side, all `-32602 invalid_params`):

| Code | When |
|------|------|
| `session_id_required_with_summary` | `summary_for_agent` set but `session_id` missing |
| `empty_summary` | summary trims to zero length |
| `summary_too_long` | summary > 4096 chars (matches `TranscriptsIndex` FTS5 doc cap) |

Validation runs BEFORE the state flip, so a rejected call
keeps the scope paused. Stamping itself is best-effort —
appender errors leave the scope `AgentActive` (resume still
succeeds) and surface only via `ack.transcript_stamped:
Some(false)`.

The SDK helper takes the summary on `release()` after pinning
the session via `with_session()`:

```rust
let takeover = HumanTakeover::engage(&admin, scope, token_hash, None)
    .await?
    .with_session(active_session_id);
// ... operator types replies via takeover.send_reply ...
takeover
    .release(Some(
        "cliente confirmó dirección, IA puede continuar con envío".into(),
    ))
    .await?;
```

The pinned session is reused by both `send_reply` (transcript
stamping) and `release` (summary injection) — set once,
forget. Per-call `SendReplyArgs.with_session()` overrides the
pinned one when both are present.

### Pending inbounds during pause (Phase 82.13.b.3)

While a scope is `PausedByOperator`, inbound user messages
arriving on the channel are buffered server-side instead of
firing an agent turn. On resume, the buffer is drained and
each inbound is stamped on the agent transcript as a `User`
entry with its ORIGINAL timestamp — so the agent reads real
chronology of what the customer said during takeover.

| Field | Value |
|-------|-------|
| `role` | `User` |
| `content` | Original (already-redacted) inbound body |
| `source_plugin` | Channel that produced the inbound (`whatsapp`, etc.) |
| `sender_id` | Counterparty id (e.g. WA jid) |
| `message_id` | Channel-side provider id when present |

The cap is configured via `NEXO_PROCESSING_PENDING_QUEUE_CAP`
(default 50, set to `0` to disable buffering entirely).
When the cap is exceeded, the OLDEST entry is evicted FIFO
and an `AgentEventKind::PendingInboundsDropped` firehose
event fires so operator UIs can surface the drop.

```jsonc
// Firehose frame on cap-exceeded eviction:
{
    "jsonrpc": "2.0",
    "method": "nexo/notify/agent_event",
    "params": {
        "kind": "pending_inbounds_dropped",
        "agent_id": "ana",
        "scope": { "kind": "conversation", "agent_id": "ana", ... },
        "dropped": 1,
        "at_ms": 1700000000000
    }
}
```

`ProcessingAck.drained_pending: Some(N)` on the resume call
reports how many entries were drained — `None` when the
queue was empty (no field on the wire). Operator UIs render
"replay: 3 messages" so the operator knows what the agent
will see on its next turn.

**Round-trip end-to-end (Phase 82.13.c, 2026-05-02):**
the inbound dispatcher push hook now lives in
`runtime.rs`, gated on a shared
`Arc<dyn ProcessingControlStore>` boot wires to BOTH the
admin RPC dispatcher AND every `AgentRuntime`. When the
operator pauses via `nexo/admin/processing/pause`, the very
next inbound channel message is buffered onto the
per-scope queue (cap = `NEXO_PROCESSING_PENDING_QUEUE_CAP`,
default 50, FIFO eviction). Body is redacted at push time
so the queue never holds raw PII. Resume drains the queue
onto the transcript as `User` entries with original
timestamps — agent reanudes coherently with full
chronology.

**Smoke recipe** (manual end-to-end):

```bash
# 1. Pause a conversation via admin RPC.
curl -X POST localhost:.../admin -d '{
    "method": "nexo/admin/processing/pause",
    "params": {
        "scope": { "kind": "conversation", "agent_id": "ana",
                   "channel": "whatsapp", "account_id": "wa.0",
                   "contact_id": "wa.55" },
        "operator_token_hash": "..."
    }
}'

# 2. Send 3 WhatsApp inbounds while paused.
#    The agent does NOT reply (intake hook buffers them).

# 3. Resume with optional summary.
curl -X POST localhost:.../admin -d '{
    "method": "nexo/admin/processing/resume",
    "params": {
        "scope": { ... },
        "session_id": "...",
        "summary_for_agent": "cliente confirmó dirección",
        "operator_token_hash": "..."
    }
}'

# 4. Verify the transcript JSONL contains 3 fresh `User`
#    entries with their ORIGINAL timestamps (not now()),
#    plus a `[operator_summary] cliente confirmó dirección`
#    System entry just after the resume.

# 5. Send 1 more WhatsApp inbound → agent replies normally,
#    seeing all 4 buffered + 1 fresh user messages on its
#    next turn.
```

**Boot activation** still depends on `src/main.rs` building
the `AdminRpcBootstrap` (deferred follow-up — same
boot-order refactor that gates the rest of the admin RPC
surface). Until then, the pause check + buffer infra exist
but are dormant in production. Once that lands, this
round-trip works without any further changes.

## Agent escalations (Phase 82.14)

Cross-app primitive for the "I need help here" channel:
agents flag work items they cannot complete autonomously,
operators see a list and dismiss / take over. v0 ships the
admin RPC surface (read + resolve) plus the auto-resolve
hook on `processing/pause`; the `escalate_to_human`
built-in tool that raises new escalations is deferred to
82.14.b.

### Wire shapes

```rust
enum EscalationReason {
    OutOfScope, MissingData, NeedsHumanJudgment,
    Complaint, Error, Ambiguity, PolicyViolation, Other,
}
enum EscalationUrgency { Low, Normal, High }

#[non_exhaustive]
enum ResolvedBy {
    OperatorTakeover,
    OperatorDismissed { reason: String },
    AgentResolved,
}

#[non_exhaustive]
enum EscalationState {
    None,
    Pending {
        scope: ProcessingScope,   // 82.13 enum
        summary, reason, urgency,
        context: BTreeMap<String, Value>,
        requested_at_ms,
    },
    Resolved { scope, resolved_at_ms, by },
}
```

`context` is free-form per agent shape: chat agents emit
`{"question": …, "customer_phone": …}`, batch agents emit
`{"job_id": …, "invalid_rows": 47}`, image-gen emits
`{"prompt": …, "policy": "nudity"}`. Keeps the schema
stable while letting each agent surface meaningful detail.

### Methods

- `nexo/admin/escalations/list { filter (default
  pending), agent_id?, scope_kind?, limit }` →
  `EscalationsListResponse { entries }`. Newest-first by
  `requested_at_ms` / `resolved_at_ms`; default cap 100,
  max 1000.
- `nexo/admin/escalations/resolve { scope, by, dismiss_reason?,
  operator_token_hash }` → `EscalationsResolveResponse
  { changed, correlation_id }`. `by = "dismissed"` requires
  a `dismiss_reason`; `by = "takeover"` is the same outcome
  the auto-resolve hook produces.

Two granular capabilities:
- `escalations_read` — gates `list`. Read-only dashboards
  hold this.
- `escalations_resolve` — gates `resolve`. Strictly stronger
  grant for operator UIs that act on escalations.

### Auto-resolve on pause

When `nexo/admin/processing/pause` fires on a scope with a
matching `Pending` escalation AND both the processing +
escalation stores are wired, the dispatcher
auto-flips the escalation to `Resolved
{ OperatorTakeover }` BEFORE applying the pause. Failures
in the auto-resolve path log at `warn` and never block the
pause itself — operator intent (pause) takes priority over
side-effects.

### Notification literals

`escalation_requested` and `escalation_resolved` are pinned
as `pub const` in the wire crate; the emit site lands in
82.14.b alongside the `escalate_to_human` built-in tool +
the BindingContext→scope derivation.

## Limitations

- **Bidirectional flow over single stdio**: `app:` ID prefix
  disambiguates microapp-initiated requests from daemon-initiated
  ones. Daemon must not use `app:` prefix for its own request IDs.
- **Audit log writer choice**: `InMemoryAuditWriter` resets on
  daemon restart; pick `SqliteAdminAuditWriter::open(path)` for
  durable retention + the boot-time `sweep_retention()` sweeper.
- **`channels/doctor` static-only**: live MCP probe stays in
  `nexo channel doctor --runtime` CLI.
- **Live operator approval**: every grant is yaml-static. v1 has
  no `ask` interactive flow (deferred to 82.10.i).

## See also

- [Building microapps in Rust](./rust.md) — SDK + helper crate
  surface (where `AdminClient` lives behind the `admin` feature).
- [Capability toggles](../ops/capabilities.md) — operator-global
  INVENTORY kill switches.
- [Pairing protocol](../ops/pairing.md) — Phase 26 underlying
  pairing infrastructure.
- [Config hot-reload](../ops/hot-reload.md) — Phase 18 reload
  trigger that admin RPC mutations hook into.
