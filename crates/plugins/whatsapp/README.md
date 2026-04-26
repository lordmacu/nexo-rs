# nexo-plugin-whatsapp

> WhatsApp channel plugin for Nexo — wraps [`wa-agent`](https://crates.io/crates/wa-agent) (Signal Protocol + QR pairing) so an agent can DM customers, friends, or its operator over WhatsApp without a Business API account.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Bridges `wa-agent`** — the underlying Signal Protocol
  implementation handles QR pairing, session keys, message
  encryption / decryption. This crate is the Nexo-shaped
  facade.
- **Inbound** — every received message becomes an
  `InboundMessage { sender, text, attachments }` published on
  `plugin.inbound.whatsapp[.<account>]`.
- **Outbound** — `plugin.outbound.whatsapp[.<account>]`
  consumer dispatches text / image / document / voice-note
  messages.
- **Voice-note transcription** — when the inbound message is
  an `audio/ogg` voice note, the plugin auto-transcribes via
  the configured LLM provider (Anthropic, OpenAI-compat,
  Whisper, …) and inlines the text in the published payload.
- **QR pairing** — first-run rendering on stderr ANSI + PNG
  output for hands-off setups. Phase 26 challenge-gate
  integration.
- **Lifecycle health** — heartbeat + reconnect on session
  drop, so a phone losing WiFi recovers without a restart.
- **Per-agent credentials** — Phase 17 `nexo-auth` resolver
  picks the device session per agent so multi-tenant setups
  serve N customer phones from one daemon.
- **Pairing protocol adapter** — implements
  `PairingChannelAdapter` (strips `@c.us` + `@s.whatsapp.net`
  suffixes, prepends `+` for E.164 normalisation).

## Configuration

```yaml
plugins:
  whatsapp:
    instance: primary
    workspace_dir: /var/lib/nexo/whatsapp/primary
    qr_png: /var/lib/nexo/whatsapp/qr.png  # optional
    transcribe:
      enabled: true
      provider: anthropic
      model: claude-haiku-4
```

## Wire format

Inbound:

```json
{
  "channel": "whatsapp",
  "instance": "primary",
  "sender": "+5491155556666",
  "text": "...",
  "attachments": [
    { "kind": "image", "url": "data:image/jpeg;base64,..." }
  ]
}
```

Outbound:

```json
{ "kind": "text", "to": "+5491155556666", "text": "..." }
{ "kind": "image", "to": "+...", "caption": "...", "data": "data:image/...;base64,..." }
```

## Install

```toml
[dependencies]
nexo-plugin-whatsapp = "0.1"
```

## Documentation for this crate

- [WhatsApp plugin guide](https://lordmacu.github.io/nexo-rs/plugins/whatsapp.html)
- [WhatsApp via whatsapp-rs (ADR 0007)](https://lordmacu.github.io/nexo-rs/adr/0007-whatsapp-signal-protocol.html)
- [Pairing protocol](https://lordmacu.github.io/nexo-rs/ops/pairing.html)
- [WhatsApp sales agent recipe](https://lordmacu.github.io/nexo-rs/recipes/whatsapp-sales-agent.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
