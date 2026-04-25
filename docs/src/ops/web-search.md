# Web search

The `web_search` built-in tool lets an agent query the web through one
of four providers: **Brave**, **Tavily**, **DuckDuckGo**, **Perplexity**.
The runtime owns provider selection, caching, sanitisation, and circuit
breaking — agents only see results.

The feature is **off by default**. Operators opt in per agent (and
optionally override per binding).

## Per-agent config

```yaml
# config/agents.yaml
agents:
  - id: ana
    web_search:
      enabled: true               # default false
      provider: auto              # "auto" | "brave" | "tavily" | "duckduckgo" | "perplexity"
      default_count: 5            # 1..=10
      cache_ttl_secs: 600         # 0 disables cache
      expand_default: false       # default value of `expand` arg
```

### `provider: auto`

Picks the first credentialed provider in this order:

1. `brave` (env `BRAVE_SEARCH_API_KEY`)
2. `tavily` (env `TAVILY_API_KEY`)
3. `perplexity` (env `PERPLEXITY_API_KEY`, requires the `perplexity` feature)
4. `duckduckgo` (no key — bundled by default; the always-available fallback)

DuckDuckGo scrapes `html.duckduckgo.com` and is rate-limited / captcha-prone;
the runtime detects bot challenges and trips the breaker so the next call
rotates to a different provider.

## Per-binding override

Same shape as `link_understanding`: `null` (default) inherits the agent
value, any object replaces it.

```yaml
agents:
  - id: ana
    web_search: { enabled: true }
    bindings:
      - inbound: plugin.inbound.whatsapp.*
        web_search: { enabled: false }   # silent on WA
      - inbound: plugin.inbound.telegram.*
        # inherits agent default
```

## Tool surface

The LLM sees this signature:

```json
{
  "name": "web_search",
  "parameters": {
    "query":     "string  (required)",
    "count":     "integer (1-10, optional)",
    "provider":  "string  (optional override)",
    "freshness": "day | week | month | year (optional)",
    "country":   "ISO-3166 alpha-2 (optional)",
    "language":  "ISO-639-1 (optional)",
    "expand":    "boolean (optional)"
  }
}
```

Return shape:

```json
{
  "provider": "brave",
  "query":    "rust async runtimes",
  "from_cache": false,
  "results": [
    {
      "url": "https://example.com/post",
      "title": "Title",
      "snippet": "First 4 KiB of the description, sanitised.",
      "site_name": "example.com",
      "published_at": "2026-04-20T00:00:00Z"
    }
  ]
}
```

When `expand: true` and Phase 21 link understanding is enabled, the
top three hits also get a `body` field populated by the shared
`LinkExtractor`. Bodies obey the same denylist + size caps that
[Link understanding](./link-understanding.md) describes.

## Cache

In-process SQLite cache shared across every agent. Key format:

```
sha256(SCHEMA_VERSION || provider || query || canonical_params)
```

`canonical_params` excludes `provider` (router decides) and `expand`
(post-processing). `cache_ttl_secs: 0` disables caching entirely.

Operators that want a separate cache file or schema migration set
`web_search.cache.path` in `web_search.yaml` (planned — see
[FOLLOWUPS](https://github.com/...)).

## Circuit breaker

Every provider call goes through `nexo_resilience::CircuitBreaker`
keyed `web_search:<provider>`. Default config: 5 consecutive failures
trip the breaker, exponential backoff up to 120 s. Open-state calls
return `ProviderUnavailable(provider)` immediately and the router
rotates to the next candidate (when called via auto-detect).

## Sanitisation

Every `title`, `url`, and `snippet` returned by a provider passes
through `sanitise_for_prompt`:

- control chars stripped,
- CR / LF / tab collapsed to single spaces,
- runs of whitespace collapsed,
- byte-capped at 4 KiB (snippet) / 512 B (title) / 2 KiB (URL),
- truncation respects UTF-8 char boundaries.

This is the same defence-in-depth Phase 19 (`language` directive) and
Phase 21 (`# LINK CONTEXT`) apply: SERPs are attacker-controlled input.

## Telemetry

Exported on `/metrics`:

- `nexo_web_search_calls_total{provider,result}` — counter, one
  increment per provider attempt. `result` is `ok` (provider returned
  hits), `error` (network / HTTP / parse failure), or `unavailable`
  (the breaker short-circuited the call before it left the process).
- `nexo_web_search_cache_total{provider,hit}` — counter, every
  TTL-cached lookup. `provider` is the *first* candidate (the one the
  cache key is built from). Compute hit rate as
  `cache_total{hit="true"} / sum(cache_total)`.
- `nexo_web_search_breaker_open_total{provider}` — counter; one
  increment per request the breaker rejected. Pair with
  `circuit_breaker_state{breaker="web_search:<provider>"}` to alert on
  *sustained* open state vs a flap.
- `nexo_web_search_latency_ms{provider}` — histogram. Only observed
  for attempts that issued an HTTP request, so the percentile reflects
  real provider latency (cache hits and breaker short-circuits would
  pull p50 down to 0 and hide regressions).

## When to leave it off

- Privacy-sensitive deployments where outbound HTTP from the agent
  host is not allowed.
- Channels where the cost of a noisy SERP in the prompt outweighs the
  agent's value (use per-binding `enabled: false`).
- Agents that already have `link_understanding` for the URLs the user
  shares — no need for SERP duplication.
