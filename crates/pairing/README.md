# nexo-pairing

> Phase 26 — pairing protocol primitives for Nexo. **DM-challenge gate** for inbound allowlisting + **setup-code issuer** (HMAC-signed bootstrap tokens + QR rendering) for hands-off companion-app pairing.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

### DM-challenge gate (opt-in inbound allowlist)

- **`PairingStore`** — SQLite-backed; `pairing_pending`
  (TTL 60 min) + `pairing_allow_from` (durable, soft-delete
  on revoke).
- **`PairingGate::should_admit`** — consulted on the runtime
  intake hot path; with `auto_challenge: true` an unknown
  sender gets a one-time human-friendly code and the operator
  approves via `nexo pair approve`.
- **`PairingChannelAdapter` trait** + per-channel impls for
  WhatsApp + Telegram. Normalises sender ids
  (`+5491155556666@c.us` → `+5491155556666`) so cache keys
  match the canonical form, and delivers challenge messages
  through the channel's outbound topic.
- **`PairingAdapterRegistry`** — `channel_id → Arc<dyn
  PairingChannelAdapter>` lookup the runtime owns.

### Setup-code (operator-initiated)

- **`SetupCodeIssuer::issue`** — HMAC-signed bootstrap token
  + URL packed into a base64url payload. Companion app
  decodes via `decode_setup_code`.
- **QR rendering** — text (`render_ansi`) for terminals + PNG
  (`render_png` behind the `qr-png` feature) for hands-off
  scanning.
- **Token expiry** — short-lived (60s default, configurable
  via `pairing.yaml`); claim-side `expires_at` baked into
  the JWT-shaped token so a leaked code can't be replayed.
- **URL resolver** — priority chain (`--public-url` →
  `pairing.yaml` → `NEXO_TUNNEL_URL` → `$NEXO_HOME/state/
  tunnel.url` sidecar → loopback fail-closed). Cleartext
  `ws://` only on hosts in the allow-list (loopback,
  RFC1918, link-local, `.local`, `10.0.2.2`).

### Telemetry

- `pairing_requests_pending{channel}` gauge
- `pairing_approvals_total{channel,result}` counter
- `pairing_codes_expired_total` counter
- `pairing_bootstrap_tokens_issued_total{profile}` counter
- `pairing_inbound_challenged_total{channel,result}` counter

## Public API

```rust
pub struct PairingStore { /* … */ }
pub struct PairingGate { /* … */ }
pub struct PairingAdapterRegistry { /* … */ }
pub struct SetupCodeIssuer { /* … */ }

pub trait PairingChannelAdapter: Send + Sync {
    fn channel_id(&self) -> &'static str;
    fn normalize_sender(&self, raw: &str) -> Option<String>;
    fn format_challenge_text(&self, code: &str) -> String { /* default */ }
    async fn send_reply(&self, account: &str, to: &str, text: &str) -> Result<()>;
    async fn send_qr_image(&self, _account: &str, _to: &str, _png: &[u8]) -> Result<()> { /* default bail */ }
}

pub fn encode_setup_code(payload: &SetupCode) -> Result<String>;
pub fn decode_setup_code(code: &str) -> Result<SetupCode>;
pub fn token_expires_at(token: &str) -> Option<DateTime<Utc>>;
```

## Configuration

```yaml
# config/pairing.yaml (optional; absent = legacy hardcoded paths)
pairing:
  storage:
    path: /var/lib/nexo/pairing/pairing.db
  setup_code:
    secret_path: /var/lib/nexo/pairing/pairing.key
    default_ttl_secs: 600
  public_url: wss://nexo.example.com/pair
  ws_cleartext_allow:
    - kitchen-pi.local
```

## Install

```toml
[dependencies]
nexo-pairing = "0.1"
```

Disable PNG rendering if only text QR is needed:

```toml
nexo-pairing = { version = "0.1", default-features = false }
```

## Documentation for this crate

- [Pairing protocol](https://lordmacu.github.io/nexo-rs/ops/pairing.html)
- [Companion CLI scaffold](https://github.com/lordmacu/nexo-rs/tree/main/crates/companion-tui)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
