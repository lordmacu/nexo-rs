# llm.yaml

LLM provider registry. Each agent's `model.provider` must resolve to a
key in this file.

Source: `crates/config/src/types/llm.rs`.

## Shape

```yaml
providers:
  minimax:
    api_key: ${MINIMAX_API_KEY:-}
    group_id: ${MINIMAX_GROUP_ID:-}
    base_url: https://api.minimax.io
    rate_limit:
      requests_per_second: 2.0
      quota_alert_threshold: 100000
  anthropic:
    api_key: ${ANTHROPIC_API_KEY:-}
    base_url: https://api.anthropic.com
    rate_limit:
      requests_per_second: 2.0
    auth:
      mode: oauth_bundle
      bundle: ./secrets/anthropic_oauth.json
retry:
  max_attempts: 5
  initial_backoff_ms: 1000
  max_backoff_ms: 60000
  backoff_multiplier: 2.0
```

## Per-provider fields

| Field | Type | Required | Default | Purpose |
|-------|------|:-------:|---------|---------|
| `api_key` | string | ✅ | — | API key. Supports `${ENV_VAR}` and `${file:…}`. |
| `base_url` | url | ✅ | — | API endpoint. Override to use a proxy or a local server. |
| `group_id` | string | — | — | **MiniMax-only.** Group identifier. |
| `rate_limit.requests_per_second` | f64 | — | `2.0` | Outbound throttle. |
| `rate_limit.quota_alert_threshold` | u64 | — | — | Optional soft-alarm tokens-per-day threshold. |
| `api_flavor` | enum | — | `openai_compat` | `openai_compat` or `anthropic_messages`. Lets MiniMax expose the Anthropic wire. |
| `embedding_model` | string | — | — | Override model used for embeddings (e.g. Gemini's `text-embedding-004`). |
| `safety_settings` | JSON | — | — | Gemini-only; attached verbatim to requests. |

## Top-level retry block

Applies to every provider that doesn't define its own:

| Field | Default | Purpose |
|-------|---------|---------|
| `max_attempts` | `5` | Total attempts including the first try. |
| `initial_backoff_ms` | `1000` | First backoff. |
| `max_backoff_ms` | `60000` | Cap. |
| `backoff_multiplier` | `2.0` | Exponential factor. |

Retries are **jittered** to avoid thundering-herd reconnects. See
[Fault tolerance — Retry policies](../architecture/fault-tolerance.md#retry-policies).

## Auth modes

```yaml
auth:
  mode: auto | static | token_plan | oauth_bundle
  bundle: ./secrets/anthropic_oauth.json
  setup_token_file: ./secrets/anthropic_setup.json
  refresh_endpoint: https://auth.example.com/refresh
  client_id: your-oauth-client
```

| `mode` | When |
|--------|------|
| `auto` | Let the provider client decide from available credentials. |
| `static` | Use `api_key` verbatim. |
| `token_plan` | MiniMax "Token Plan" OAuth bundle. |
| `oauth_bundle` | Anthropic PKCE OAuth bundle written by `agent setup`. |

## Supported providers

| Key | Notes |
|-----|-------|
| `minimax` | Primary provider. MiniMax M2.5. OpenAI-compat or Anthropic-flavour wire. |
| `anthropic` | Claude models. API key **or** OAuth subscription. |
| `openai` | OpenAI API and anything speaking its wire (Ollama, Groq, local proxies). |
| `gemini` | Google Gemini, including embedding support. |

## Provider-specific docs

- [MiniMax M2.5](../llm/minimax.md)
- [Anthropic / Claude](../llm/anthropic.md)
- [OpenAI-compatible](../llm/openai.md)
- [Rate limiting & retry](../llm/retry.md)

## Common mistakes

- **`api_key: sk-…` committed to git.** Use `${ENV_VAR}` or
  `${file:./secrets/…}`; the `secrets/` directory is gitignored.
- **Mismatched `embedding_model` dimensions.** The vector store
  asserts `embedding.dimensions` matches the model output. A mismatch
  aborts startup with an explicit message.
- **Setting both `api_key` and `auth.mode: oauth_bundle`.** The auth
  mode wins. The `api_key` is kept as a fallback for tools that bypass
  the OAuth path.
