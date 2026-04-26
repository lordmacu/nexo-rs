# nexo-plugin-telegram

> Telegram bot channel plugin for Nexo — long-poll inbound + send-tools outbound, with MarkdownV2 escaping and circuit-breaker hardening.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Long-poll `getUpdates`** loop — pulls new messages from the
  Telegram Bot API, normalises them, and publishes on
  `plugin.inbound.telegram[.<instance>]`.
- **Outbound send tool** — consumes
  `plugin.outbound.telegram[.<instance>]` and dispatches
  `sendMessage`, `sendPhoto`, `sendDocument`,
  `editMessageText`, `setMessageReaction`.
- **MarkdownV2-aware** — automatically escapes Telegram's reserved
  chars (`_*[]()~`>#+-=|{}.!`) when `parse_mode: MarkdownV2` is
  set, so the agent doesn't have to think about escaping.
- **UTF-16-aware truncation** — `MAX_TEXT_LEN` (4096 UTF-16 units)
  + `MAX_CAPTION_LEN` (1024) match Telegram's wire limits.
- **`split_text` helper** — large outbound texts split on newline
  boundaries first, hard-split otherwise.
- **CircuitBreaker on every HTTP call** — JSON POST, multipart
  `sendDocument`, and `download_file` all flow through
  [`nexo-resilience`](https://github.com/lordmacu/nexo-rs/tree/main/crates/resilience).
- **Per-agent credentials** — Phase 17 `nexo-auth` resolver picks
  the bot token per agent, so multi-tenant deployments don't have
  to share one bot.
- **Pairing-protocol adapter** — implements
  `PairingChannelAdapter` so Phase 26 challenge messages format
  correctly + escape MarkdownV2.

## Configuration

```yaml
plugins:
  telegram:
    instance: primary
    bot_token_file: /run/secrets/tg_bot_token
    poll_timeout_secs: 30
    parse_mode: MarkdownV2          # MarkdownV2 | Markdown | HTML | none
```

Wire format (inbound):

```json
{
  "channel": "telegram",
  "instance": "primary",
  "sender": "@username",
  "chat_id": 123456789,
  "text": "..."
}
```

## Install

```toml
[dependencies]
nexo-plugin-telegram = "0.1"
```

## Documentation for this crate

- [Telegram plugin guide](https://lordmacu.github.io/nexo-rs/plugins/telegram.html)
- [Per-agent credentials](https://lordmacu.github.io/nexo-rs/config/credentials.html)
- [Pairing protocol](https://lordmacu.github.io/nexo-rs/ops/pairing.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
