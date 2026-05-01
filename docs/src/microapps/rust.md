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
- **`build_meta_value` / `parse_binding_from_meta`** — the
  inverse pair around the dual-write `_meta` payload. The
  daemon emits, the microapp parses.
- **`WebhookEnvelope`** — typed JSON envelope the daemon
  publishes to NATS after every accepted webhook request.
- **`format_webhook_source`** — Phase 72 turn-log marker
  helper.

Round-trip example:

```rust
use nexo_tool_meta::{parse_binding_from_meta, BindingContext};

// Inside a JSON-RPC `tools/call` handler.
fn handle_call(args: &serde_json::Value) {
    let meta = &args["_meta"];
    if let Some(binding) = parse_binding_from_meta(meta) {
        // Route the work to the right tenant.
        match binding.channel.as_deref() {
            Some("whatsapp") => { /* WA-specific */ }
            Some("telegram") => { /* TG-specific */ }
            _ => { /* default */ }
        }
    } else {
        // Bindingless path: delegation receive, heartbeat
        // bootstrap, tests. Microapps that don't care still
        // see the legacy flat block at `meta["agent_id"]` etc.
    }
}
```

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
