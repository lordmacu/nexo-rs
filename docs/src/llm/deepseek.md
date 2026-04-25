# DeepSeek

Connector for DeepSeek's hosted models. The API is OpenAI-compatible
end to end (same `/v1/chat/completions` shape, same SSE streaming,
same Bearer auth) so the connector is a thin factory that wraps
`OpenAiClient` with DeepSeek's default endpoint.

Source: `crates/llm/src/deepseek.rs`.

## Configuration

```yaml
# config/llm.yaml
providers:
  deepseek:
    api_key: ${DEEPSEEK_API_KEY}
    # base_url defaults to https://api.deepseek.com/v1 when blank.
    # Override only for self-hosted gateways or testing fixtures.
    base_url: ""
    rate_limit:
      requests_per_second: 2.0
      quota_alert_threshold: 100000
```

Pin the agent to it:

```yaml
agents:
  - id: ana
    model:
      provider: deepseek
      model: deepseek-chat
```

## Models

| Model id | Use case |
|----------|----------|
| `deepseek-chat` | General-purpose. Supports tool calling. |
| `deepseek-reasoner` | Long-form reasoning. **No tool calling** in current API revision. |

`deepseek-reasoner` agents must therefore leave `allowed_tools` empty
(or list only tools the agent never plans to invoke). Tool calls fired
against the reasoner endpoint return an error from upstream.

## Streaming

Identical to OpenAI's SSE format, so `OpenAiClient::chat_stream` parses
it without per-provider code. `agent_llm_stream_ttft_seconds` and
`agent_llm_stream_chunks_total` Prometheus series labelled with
`provider="deepseek"` show up automatically.

## Tool calling

`deepseek-chat` follows OpenAI's tool-calling spec verbatim. JSON
arguments deserialise the same way; `parallel_tool_calls` is honoured.

## Rate limits

DeepSeek returns standard `429` with a `retry-after` header. The
existing retry plumbing (`crates/llm/src/retry.rs`) consumes that
header so 429s back off cleanly without touching the connector.

## Quota / cost

DeepSeek's pricing is per-1M-tokens; the `TokenUsage` returned by
each `ChatResponse` is forwarded to the standard
`agent_llm_tokens_total` counter (labels: `provider="deepseek"`,
`model`, `usage_kind`).

## Known limitations

- **No native embeddings client** — DeepSeek does not currently
  publish an embeddings endpoint. Use a different provider for
  `embedding_model` if your agent needs vector search.
- **Reasoner tool-call gap** — see [Models](#models). Validate at boot
  by leaving `allowed_tools: []` on agents pinned to
  `deepseek-reasoner`.
- **Cache awareness** — DeepSeek's KV-cache hit information is
  surfaced through the same `cache_usage` field as the OpenAI client
  reports it.

## See also

- [OpenAI-compatible](./openai.md) — same wire format, full notes on
  the underlying client.
- [Rate limiting & retry](./retry.md) — backoff policy.
