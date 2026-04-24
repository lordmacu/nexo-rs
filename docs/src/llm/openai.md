# OpenAI-compatible

Client for OpenAI itself **and** for any upstream that speaks the same
wire: Ollama, Groq, OpenRouter, LM Studio, vLLM, Azure OpenAI, or your
own proxy.

Source: `crates/llm/src/openai_compat.rs`.

## Configuration

```yaml
# config/llm.yaml
providers:
  openai:
    api_key: ${OPENAI_API_KEY:-}
    base_url: https://api.openai.com/v1
    rate_limit:
      requests_per_second: 2.0
```

Per-agent:

```yaml
model:
  provider: openai
  model: gpt-4o
```

## Known-working upstreams

Point `base_url` at any of these and it works out of the box:

| Upstream | `base_url` |
|----------|------------|
| OpenAI | `https://api.openai.com/v1` |
| Ollama | `http://localhost:11434/v1` |
| Groq | `https://api.groq.com/openai/v1` |
| OpenRouter | `https://openrouter.ai/api/v1` |
| LM Studio | `http://localhost:1234/v1` |
| vLLM | `http://<host>:<port>/v1` |
| Azure OpenAI | Azure resource URL (watch for differences) |
| MiniMax (compat mode) | `https://api.minimax.io` |

## Authentication

Single mode: static API key sent as `Authorization: Bearer <key>`.
Some upstreams ignore the key entirely (Ollama, local vLLM) — supply
any non-empty string to satisfy the config validator.

## Features & gaps

| Feature | Status |
|---------|--------|
| Chat completions | ✅ |
| Tool calling | ✅ (OpenAI function-calling shape) |
| Streaming | ✅ |
| `tool_choice: auto \| required \| none \| {type:function}` | ✅ |
| JSON mode / structured outputs | upstream-dependent |
| Multimodal | upstream-dependent |
| Embeddings | supported for OpenAI proper; other upstreams may vary |

**Feature gating when the upstream lacks support:** we do not pre-probe
features — a call that requires a feature the upstream doesn't speak
will fail with the upstream's own error (typically a 400). The error
bubbles up as `LlmError::Other` and **does not retry**, so you notice
quickly.

## Error classification

| Response | Mapping | Behavior |
|----------|---------|----------|
| 429 | `LlmError::RateLimit` (fallback 30s) | Retried |
| 5xx | `LlmError::ServerError` | Retried |
| Other 4xx | `LlmError::Other` | Fail fast |

## Common mistakes

- **Trailing slash in `base_url`.** Some upstreams are lenient, some
  are not. Stick to the form shown in the table.
- **Using Azure OpenAI without the deployment path.** Azure requires
  an extra segment (`/openai/deployments/<name>/chat/completions`)
  that the vanilla OpenAI path doesn't. Currently not supported out of
  the box; use a proxy or a custom provider if you need Azure.
- **Relying on JSON mode everywhere.** Many local servers don't
  enforce schemas. Validate the response yourself when using Ollama /
  LM Studio for critical tool args.
