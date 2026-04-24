# Anthropic / Claude

Native Anthropic client with multiple authentication paths: static
API key, setup tokens, full OAuth PKCE subscription flow, or automatic
import from the local Claude Code CLI.

Source: `crates/llm/src/anthropic.rs`, `crates/llm/src/anthropic_auth.rs`.
Phase 15 added the subscription flow end-to-end.

## Configuration

```yaml
# config/llm.yaml
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY:-}
    base_url: https://api.anthropic.com
    rate_limit:
      requests_per_second: 2.0
    auth:
      mode: oauth_bundle
      bundle: ./secrets/anthropic_oauth.json
```

Per-agent selection:

```yaml
model:
  provider: anthropic
  model: claude-haiku-4-5
```

## Authentication modes

| `auth.mode` | Credential | Header |
|-------------|------------|--------|
| `static` | `api_key` (`sk-ant-…`) | `x-api-key: <key>` |
| `setup_token` | `sk-ant-oat01-…` (min 80 chars) | `Authorization: Bearer <key>` + `anthropic-beta: oauth-2025-04-20` |
| `oauth_bundle` | `{access, refresh, expires_at}` JSON | `Authorization: Bearer <access>` |
| `auto` | tries all of the above in order | — |

### `auto` resolution order

Used when `auth.mode: auto` or omitted:

```mermaid
flowchart TD
    START[anthropic client build] --> B1{oauth_bundle<br/>file exists?}
    B1 -->|yes| USE1[use OAuth bundle]
    B1 -->|no| B2{Claude Code CLI<br/>credentials found?}
    B2 -->|yes| USE2[import from<br/>~/.claude/.credentials.json]
    B2 -->|no| B3{setup_token<br/>file exists?}
    B3 -->|yes| USE3[use setup token]
    B3 -->|no| B4{api_key<br/>set?}
    B4 -->|yes| USE4[use static key]
    B4 -->|no| FAIL([fail: no credentials])
```

### OAuth bundle

The wizard runs a PKCE flow in the browser and writes the bundle to
`./secrets/anthropic_oauth.json`:

```json
{
  "access_token": "...",
  "refresh_token": "...",
  "expires_at": "2026-05-01T12:00:00Z"
}
```

- **Refresh endpoint:** `https://console.anthropic.com/v1/oauth/token`
- **Refresh cadence:** 60 seconds before `expires_at`, background task
  POSTs `grant_type=refresh_token`
- **Concurrency:** all refreshes serialize behind a mutex
- **Shared OAuth client id:** `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
- **Stale-token handling:** a 401 mid-flight marks the token stale so
  the next refresh fires immediately instead of waiting for the
  expiry window

### CLI credentials import

If you're already running Claude Code CLI on the same host, the client
auto-detects and imports `~/.claude/.credentials.json`. Zero config —
if it exists and is valid, it's used.

## Tool calling

Native Anthropic shape:

- Tool definitions: `{name, description, input_schema}`
- Tool invocation: `tool_use` blocks with `id`, `name`, `input`
- Tool result: `tool_result` blocks correlated via `tool_use_id`

Streaming uses native SSE; a dedicated parser in
`crates/llm/src/stream.rs` handles `message_start`, `content_block_*`,
and `message_delta` events.

## Error classification

| Response | Mapping | Behavior |
|----------|---------|----------|
| 429 | `LlmError::RateLimit { retry_after_ms }` (fallback 60s) | Retried |
| 401 / 403 | `LlmError::CredentialInvalid` with context (API vs OAuth) | Marks OAuth token stale; fails fast so the operator sees it |
| 5xx | `LlmError::ServerError` | Retried |
| Other 4xx | `LlmError::Other` | Fail fast |

## Supported features

- Chat completions ✅
- Tool calling ✅
- Streaming (SSE) ✅
- Multimodal (images) ✅
- Prompt caching ✅ (via Anthropic beta headers)
- Extended thinking ✅ (model-dependent)

## Common mistakes

- **Setup-token string under 80 chars.** The setup-token validator
  refuses it at parse time. Make sure you pasted the full string.
- **`api_key` + `oauth_bundle` both set.** The auth mode wins. The
  static key is kept only as a fallback the auto-resolver may pick up
  if the bundle is missing.
- **Claude Code CLI credentials being used unintentionally.** If
  `auto` mode is on and you installed CLI on the host, that path wins
  before `api_key`. Set `auth.mode: static` to pin the static key.
