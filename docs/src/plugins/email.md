# Email plugin

Multi-account IMAP/SMTP channel for Nexo agents. Receives messages
through IMAP IDLE (with a 60 s polling fallback for servers that
don't speak IDLE), sends through SMTP under a circuit-breaker, and
exposes six tools (`email_send`, `email_reply`, `email_archive`,
`email_move_to`, `email_label`, `email_search`) so an agent can read
and act on a mailbox.

> **Status (Phase 48 closed).** IMAP `ImplicitTls` (port 993) is the
> only mode in v1; STARTTLS, multi-selector DKIM probe, persistent
> bounce history, and the interactive setup wizard are tracked in
> `proyecto/FOLLOWUPS.md` for follow-up phases.

## Configuration

`config/plugins/email.yaml` — multi-account schema. Credentials live
in `nexo-auth` (Phase 17), not in this YAML; see [Per-account
credentials](#per-account-credentials) below.

```yaml
email:
  enabled: true
  max_body_bytes: 32768           # body_text truncation
  max_attachment_bytes: 26214400  # 25 MiB; oversized attachments are
                                  # written truncated and flagged
  attachments_dir: data/email-attachments
  outbound_queue_dir: data/email-outbound
  poll_fallback_seconds: 60       # used when IDLE isn't supported
  idle_reissue_minutes: 28        # < RFC 2177's 29-minute ceiling
  spf_dkim_warn: true             # boot-time DNS check, non-fatal

  loop_prevention:
    auto_submitted: true          # RFC 3834
    list_headers: true            # List-Id / List-Unsubscribe / Precedence
    self_from: true               # bounce-back from our own outbound

  accounts:
    - instance: ops
      address: ops@example.com
      provider: custom            # gmail | outlook | yahoo | icloud | custom
      imap: { host: imap.example.com, port: 993, tls: implicit_tls }
      smtp: { host: smtp.example.com, port: 587, tls: starttls }
      folders:
        inbox:   INBOX
        sent:    Sent
        archive: Archive
      filters:
        from_allowlist: []
        from_denylist:  []
```

Topics: `plugin.inbound.email.<instance>` (parsed inbound),
`plugin.outbound.email.<instance>` (commands you publish to send),
`plugin.outbound.email.<instance>.ack` (per-message ack), and
`email.bounce.<instance>` (DSNs).

## Per-account credentials

`secrets/email/<instance>.toml` — `chmod 0o600` enforced at boot.
Three auth kinds are supported.

```toml
# Password (app password works fine for Outlook / iCloud / Yahoo).
[auth]
kind = "password"
username = "ops@example.com"
password = "${EMAIL_OPS_PASSWORD}"

# Pre-issued OAuth2 bearer (bring-your-own-token).
[auth]
kind = "oauth2_static"
username = "ops@gmail.com"
access_token  = "${EMAIL_OPS_TOKEN}"
refresh_token = "${EMAIL_OPS_REFRESH}"   # optional
expires_at    = 1735689600                # optional unix sec

# Reuse an account already in `config/plugins/google-auth.yaml`.
[auth]
kind = "oauth2_google"
username = "ops@gmail.com"
google_account_id = "ops"
```

`${ENV}` placeholders are resolved at boot via
`nexo_config::env::resolve_placeholders`. The OAuth2-Google variant
delegates token reads to the Google credential store and shares
its per-account refresh mutex so concurrent IMAP IDLE workers
never race a token rotation.

## Provider auto-detect

The setup helper `provider_hint(domain)` recognises five families
out of the box:

| Domain                                            | Provider | IMAP host                | SMTP host                |
| ------------------------------------------------- | -------- | ------------------------ | ------------------------ |
| `gmail.com`, `googlemail.com`                     | Gmail    | `imap.gmail.com:993`     | `smtp.gmail.com:587`     |
| `outlook.com`, `hotmail.com`, `live.com`, `msn.com` | Outlook | `outlook.office365.com:993` | `smtp.office365.com:587` |
| `yahoo.com`, `yahoo.co.uk`, `ymail.com`, `rocketmail.com` | Yahoo | `imap.mail.yahoo.com:993` | `smtp.mail.yahoo.com:587` |
| `icloud.com`, `me.com`, `mac.com`                 | iCloud   | `imap.mail.me.com:993`   | `smtp.mail.me.com:587`   |
| anything else                                     | Custom   | (prompt)                 | (prompt)                 |

Gmail addresses also get a `suggest_oauth_google = true` hint so
the wizard offers to reuse `google-auth.yaml` instead of asking
for an app password.

## Tools

The agent gets six tools when the email plugin is active:

| Tool             | Purpose                                                 |
| ---------------- | ------------------------------------------------------- |
| `email_send`     | Send a new message. `from` is pinned to the account address (anti-spoof). |
| `email_reply`    | Fetch the parent by UID, derive recipients (`reply_all` adds `parent.To/Cc` minus own), inherit `In-Reply-To` / `References`. |
| `email_archive`  | UID MOVE to the configured archive folder; falls back to `COPY + STORE \Deleted + EXPUNGE`. |
| `email_move_to`  | Same as archive but to an arbitrary folder (no auto-create). |
| `email_label`    | Gmail-only: `STORE +X-GM-LABELS` / `-X-GM-LABELS`. Errors on non-Gmail. |
| `email_search`   | Portable JSON DSL → IMAP SEARCH atoms. Default limit 50, max 200. |

Every result is wrapped in a `{ ok: bool, ... }` envelope. Errors
become `{ ok: false, error: "..." }` rather than thrown exceptions
so the agent doesn't have to branch on exception types.

`email_search` query shape:

```json
{
  "instance": "ops",
  "folder": "INBOX",
  "query": {
    "from": "alice@x", "to": "bob@x",
    "subject": "report", "body": "kpi",
    "since": "2024-01-01", "before": "2024-12-31",
    "unseen": true, "seen": false
  },
  "limit": 50
}
```

User-controlled strings pass through `imap_quote` (RFC 3501
quoted-string + CR/LF collapse) before reaching the wire — that's
the security boundary against atom injection.

Outbound attachments are referenced by file path; the dispatcher
reads the bytes at enqueue time so a missing file fails fast with
`ack: Failed` instead of parking a doomed job:

```json
{
  "instance": "ops",
  "to": ["alice@x"],
  "subject": "Report",
  "body": "see attached",
  "attachments": [
    { "data_path": "/tmp/q3.pdf", "filename": "q3.pdf" }
  ]
}
```

## Inbound events

Published as JSON on `plugin.inbound.email.<instance>`:

```jsonc
{
  "account_id": "ops@example.com",
  "instance": "ops",
  "uid": 42,
  "internal_date": 1700000000,
  "raw_bytes": "<.eml bytes (binary-safe via serde_bytes)>",
  "meta": {
    "message_id": "<abc@x>",
    "in_reply_to": "<parent@x>",
    "references": ["<root@x>", "<parent@x>"],
    "from": { "address": "alice@x", "name": "Alice Doe" },
    "to":   [{ "address": "ops@example.com" }],
    "cc":   [],
    "subject": "Re: hi",
    "body_text": "...",
    "body_html": null,
    "date": 1700000000,
    "headers_extra": { "list-id": "<l@x>" },
    "body_truncated": false
  },
  "attachments": [
    {
      "sha256": "abc...",
      "local_path": "data/email-attachments/abc...",
      "size_bytes": 4096,
      "mime_type": "application/pdf",
      "filename": "report.pdf",
      "disposition": "attachment",
      "truncated": false
    }
  ],
  "thread_root_id": "<root@x>"
}
```

`thread_root_id` is the canonical session key — pass it through
`session_id_for_thread()` (UUIDv5) to bridge into `nexo-core`'s
session map.

## Bounce events

Delivery reports never reach the LLM as conversational content.
They publish on `email.bounce.<instance>`:

```jsonc
{
  "account_id": "ops@example.com",
  "instance": "ops",
  "original_message_id": "<our-outbound@example.com>",
  "recipient": "ghost@unknown.com",
  "status_code": "5.1.1",
  "action": "failed",
  "reason": "smtp; 550 5.1.1 user unknown",
  "classification": "permanent"
}
```

`classification` follows SMTP convention: `5.x.x` → `permanent`,
`4.x.x` → `transient`, anything else → `unknown`. The detector
fires on a `Content-Type: multipart/report; report-type=delivery-
status` envelope; legacy Postfix / sendmail bounces without that
marker are caught via a `From` localpart heuristic
(`MAILER-DAEMON`, `mail-daemon`, `mail.daemon`, `postmaster`).

## Loop-prevention

After parse, before publish, the worker walks `LoopPreventionCfg`
in priority order and short-circuits on the first match:

| Reason            | Trigger                                                   |
| ----------------- | --------------------------------------------------------- |
| `auto_submitted`  | `Auto-Submitted` header is anything other than `no` (RFC 3834). |
| `list_mail`       | `List-Id` or `List-Unsubscribe` present (RFC 2369).       |
| `precedence_bulk` | `Precedence: bulk\|junk\|list` (RFC 2076).                |
| `self_from`       | Inbound `From` matches the account's own address.         |
| `dsn_inbound`     | `parse_bounce` returned `Some` (handled before loop walk). |

Each suppressed message advances the IMAP cursor — it has been
processed, just not surfaced.

## SPF / DKIM boot warns

When `spf_dkim_warn: true`, each account triggers a 3 s
non-blocking DNS lookup at start. WARN lines are
operator-actionable:

| Tag                                | Means                                                       |
| ---------------------------------- | ----------------------------------------------------------- |
| `email.spf.missing`                | No `v=spf1` TXT record at the apex of the From domain.      |
| `email.spf.misalignment`           | SPF policy exists but doesn't authorise the configured SMTP host. |
| `email.dkim.missing`               | No TXT at `default._domainkey.<domain>`. Try selectors `default`, `google`, `selector1`, `mail`. |
| `email.spf_dkim.dns_unavailable`   | The DNS lookup itself failed. Often transient.              |

DMARC, multi-selector DKIM rotation, and signature verification
are deliberately out of scope for v1.

## Troubleshooting

* **`email.idle.unsupported`** — the server doesn't advertise
  IDLE; the worker is permanently in 60 s polling mode. Yahoo
  Plus and some legacy IMAP servers behave this way.
* **`email.uidvalidity.changed`** — the mailbox was recreated
  server-side; the cursor reset to `last_uid=0` and every existing
  message will be processed again.
* **Outbound DLQ growing** — inspect
  `data/email-outbound/<instance>.dlq.jsonl`. After 5 transient
  attempts (or any 5xx) jobs land here; there's no auto-purge.
* **`email.auth.xoauth2_failed`** — the OAuth2 token was rejected.
  The worker retries once with a forced refresh; if it still
  fails the SMTP / IMAP circuit-breaker opens.
* **`EMAIL_INSECURE_TLS=1`** — disables TLS cert verification.
  Logged at WARN; only safe for fake servers / loopback.

## Limitations

| Deferred                              | Tracked in                                            |
| ------------------------------------- | ----------------------------------------------------- |
| IMAP STARTTLS (only `ImplicitTls` 993) | `proyecto/FOLLOWUPS.md`                              |
| Multi-selector DKIM probe             | `proyecto/FOLLOWUPS.md`                              |
| Persistent bounce history             | `proyecto/FOLLOWUPS.md`                              |
| Interactive setup wizard              | `proyecto/FOLLOWUPS.md`                              |
| greenmail e2e test harness            | `proyecto/FOLLOWUPS.md`                              |
| Email-specific Prometheus metrics     | `proyecto/FOLLOWUPS.md`                              |
| Phase 16 binding-policy auto-filter   | `proyecto/FOLLOWUPS.md`                              |
| HTML body in outbound                 | (text/plain only in v1)                              |
| `.ics` calendar invites               | Phase 65                                              |
| Vision OCR over attached images       | Phase 49                                              |
