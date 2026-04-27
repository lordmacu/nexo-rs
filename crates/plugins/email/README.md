# nexo-plugin-email

> Multi-account IMAP / SMTP channel for [Nexo](https://github.com/lordmacu/nexo-rs) agents.

This crate ships the email channel plugin: IMAP IDLE inbound (with a
60 s polling fallback), SMTP outbound under a CircuitBreaker, MIME
parse + multipart build (`mail-parser` + `mail-builder`), six
agent-callable tools (`email_send`, `email_reply`, `email_archive`,
`email_move_to`, `email_label`, `email_search`), threading via
`Message-ID` / `In-Reply-To` / `References` (Phase 48.6 UUIDv5
session ids), loop-prevention against auto-replies / list mail /
self-bounces, and DSN/bounce parsing into a dedicated
`email.bounce.<instance>` topic.

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Operator docs (mdBook):** <https://lordmacu.github.io/nexo-rs/plugins/email.html>

## Status

Phase 48 closed. v1 ships IMAP `ImplicitTls` (port 993) only.
STARTTLS for IMAP, multi-selector DKIM probe, persistent bounce
history, the interactive setup wizard, and a greenmail e2e harness
are tracked in [`proyecto/FOLLOWUPS.md`](https://github.com/lordmacu/nexo-rs/blob/main/proyecto/FOLLOWUPS.md)
for follow-up phases.

## Quickstart

Drop a YAML block under `config/plugins/email.yaml` and a TOML
secret under `secrets/email/<instance>.toml` (mode `0o600`). Full
schema, secret format, tool catalog, inbound/bounce wire formats,
loop-prevention rules and SPF/DKIM behaviour live in the operator
docs at the link above.

```yaml
email:
  enabled: true
  spf_dkim_warn: true
  accounts:
    - instance: ops
      address: ops@example.com
      provider: custom
      imap: { host: imap.example.com, port: 993, tls: implicit_tls }
      smtp: { host: smtp.example.com, port: 587, tls: starttls }
```

## When to use this crate vs not

- ✅ Customer-support agent triaging `support@yourdomain.com`.
- ✅ Personal agent that summarises your inbox and drafts replies.
- ✅ Async batch flows (form submissions, receipts).
- ❌ Real-time chat — IMAP latency is 30-120 s. Use the WhatsApp or
  Telegram plugins for sub-second.
- ❌ One-off transactional sends from a marketing pipeline — reach
  for SES / Postmark directly.

## License

Licensed under either of:

- Apache License, Version 2.0
  ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license
  ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
