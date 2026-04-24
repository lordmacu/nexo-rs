# Email

Generic SMTP/IMAP plugin. **Scaffolded but not yet wired** — config
shape is defined, but no tool surface or inbound bridge ships today.
For a working Gmail → agent pipeline today, use
[gmail-poller](./google.md#gmail-poller).

Source: `crates/plugins/email/` (empty `lib.rs`),
config in `crates/config/src/types/plugins.rs`.

## Config

```yaml
# config/plugins/email.yaml
email:
  smtp:
    host: smtp.example.com
    port: 587
    username: agent@example.com
    password: ${file:./secrets/email_password.txt}
  imap:
    host: imap.example.com
    port: 993
```

| Field | Default | Purpose |
|-------|---------|---------|
| `smtp.host` | — (required) | SMTP server. |
| `smtp.port` | `587` | SMTP port. |
| `smtp.username` | — (required) | SMTP auth user. |
| `smtp.password` | — (required) | SMTP auth password. |
| `imap.host` | — | IMAP server (inbound). |
| `imap.port` | `993` | IMAP port. |

## Status

- No NATS topics active
- No tools exposed to the LLM
- No inbound bridge
- Config schema reserved so future phases can land incrementally

## What to use instead

For inbound triage:

- [gmail-poller](./google.md#gmail-poller) — cron-style Gmail
  polling with regex capture groups and template-based dispatch to
  any `plugin.outbound.*` topic. Production-ready.

For outbound notifications:

- Delegate to a send agent wired to a transactional-email provider
  via a custom extension, until this plugin lands.

Track progress under the future Phase 17 in `../PHASES.md`.
