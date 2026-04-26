# nexo-plugin-email

> Email channel plugin for Nexo — IMAP inbound + SMTP outbound, with native AWS SES adapter.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **Inbound IMAP polling** — connects to a Gmail / Office365 / generic
  IMAP server, fetches new messages since the last cursor, normalises
  them into `InboundMessage { sender, text, attachments }`, and
  publishes on `plugin.inbound.email[.<account>]`.
- **Outbound SMTP send** — consumes `plugin.outbound.email[.<account>]`
  and dispatches messages via SMTP+STARTTLS or implicit TLS.
- **AWS SES adapter** — when `provider: ses` is configured, uses
  the EC2 instance role / `~/.aws/credentials` instead of stuffing
  SMTP creds into YAML.
- **Threading-aware** — preserves `In-Reply-To` + `References`
  headers so reply chains stay threaded in the recipient's inbox.
- **Attachment support** — base64-decodes inbound attachments into
  `data/attachments/<sha256>` (deduped) and references them by
  digest in the inbound payload.
- **Per-agent credentials** — Phase 17 `nexo-auth` resolver picks
  the mailbox keys per agent, so a multi-tenant deployment can
  ship one daemon serving N customer inboxes.

## Configuration

```yaml
# config/plugins/email.yaml (or inline under plugins:)
plugins:
  email:
    instance: primary
    inbox:
      provider: imap                # imap | gmail-oauth
      host: imap.gmail.com
      port: 993
      username: ${EMAIL_USER}
      password_file: /run/secrets/email_password
      poll_interval_secs: 60
    outbox:
      provider: smtp                # smtp | ses
      host: smtp.gmail.com
      port: 587
      username: ${EMAIL_USER}
      password_file: /run/secrets/email_password
      from: "agent@yourdomain.com"
```

For SES with EC2 instance role:

```yaml
plugins:
  email:
    outbox:
      provider: ses
      aws_region: us-east-1
      from: "agent@yourdomain.com"
```

## Wire format

Inbound published on `plugin.inbound.email[.<instance>]`:

```json
{
  "channel": "email",
  "instance": "primary",
  "sender": "user@example.com",
  "text": "...",
  "subject": "...",
  "in_reply_to": "...",
  "attachments": [
    { "sha256": "...", "filename": "report.pdf", "mime": "application/pdf" }
  ]
}
```

Outbound consumed on `plugin.outbound.email[.<instance>]`:

```json
{ "kind": "email", "to": "user@example.com", "subject": "...", "text": "..." }
```

## Install

```toml
[dependencies]
nexo-plugin-email = "0.1"
```

## When to use this crate vs not

- ✅ Customer support agent that triages support@yourdomain.com.
- ✅ Personal agent that summarises your inbox + drafts replies.
- ✅ Async batch flows (form submissions / receipts).
- ❌ Real-time chat — IMAP poll latency is 30-120s. Use WhatsApp
  or Telegram for sub-second.
- ❌ One-off transactional sends — use AWS SES SDK or a transactional
  provider directly.

## Documentation for this crate

- [Email plugin guide](https://lordmacu.github.io/nexo-rs/plugins/email.html)
- [Per-agent credentials](https://lordmacu.github.io/nexo-rs/config/credentials.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
