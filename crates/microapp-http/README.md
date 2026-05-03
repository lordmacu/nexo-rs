# nexo-microapp-http

Reusable HTTP scaffolding for [`nexo-rs`](https://github.com/lordmacu/nexo-rs)
microapps that expose an operator UI on loopback.

Lifted from the `agent-creator-microapp` reference implementation —
every microapp declaring `[capabilities.http_server]` ends up needing
the same handful of pieces:

| Module | What it gives you |
|---|---|
| [`error`] | Map `nexo_microapp_sdk::admin::AdminError` to an HTTP status + JSON body the operator UI can switch on. |
| [`auth`] | `LiveTokenState` (ArcSwap-backed bearer + operator-token-hash) + `require_bearer` middleware (constant-time compare, query-token fallback for SSE) + `handle_token_rotated` listener for `nexo/notify/token_rotated`. |
| [`static_assets`] | Serve a built SPA `dist/` with two-tier `Cache-Control` (1y immutable on hashed assets, no-cache on shells), gzip + brotli runtime compression, traversal-protected resolver. |
| [`admin_proxy`] | A dumb forwarder for `POST /api/admin` — body `{ method, params }` → `AdminClient::call` → JSON response. SDK takes care of operator-token-hash stamping (Phase 82.10.m). |

## Status

`v0.1.0` — Tier A, in-tree path-dep only. Will publish to crates.io
alongside the rest of Tier A in Phase 83.14.
