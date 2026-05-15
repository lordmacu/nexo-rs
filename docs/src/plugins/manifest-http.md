# `[plugin.http]` — daemon-proxied HTTP routes

> Phase 81.33.b.real Stage 2 (Layer 8 of the
> [plugin auto-discovery design](../architecture/plugin-auto-discovery.md)).
> Status: shipped 2026-05-15.

Plugins that need to expose HTTP endpoints to operators or
external callers declare a mount prefix in their
`nexo-plugin.toml`. The daemon's HTTP server (port `:8080`)
matches every request against the registered prefixes and
forwards matches to the plugin's subprocess via broker
JSON-RPC. The plugin handles internal routing under the prefix.

Distinct from
[`[plugin.http_server]`](./contract.md#plugin-http_server),
which advertises a plugin-bound port the daemon does NOT proxy.

## Manifest section

```toml
[plugin.http]
mount_prefix     = "/whatsapp"
# Optional:
# timeout_seconds = 60     # default: 30
```

Fields:

- `mount_prefix` (required) — path prefix the daemon mounts.
  Must start with `/`. Cannot contain `?` or `#`. The daemon
  matches `path.starts_with(mount_prefix)`.
- `timeout_seconds` (optional) — per-request broker RPC timeout.
  Default 30s. Plugins serving slow flows (image generation,
  OAuth dances) extend.

## Broker JSON-RPC contract

Daemon → plugin on `plugin.<id>.http.request`:

```json
{
  "method": "GET",
  "path": "/whatsapp/pair",
  "query": "instance=default",
  "headers": [["Host", "127.0.0.1:8080"], ["User-Agent", "..."]],
  "body_base64": ""
}
```

Plugin replies:

```json
{
  "status": 200,
  "headers": [["Content-Type", "text/html; charset=utf-8"]],
  "body_base64": "<base64-encoded body bytes>"
}
```

Body bytes are base64-encoded so binary payloads (PNGs, PDFs,
small file uploads) round-trip cleanly through JSON. The
daemon decodes server-side and writes raw bytes back to the TCP
stream.

## Route matching

The router stores all prefixes in **longest-first** order. A
request matches the most-specific prefix:

| Registered prefixes | Request | Matched plugin |
|---|---|---|
| `/api`, `/api/v1` | `/api/v1/users` | `/api/v1` plugin |
| `/api`, `/api/v1` | `/api/v2/users` | `/api` plugin |
| `/api` | `/health` | none (fallthrough) |

A miss falls through to the daemon's legacy hardcoded paths
(`/health`, `/metrics`, etc.).

**Reserved prefixes.** The router refuses to register any plugin
mount under these daemon-internal paths:

- `/health` — liveness checks
- `/metrics` — Prometheus scrape
- `/pair` — daemon's WS pairing companion
- `/admin` — admin RPC surface (port 9091, not 8080, but
  reserved here too for safety against future shared-port designs)
- `/.well-known` — protocol probes (RFC 8615)

A plugin manifest declaring any of these (or a sub-path like
`/health/foo`) is rejected at registration with a warn-level log;
the plugin's broker handler stays unhooked for that prefix and
the daemon's internal handler serves uninterrupted.

Plugins choosing non-reserved prefixes (`/whatsapp`, `/oauth`,
`/api/v1/...`, `/healthy`, etc.) register normally.

## Error rendering

| Failure | Status | Body |
|---|---|---|
| Broker timeout (plugin slow / unresponsive) | 504 Gateway Timeout | `{"error":"plugin gateway timeout"}` |
| Plugin replied with malformed JSON | 502 Bad Gateway | `{"error":"plugin reply malformed"}` |
| Plugin replied with `status: 500` etc. | passes through | plugin's body verbatim |

The daemon writes the plugin's `headers` + `body_base64` verbatim
for non-error replies — the plugin owns the response shape.

## Limitations (Stage 2)

- **No streaming responses.** SSE / chunked transfer / progressive
  rendering NOT supported. Plugin must buffer the full response
  before replying. Use `[plugin.http_server]` (own port) for
  streaming endpoints.
- **No WebSocket upgrades.** The daemon's `/pair` WS handshake
  is daemon-internal (Phase 87 pairing companion). Plugins
  wanting WS endpoints bind their own port.
- **Body size cap.** Daemon reads up to 16KB upfront. Larger
  bodies require streaming, which Stage 2 does not support.
- **No request-id header injection.** Plugins should generate
  their own trace ids if needed; the daemon does not auto-stamp
  `X-Request-Id`.

## Implementing the plugin side

Subprocess plugins built on the Rust `nexo-microapp-sdk`
register a broker handler on `plugin.<plugin_id>.http.request`:

```rust
// Sketch (final SDK helpers ship alongside the next plugin release):
ctx.broker
    .subscribe("plugin.<id>.http.request")
    .await?
    .for_each(|msg| async {
        let req: HttpRequest = serde_json::from_value(msg.payload)?;
        let res = my_router.dispatch(&req).await;
        broker
            .publish(&msg.reply_to.unwrap(), Event::new(/* ... */, res))
            .await
    });
```

Reference impl lands with the next `nexo-plugin-whatsapp` release;
until then plugins copy the dispatch logic from the daemon's
current `nexo_plugin_whatsapp::pairing::dispatch_route`.

## Migration status

Canonical plugin crates have NOT yet shipped a manifest revision
adding `[plugin.http]`. Daemon's legacy hardcoded
`if let Some(rest) = path.strip_prefix("/whatsapp/")` block
(gated by `#[cfg(feature = "plugin-whatsapp")]` via 93.12.c.2)
continues to serve. When a plugin ships the section, the router
matches before the legacy block and the plugin's broker handler
takes over without operator action; the legacy block becomes
dead code (still cfg-gated, removable in a follow-up after the
canonical plugins migrate).

## Validation

- `cargo build --release-fast --bin nexo` (default) — 3m11s.
- `cargo build --release-fast --bin nexo --no-default-features` —
  2m53s.
- `cargo nextest run --workspace` — 6294/6294 (8 new tests in
  `plugin_http::tests` covering router matching, response
  decoding, broker error path).
- `cargo nextest run --no-default-features -p nexo-rs` — 105/105.
- `cargo nextest run -p nexo-pairing` — 75/75.

## Trade-offs

| Concern | Decision |
|---|---|
| Sync HTTP handler blocking on broker | Daemon-side handler is `async` already; broker RPC stays async. No sync→async bridge needed. |
| Binary body via base64 | Acceptable for pairing pages (≤100KB) + OAuth callbacks. Streaming follow-up if large-upload demand appears. |
| Header passthrough | Daemon forwards all request headers; reply sets `Content-Type` from plugin response. Cookies need explicit `Set-Cookie` from plugin. |
| Mount prefix conflicts with daemon routes | Daemon **reserves** `/health`, `/metrics`, `/pair`, `/admin`, `/.well-known`. Router registration rejects any plugin asking for these prefixes or sub-paths (logged at warn level). A plugin cannot hijack health checks or admin RPCs even if its manifest declares the matching prefix. |
