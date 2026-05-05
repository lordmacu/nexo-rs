# OAuth flows (Phase 82.10.u)

> Two-step admin RPC flow that lets an operator authorise a Claude
> subscription or MiniMax Token Plan from the SPA wizard without
> the SPA ever touching the PKCE verifier or refresh tokens.

## Why an admin RPC and not just stdin

Pre-82.10.u, OAuth lived inside `crates/setup/src/services/`:
interactive stdin paste, `claude login` style. That works for
single-tenant operators who own the daemon shell, but for a
multi-tenant SaaS the operator is a browser tab — there is no
stdin.

Phase 82.10.u extracts the PKCE primitives (`crates/llm-auth`)
and exposes them over admin RPC so a SPA can drive the same flow.
The framework owns the verifier across the two HTTP requests via
`InMemoryVerifierStore`; the SPA only sees opaque session ids.

## Endpoints

### `nexo/admin/llm_providers/oauth_start`

```jsonc
// req
{
  "factory_type": "anthropic",
  "auth_mode": "oauth_auth_code",
  "tenant_id": null
}
// resp (auth_code)
{
  "session_id": "f2c1...",
  "authorize_url": "https://claude.ai/oauth/authorize?...",
  "expires_at_ms": 1714776600000,
  "flow_kind": "auth_code"
}
// resp (device_code) — for `(minimax, oauth_device_code)`
{
  "session_id": "9a3e...",
  "authorize_url": "https://api.minimax.io/...",
  "expires_at_ms": ...,
  "flow_kind": "device_code",
  "user_code": "ABC123",
  "polling_interval_ms": 2000
}
```

### `nexo/admin/llm_providers/oauth_finish`

```jsonc
// req — auth_code
{
  "session_id": "f2c1...",
  "instance_id": "anthropic-personal",
  "code": "abc#def"        // operator pasted from callback page
}
// req — device_code (no code; daemon polls)
{
  "session_id": "9a3e...",
  "instance_id": "minimax-cliente-a"
}
// resp
{
  "ok": true,
  "account_email": "user@example.com",  // Anthropic only
  "expires_at_ms": 1714780200000,
  "secret_id": "LLM_ANTHROPIC_PERSONAL_OAUTH_BUNDLE"
}
```

## State machine

```
[oauth_start]                                    [oauth_finish]
─────────────                                    ─────────────
                          (10 min TTL)
PKCE gen → store.put → ...........  → take →  exchange/poll → bundle → SecretsStore
                                                                     ↓
                                                                yaml patch:
                                                                  auth.mode = oauth_bundle
                                                                  auth.bundle = <secret path>
                                                                     ↓
                                                                reload_signal()
```

## Defensive design

| Concern | Mitigation |
|---|---|
| CSRF | `state` is checked against the stashed PKCE state inside `exchange_code` |
| Replay | `take()` removes the entry BEFORE exchange; second call → `SESSION_NOT_FOUND` |
| Expired sessions | `peek_status` discriminates `Live` / `Expired` / `Missing` so the SPA gets accurate diagnostics |
| Memory bloat | Background sweep every 60 s drops stale entries; capacity 100 with FIFO eviction |
| Verifier leak | Verifier never travels to the SPA — only opaque `session_id` |
| Audit | `code`, `access_token`, `refresh_token`, `oauth_bundle` masked via `redact_secret_keys` |

## Client SDK

`crates/llm-auth` exposes the primitives so any consumer (admin
RPC, CLI wizard, future MCP server) shares the same crypto + HTTP
shape:

```rust
use nexo_llm_auth::{gen_pkce, StateEncoding};
use nexo_llm_auth::anthropic::{build_authorize_url, exchange_code, TOKEN_URL};

let pkce = gen_pkce(StateEncoding::HexOnly);
let url = build_authorize_url(&pkce);
// ... operator pastes `<code>#<state>` ...
let bundle = exchange_code(&pkce, &code, &state, TOKEN_URL).await?;
```

## CLI flow (unchanged)

`agent setup anthropic` → `oauth_login` mode still uses
`crates/setup/src/services/anthropic_oauth.rs` which now wraps
the same `nexo-llm-auth` primitives. Operators with shell access
keep the stdin paste UX.

## Microapp wizard

The SPA-side UI lives in
`agent-creator-microapp/frontend/src/components/OAuthPane.tsx`
and the zustand store
`agent-creator-microapp/frontend/src/lib/oauthFlow.ts`. State
machine: `idle → starting → awaiting_user → exchanging → success
| error`. Auth-code variant renders a paste box; device-code
variant renders the user_code + verification_uri with a Confirm
button (the daemon polls upstream).
