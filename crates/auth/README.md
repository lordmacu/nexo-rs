# nexo-auth

> Per-agent credential resolver + boot gauntlet for Nexo channel plugins. Lets a multi-tenant deployment ship one daemon serving N customers with isolated keys.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Per-channel credential stores** — independent stores for
  WhatsApp, Telegram, and Google APIs. Each agent ships with
  its own keys; the resolver picks the right credential at
  runtime based on the agent ID + channel.
- **Boot gauntlet** — at daemon start, validates required
  scopes / pairing state across every configured agent before
  the runtime accepts traffic. Surfaces missing scopes
  (Google), invalid bot tokens (Telegram), missing device
  pairings (WhatsApp) up front instead of failing on the first
  inbound.
- **`AgentCredentialResolver`** — the runtime hands a
  `(agent_id, channel)` pair to the resolver and gets back the
  matching `Credentials`. Channel plugins call this on every
  intake / outbound dispatch.
- **`BreakerRegistry`** — maps `(channel, instance)` →
  `Arc<CircuitBreaker>` so a flapping account doesn't bring
  down the others.
- **Telemetry hooks** — counters per credential resolution
  (cache hit / miss / refresh) + audit log lines so operators
  can see which agent used which credential when.
- **Hot-reload aware** — Phase 18 `RuntimeSnapshot` swaps the
  resolver atomically; in-flight turns finish on the old
  credentials, the next intake reads the new one.

## Public API

```rust
pub struct AgentCredentialResolver { /* … */ }

impl AgentCredentialResolver {
    pub async fn resolve(
        &self,
        agent_id: &str,
        channel: Channel,
    ) -> Result<Credentials, AuthError>;
}

pub enum Channel { Whatsapp, Telegram, Google }
```

## Configuration

Per-agent credentials live in `config/agents.yaml`:

```yaml
agents:
  - id: kate
    credentials:
      telegram:
        bot_token_file: /run/secrets/kate_tg_token
      google:
        oauth_token_file: /var/lib/nexo/kate_google_oauth.json
        scopes: [calendar, gmail.send]
```

The resolver gracefully falls back to the legacy
`plugins.<channel>` block when an agent doesn't override
explicitly.

## Install

```toml
[dependencies]
nexo-auth = "0.1"
```

## Documentation for this crate

- [Per-agent credentials](https://lordmacu.github.io/nexo-rs/config/credentials.html)
- [Capability toggles](https://lordmacu.github.io/nexo-rs/ops/capabilities.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
