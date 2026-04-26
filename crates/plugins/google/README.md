# nexo-plugin-google

> Google API channel plugin for Nexo — OAuth (loopback + device flow) + the `google_*` tool family covering Gmail, Calendar, Drive, Sheets.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **OAuth 2.0 client** with two flows:
  - **Loopback** — opens a browser, handles the redirect on a
    short-lived local listener. Works on desktop / SSH-tunnel
    setups.
  - **Device flow** — prints a URL + user-code; operator types
    the code into any second device. Termux + headless setups.
- **Token storage + refresh** — encrypted at rest under
  `<workspace>/google-tokens.json` (mode 0600, configurable).
  Auto-refreshes a few minutes before expiry; surfaces
  `re-auth required` when Google revokes.
- **`google_*` tools** — `google_email_send`,
  `google_email_search`, `google_calendar_list`,
  `google_calendar_create`, `google_drive_search`,
  `google_drive_get`, `google_sheets_read`, …. Each delegates
  to `authorized_call(method, url, body)` which wraps the HTTP
  call in a per-instance `CircuitBreaker`.
- **Per-agent credentials** — Phase 17 `nexo-auth` resolver
  picks the right OAuth bundle per agent, so a multi-tenant
  setup serves N customer Google accounts from one daemon.
- **All 6 OAuth endpoints breakered** — `exchange_code`,
  `request_device_code`, `poll_device_token`,
  `refresh_if_needed`, `revoke`, and the general
  `authorized_call` all flow through the same breaker so a
  Google-wide outage doesn't keep retrying.

## Configuration

```yaml
plugins:
  google:
    client_id_file: /run/secrets/google_client_id
    client_secret_file: /run/secrets/google_client_secret
    token_file: google-tokens.json     # relative to workspace
    scopes:
      - https://www.googleapis.com/auth/calendar
      - https://www.googleapis.com/auth/gmail.send
      - https://www.googleapis.com/auth/drive.readonly
```

## Tool surface

| Tool | Endpoint | Common use |
|---|---|---|
| `google_email_send` | gmail.users.messages.send | Outbound email via Gmail account |
| `google_email_search` | gmail.users.messages.list | "find emails about X" |
| `google_calendar_list` | calendar.events.list | "what's on my calendar today" |
| `google_calendar_create` | calendar.events.insert | "schedule a meeting" |
| `google_drive_search` | drive.files.list | "find that doc about Q3" |
| `google_drive_get` | drive.files.get | "read the contents of file X" |
| `google_sheets_read` | sheets.spreadsheets.values.get | "what's in column B of …" |

## Install

```toml
[dependencies]
nexo-plugin-google = "0.1"
```

## Documentation for this crate

- [Google plugin guide](https://lordmacu.github.io/nexo-rs/plugins/google.html)
- [Per-agent credentials](https://lordmacu.github.io/nexo-rs/config/credentials.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
