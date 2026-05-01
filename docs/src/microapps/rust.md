# Building microapps in Rust

A *microapp* is an external program that talks to the Nexo
daemon over a stable wire contract. It can be a single
JSON-RPC stdio extension (Phase 11), a NATS subscriber, an HTTP
service consuming the webhook envelope, or any combination.

This page lists the helper crates published from the framework
that take care of the wire-shape boilerplate so you can focus on
the microapp's actual logic.

## Tier A — publishable utility crates

### `nexo-tool-meta`

Wire-shape types shared between the daemon and any consumer.
Slim, four-dependency, sub-second compile.

```toml
[dependencies]
nexo-tool-meta = "0.1"
```

What's inside:

- **`BindingContext`** — `(channel, account_id, agent_id,
  session_id, binding_id, mcp_channel_source)` tuple stamped on
  every tool call. Read it from `params._meta.nexo.binding`.
  Stable across turns within a binding.
- **`InboundMessageMeta`** — per-turn metadata about the
  message that triggered the agent turn (kind, sender_id,
  msg_id, inbound_ts, reply_to_msg_id, has_media,
  origin_session_id). Read it from `params._meta.nexo.inbound`.
  Provider-agnostic shape; same for whatsapp / future channels /
  webhook / event-subscriber / delegation / heartbeat.
- **`InboundKind`** — 3-way discriminator
  (`external_user` / `internal_system` / `inter_session`)
  surfacing the origin of the turn so microapps can branch
  handlers without re-deriving from sender presence alone.
- **`build_meta_value` / `parse_binding_from_meta` /
  `parse_inbound_from_meta`** — the inverse trio around the
  dual-write `_meta` payload. The daemon emits, the microapp
  parses.
- **`WebhookEnvelope`** — typed JSON envelope the daemon
  publishes to NATS after every accepted webhook request.
- **`format_webhook_source`** — Phase 72 turn-log marker
  helper.

Round-trip example:

```rust
use nexo_tool_meta::{
    parse_binding_from_meta, parse_inbound_from_meta,
    BindingContext, InboundKind,
};

// Inside a JSON-RPC `tools/call` handler.
fn handle_call(args: &serde_json::Value) {
    let meta = &args["_meta"];
    if let Some(binding) = parse_binding_from_meta(meta) {
        // Route the work to the right tenant.
        match binding.channel.as_deref() {
            Some("whatsapp") => { /* WA-specific */ }
            _ => { /* future channels */ }
        }
    } else {
        // Bindingless path: delegation receive, heartbeat
        // bootstrap, tests. Microapps that don't care still
        // see the legacy flat block at `meta["agent_id"]` etc.
    }

    // Per-turn metadata: who sent what, when, replying to which
    // earlier message, with media or not.
    if let Some(inbound) = parse_inbound_from_meta(meta) {
        match inbound.kind {
            InboundKind::ExternalUser => {
                // Real end-user — apply per-sender rate limits,
                // anti-loop heuristics, etc.
                let _sender = inbound.sender_id.as_deref();
                let _msg_id = inbound.msg_id.as_deref();
            }
            InboundKind::InternalSystem => {
                // Cron tick / scheduler / yaml-declared internal
                // event — skip user-facing checks.
            }
            InboundKind::InterSession => {
                // Peer-agent delegation — `origin_session_id`
                // carries the calling peer's request token.
                let _origin = inbound.origin_session_id;
            }
            _ => { /* future kinds */ }
        }
    }
}
```

### Wire layout

Both buckets live as siblings under `_meta.nexo.*`:

```json
{
  "_meta": {
    "agent_id": "ana",
    "session_id": "00000000-0000-0000-0000-000000000000",
    "nexo": {
      "binding": {
        "agent_id": "ana",
        "channel": "whatsapp",
        "account_id": "personal",
        "binding_id": "whatsapp:personal"
      },
      "inbound": {
        "kind": "external_user",
        "sender_id": "+5491100",
        "msg_id": "wa.ABCD1234",
        "inbound_ts": "2026-05-01T12:34:56Z",
        "reply_to_msg_id": "wa.PREV0001",
        "has_media": false
      }
    }
  }
}
```

Either bucket can be absent: `binding` is omitted on bindingless
paths (delegation receive, heartbeat, tests), `inbound` is omitted
when the producer didn't populate it (legacy paths predating
Phase 82.5). A microapp must tolerate either being missing.

### Producers

| Path | `kind` | `sender_id` | `msg_id` | Source |
|------|--------|-------------|----------|--------|
| whatsapp inbound | `external_user` | E.164 phone | `wa.<id>` | core runtime intake |
| event-subscriber | yaml-declared | JSONPath extract | event id | core runtime synthesizer |
| webhook receiver | yaml-declared (via subscriber) | header/body extract | request id | webhook receiver → subscriber |
| delegation receive | `inter_session` | None | None | core runtime route_sub |
| proactive tick | `internal_system` | None | None | core runtime heartbeat_sub |
| email-followup tick | `internal_system` | None | None | llm_behavior |

### `nexo-webhook-receiver`

Provider-agnostic per-source webhook verification primitives.
HMAC-SHA256 / HMAC-SHA1 / raw-token signature verify + event
kind extraction (header or JSON path) + NATS publish topic
rendering. No HTTP listener — pure-fn surface.

```toml
[dependencies]
nexo-webhook-receiver = "0.1"
```

### `nexo-webhook-server`

Axum-based HTTP listener that mounts the receiver behind a 5-gate
defense pipeline (method / body cap / per-source concurrency /
`(source, client_ip)` rate limit / signature). Suitable as a
standalone webhook ingestion service in any Rust daemon.

```toml
[dependencies]
nexo-webhook-server = "0.1"
```

### `nexo-resilience`

Circuit breaker + retry + rate-limit primitives. Nothing
nexo-specific — drop-in for any Rust service that needs them.

```toml
[dependencies]
nexo-resilience = "0.1"
```

### `nexo-driver-permission`

Bash safety classifier — destructive-command warning, sed-in-place
detection, read-only validation, sandbox heuristic. Useful for
any tool that lets an LLM (or any other untrusted source) emit
shell commands.

```toml
[dependencies]
nexo-driver-permission = "0.1"
```

## Tier B — runtime helpers (Phase 83.4)

`nexo-microapp-sdk` (planned) will package the JSON-RPC stdio
loop, the `BindingContext` parser, and the webhook envelope
consumer behind ergonomic helpers — replaces the ~200 LOC of
boilerplate every microapp would otherwise rewrite. Watch
Phase 83 in `proyecto/PHASES-microapps.md`.

## Forward-compatibility

Every Tier A type that crosses the wire is either
`#[non_exhaustive]` (microapps cannot rely on field exhaustivity
when reading) or has a documented field-add policy. Field
additions are deliberate semver-minor: a microapp built against
`0.1.0` keeps working when the daemon emits a `0.2.x`-shaped
payload because:

- Read-side: serde's permissive default ignores unknown keys.
- Write-side: the daemon never removes fields without bumping
  major.

## Reference microapp

`agent-creator-microapp` (out-of-tree at
`https://github.com/lordmacu/agent-creator-microapp`) is a
working microapp that demonstrates:

- JSON-RPC stdio loop (`initialize` / `tools/list` / `tools/call`
  / `shutdown` / hooks).
- Wire-contract integration test that spawns the binary as a
  subprocess and asserts the daemon-side payload shape.
- `parse_binding_from_meta` consumption from `nexo-tool-meta`.

Use it as the starting template until the dedicated `crates/template-rust/`
microapp scaffold lands in Phase 83.7.
