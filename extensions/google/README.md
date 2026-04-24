# Google Workspace Extension (Rust)

Gmail / Calendar / Tasks / Drive client over Google's REST APIs with
OAuth 2.0 user refresh-token flow. **21 tools** across 4 services. Reads
are unrestricted; writes are gated per-service via env flags.

## Auth

OAuth 2.0 "installed app" / user flow. The operator obtains a refresh
token once and wires it through env vars:

- `GOOGLE_CLIENT_ID` — OAuth client id from Google Cloud Console
- `GOOGLE_CLIENT_SECRET` — OAuth client secret
- `GOOGLE_REFRESH_TOKEN` — long-lived refresh token from the initial consent

The extension exchanges the refresh token for an access token at
`https://oauth2.googleapis.com/token` (override with `GOOGLE_OAUTH_TOKEN_URL`
for tests). Access tokens are cached in-process until ~60s before expiry.

**Service accounts are not used here.** Personal Gmail accounts can't
authenticate via service account without domain-wide delegation (Workspace
only). User flow works for both personal and Workspace accounts.

## Tools (piloto)

- `status` — credential presence, endpoints, write-flag visibility
- `gmail_list` — `max_results` 1..500 (default 20), Gmail query language
  in `query`, `label_ids`, `page_token`, `include_spam_trash`

Returns `{count, messages: [{id, thread_id}], next_page_token, estimate_total}`.
Metadata only — full bodies come in the next sub-phase's `gmail_read`.

## Write-flag gate (forward-looking)

Future write tools (`gmail_send`, `calendar_create_event`, `tasks_add`, ...)
will be gated by per-service env flags:

- `GOOGLE_ALLOW_SEND` — gmail send + modify labels
- `GOOGLE_ALLOW_CALENDAR_WRITE` — calendar create/update/delete
- `GOOGLE_ALLOW_TASKS_WRITE` — tasks create/complete/delete

The `status` tool already exposes their current values so an operator can
see what the agent is allowed to do.

## Error codes

- -32011 unauthorized / refresh failed
- -32012 forbidden (scope missing)
- -32001 not found
- -32013 rate limited (with retry_after_secs in message)
- -32602 bad input / max_results out of range
- -32003 transport / HTTP 5xx after retries
- -32004 circuit open
- -32005 timeout
- -32006 invalid json

## Tests

9 integration tests via wiremock:
- status with credentials set
- gmail_list refreshes token + returns id list
- gmail_list propagates `q` query
- refresh token failure → -32011 unauthorized
- gmail 401 → -32011
- gmail 429 → -32013 with retry_after
- max_results out-of-range rejected locally
- missing refresh_token → -32011
- unknown tool → -32601

## Env

- `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` / `GOOGLE_REFRESH_TOKEN`
- `GOOGLE_OAUTH_TOKEN_URL` (default `https://oauth2.googleapis.com/token`)
- `GOOGLE_GMAIL_URL` (default `https://gmail.googleapis.com/gmail/v1`)
- `GOOGLE_HTTP_TIMEOUT_SECS` default 15
