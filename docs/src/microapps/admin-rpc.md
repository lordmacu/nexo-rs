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
- Manage LLM provider entries (`llm.yaml.providers.*`)
- Approve / revoke MCP-channel servers per agent
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
  EXISTS` + WAL + 2 indices on `microapp_id` and `method`).
  `sweep_retention(retention_days, max_rows)` runs at boot to
  enforce age + cap limits via the
  `NEXO_MICROAPP_ADMIN_AUDIT_RETENTION_DAYS` /
  `_MAX_ROWS` toggles. Library-level `tail(&AuditTailFilter)`
  query (Phase 82.10.h.2) backs the future `nexo microapp admin
  audit tail` CLI subcommand — `format_rows_as_table` and
  `format_rows_as_json` helpers ship in the same module so the
  CLI is one trivial flag-mapping away.

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
