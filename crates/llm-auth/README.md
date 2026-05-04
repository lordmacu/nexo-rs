# nexo-llm-auth

Reusable OAuth + PKCE primitives for Nexo LLM providers.

This crate exists so the same OAuth code that powers `agent setup
anthropic` (interactive CLI wizard) is also callable from the
`nexo/admin/llm_providers/oauth_*` admin RPC surface.

## What lives here

- **`pkce`** — PKCE verifier/challenge generation, percent-encoding,
  and code-payload parsing. Supports the two state-encoding flavors
  the upstream providers tolerate (Anthropic insists on hex-only,
  MiniMax accepts base64url).
- **`anthropic`** — Anthropic Claude.ai authorization-code OAuth flow:
  build the authorize URL and exchange the code for a refreshing
  bundle.
- **`minimax`** — MiniMax Token-Plan device-code OAuth flow: request a
  user code, then poll the token endpoint until the user approves.
- **`bundle`** — Typed `OAuthBundle { access_token, refresh_token,
  expires_at, account_email, source }` plus an atomic file persister.
- **`verifier_store`** — In-memory PKCE verifier session store with
  TTL sweep + LRU eviction, used by the admin RPC dispatcher to
  resume an OAuth flow across two HTTP requests (`oauth_start` →
  `oauth_finish`).

## What does NOT live here

- The interactive CLI glue (browser open, stdin paste loop) stays in
  `crates/setup/src/services/anthropic_oauth.rs` — this crate is
  pure async / no terminal I/O.
- Provider HTTP clients (`crates/llm/src/anthropic.rs` etc) — they
  consume `OAuthBundle` and refresh it; this crate only **acquires**
  the bundle.

## Stability

`#[non_exhaustive]` on every public struct + enum so adding fields
to the OAuth bundles or schema descriptors is non-breaking.
