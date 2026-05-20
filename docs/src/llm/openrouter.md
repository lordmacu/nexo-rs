# OpenRouter

Connector for [OpenRouter](https://openrouter.ai), a unified gateway
that routes one OpenAI-shaped request to many vendor models behind a
single API key. The connector is a thin wrapper around `OpenAiClient`
that stamps attribution headers on canonical-host requests and
forward-fixes the legacy `/v1` base URL.

Source: `crates/llm/src/openrouter.rs`.

## When to use it

- You want one API key + one billing seat for Anthropic, OpenAI,
  Google, DeepSeek, xAI, Meta-Llama, Mistral, Qwen, etc.
- You want server-side fallback routing without writing a circuit
  breaker per provider.
- You want to A/B-test a new model without signing up with its
  vendor first.

If you are committed to one vendor, prefer that vendor's dedicated
connector (`anthropic`, `openai`, `gemini`, `deepseek`) — direct
routes skip OpenRouter's extra network hop and preserve provider-
specific cache markers.

## Configuration

```yaml
# config/llm.yaml
providers:
  openrouter:
    api_key: ${OPENROUTER_API_KEY}
    # base_url defaults to https://openrouter.ai/api/v1 when blank.
    # Override only for proxies, self-hosted gateways or tests.
    base_url: ""
    rate_limit:
      requests_per_second: 2.0
```

Pin an agent to it:

```yaml
# config/agents.yaml
agents:
  - id: ana
    model:
      provider: openrouter
      model: anthropic/claude-opus-4-7
```

`OPENROUTER_API_KEY` is the conventional environment variable;
operators who already use the multi-instance shape can point at a
secret instead with `api_key_secret_id`.

## Models

Model slugs follow the opaque `vendor/model` shape — OpenRouter
routes server-side, so the connector never parses the slug as a
path. Operator UIs surface a curated list via the registry catalog:

| Slug | Notes |
|------|-------|
| `openrouter/auto` | OpenRouter picks the cheapest healthy route |
| `anthropic/claude-opus-4-7` | Anthropic's flagship |
| `anthropic/claude-sonnet-4-6` | Faster Anthropic tier |
| `openai/gpt-5` | OpenAI flagship |
| `google/gemini-2.5-pro` | Google flagship |
| `deepseek/deepseek-v4-pro` | DeepSeek's strongest |
| `x-ai/grok-4` | xAI |
| `meta-llama/llama-4-maverick` | Open-weights Llama |

You can send any other slug OpenRouter accepts; the curated list is
a UX hint, not a server-side allowlist.

### Slug validation

The factory rejects two slug shapes loudly at build time:

- Empty / whitespace-only — boot fails with
  `openrouter: model slug is empty`.
- Duplicated prefix `openrouter/openrouter/…` — boot fails with
  `openrouter: model slug has duplicated 'openrouter/' prefix: …`.
  This guards against the historical OpenRouter config bug where
  YAML writers stamped the prefix twice.

### Base URL canonicalization

Operators who copied the wrong URL into YAML get a silent forward-fix
at factory build time:

| Input | Resolved |
|-------|----------|
| _(blank)_ | `https://openrouter.ai/api/v1` |
| `https://openrouter.ai/v1` | `https://openrouter.ai/api/v1` |
| `https://openrouter.ai/api/v1/` | `https://openrouter.ai/api/v1` |
| `https://proxy.example.com/openrouter` | _(unchanged)_ |

## Cache markers passthrough

When the slug starts with `anthropic/` **and** the resolved base
URL matches the canonical OpenRouter host, the client rewrites the
system message into Anthropic's multi-block `cache_control` shape:

```jsonc
// Wire body sent to OpenRouter:
{
  "model": "anthropic/claude-opus-4-7",
  "messages": [
    {
      "role": "system",
      "content": [
        { "type": "text", "text": "TOOLS_BUNDLE" },
        { "type": "text", "text": "IDENTITY",
          "cache_control": { "type": "ephemeral", "ttl": "1h" } },
        { "type": "text", "text": "ROLLING_TAIL",
          "cache_control": { "type": "ephemeral" } }
      ]
    },
    { "role": "user", "content": "…" }
  ],
  "tools": [
    { "type": "function", "function": { "name": "a" } },
    { "type": "function", "function": { "name": "b" },
      "cache_control": { "type": "ephemeral", "ttl": "1h" } }
  ]
}
```

The breakpoint placement mirrors `crates/llm/src/anthropic.rs` exactly
so prompt-cache hit rate is parity with direct Anthropic routes:

- One breakpoint per contiguous same-policy run of `PromptBlock`s,
  on the LAST block of the run.
- `req.cache_tools = true` stamps a long-TTL breakpoint on the LAST
  entry of `tools`.
- Anthropic caps each request at 4 breakpoints — the 5th and
  onwards are dropped silently (prefix-cache still hits, just the
  tail does not).

Operators who point `base_url` at a proxy or self-host get no cache
markers (the gateway might strip them or, worse, forward them to a
non-Anthropic vendor that rejects the extra field). To enable cache
passthrough on a custom route you must claim the canonical host in
`base_url`.

Callers who do not populate `system_blocks` get the legacy flat
`system_prompt` shape with no markers — the cache hit rate stays
near zero. Migrate to `PromptBlock::cached_long` /
`PromptBlock::cached_short` to unlock the gain.

## Provider routing

`ChatRequest.provider_routing` (optional `ProviderRoutingPolicy`) is
serialised into OpenRouter's `provider: {…}` body extension when the
client targets the canonical OpenRouter host. Operators / agents
populate it to request a vendor preference order, a fallback policy,
required parameter support, or a sort key:

```rust
use nexo_llm::{ChatRequest, types::ProviderRoutingPolicy};

let mut req = ChatRequest::new("anthropic/claude-opus-4-7", vec![]);
req.provider_routing = Some(ProviderRoutingPolicy {
    order: vec!["anthropic".into(), "google".into()],
    allow_fallbacks: Some(true),
    require_parameters: vec!["tools".into()],
    sort: Some("throughput".into()),
});
```

The wrapper serialises the policy into:

```jsonc
{
  "provider": {
    "order": ["anthropic", "google"],
    "allow_fallbacks": true,
    "require_parameters": ["tools"],
    "sort": "throughput"
  }
}
```

Default (`None` or an empty policy) ships an unannotated request —
OpenRouter applies its own default route. Non-OpenRouter providers
ignore the field, so populating it from the agent layer is safe.

## Attribution headers

When the resolved base URL points at the canonical OpenRouter host
(`https://openrouter.ai/api/v1`), every outbound request carries:

| Header | Value |
|--------|-------|
| `HTTP-Referer` | `https://github.com/lordmacu/nexo-rs` |
| `X-Title` | `nexo-rs` |

These headers identify nexo-rs on the OpenRouter leaderboard and may
unlock per-app rate-limit bumps. If you repoint `base_url` at a
proxy or self-host, the attribution headers are dropped — we never
leak the `HTTP-Referer` outside OpenRouter.

## Cost tracking

When OpenRouter prices a request, it returns a `usage.cost` field
in the response body as a USD float (uplift-inclusive). The
connector surfaces this verbatim on
[`ChatResponse::cost_usd`](https://docs.rs/nexo-llm) (`Option<f64>`):

```rust
let resp = client.chat(req).await?;
if let Some(usd) = resp.cost_usd {
    // accumulate into per-tenant billing pipeline
    billing.record(tenant_id, usd);
}
```

Raw OpenAI / DeepSeek / Anthropic / Gemini responses omit the field
and leave `cost_usd` as `None`. Defensive: negative or NaN values
from a buggy gateway are filtered to `None` so they cannot poison
downstream aggregations. Multi-tenant SaaS deployments
([[project_microapp_is_saas_meta_creator]]) consume this field to
attribute per-tenant cost without scraping vendor invoices.

Every non-streaming `OpenRouterClient::chat` that returns a cost
also accumulates it into the Prometheus counter
`nexo_llm_cost_usd_total{provider="openrouter"}` (USD, 6-decimal
precision). The label is `openrouter`, not the underlying inner
`openai`-compat transport, so dashboards attribute spend to the
gateway correctly. Streaming cost accounting is not yet wired (the
SSE `usage` chunk carries cost on some routes — tracked as a
deferred follow-up).

## Live model catalogue

The curated `KNOWN_MODELS` list is a fast UX hint, but operators
who want the full live catalogue can call:

```rust
use nexo_llm::{
    openrouter_live_or_known_models, openrouter_refresh_models,
    OPENROUTER_MODELS_CACHE_TTL,
};

// 15-minute in-memory cache; falls back to KNOWN_MODELS on network
// failure so the UI never shows an empty list.
let slugs = openrouter_live_or_known_models(api_key).await;

// Operator clicked "Refresh catalogue":
let fresh = openrouter_refresh_models(api_key).await?;
```

The cache is process-wide and shared across every `OpenRouterClient`
in the daemon. `parse_models_response` is exposed publicly for
testing custom OR-compat proxies that emit a different model
catalogue shape — pure function, no network.

## Streaming

OpenRouter speaks OpenAI's SSE format verbatim, so
`OpenAiClient::stream` parses it without per-provider code. The
`nexo_llm_stream_ttft_seconds` and `nexo_llm_stream_chunks_total`
Prometheus series carry `provider="openrouter"` automatically.

## Tool calling

OpenRouter forwards the OpenAI tool-calling spec to the underlying
vendor. For Anthropic models the gateway translates between OpenAI
and Anthropic tool shapes server-side; for native OpenAI models the
shape passes through unchanged.

## Rate limits & retries

`429 Too Many Requests` carries the standard `retry-after` header.
`extract_openai_compat_headers` parses the `x-ratelimit-*` series so
quota tracking works without per-provider code. The retry policy in
`llm.yaml.retry` applies as written.

## Embeddings

OpenRouter does not expose a uniform embeddings endpoint across all
routes, so the connector does **not** implement `LlmClient::embed`.
Use a dedicated provider (Gemini's `text-embedding-004`, OpenAI's
`text-embedding-3-small`, …) for vector search.

## Trade-offs

- **Latency**: one extra hop adds ~30–150 ms p50 on top of the
  underlying vendor.
- **Prompt caching for Anthropic models via OpenRouter** is passed
  through automatically when the slug starts with `anthropic/` and
  the resolved base URL is the canonical OpenRouter host. The
  client rewrites the system message into the multi-block
  `cache_control` shape that Anthropic accepts. Proxies and
  non-Anthropic vendors opt out by design (the marker is silently
  dropped). See [Cache markers passthrough](#cache-markers-passthrough).
- **Vendor lock-in inverse**: if OpenRouter has an outage, every
  model behind it goes down with it. Pair `openrouter` with a
  direct vendor provider in `llm.yaml` if you need a fallback.

## Troubleshooting

| Symptom | Likely cause |
|---------|--------------|
| `401 Unauthorized` | `OPENROUTER_API_KEY` env var unset or stale |
| `slug has duplicated 'openrouter/' prefix` | Drop the second `openrouter/` from `model.model` in `agents.yaml` |
| Anthropic models slow + expensive | Prompt cache miss (Anthropic-via-OR limitation). Track follow-up `100.x.cache-control`. |
| Quota alerts noisy | Lower `rate_limit.quota_alert_threshold` in `llm.yaml` |

## Implementation

- `crates/llm/src/openrouter.rs` — factory + thin `OpenRouterClient`
  wrapper.
- `crates/llm/src/openai_compat.rs` — owns the HTTP transport, SSE
  parser, tool-call encoder. The `extra_headers` field is what
  carries the attribution map.
- `crates/llm/tests/openrouter_factory_e2e.rs` — registry-level
  build path + slug rejection + catalog wiring.
