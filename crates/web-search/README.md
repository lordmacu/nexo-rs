# nexo-web-search

> Multi-provider web search router for Nexo — Brave / Tavily / DuckDuckGo / Perplexity behind one trait, with caching, sanitisation, and per-provider circuit breakers.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`SearchProvider` trait** — uniform `search(args) -> Result<SearchResult>`
  shape implemented by `BraveProvider`, `TavilyProvider`,
  `DuckDuckGoProvider`, `PerplexityProvider`. New providers just
  impl the trait.
- **`WebSearchRouter`** — fanout / fallback orchestrator.
  `provider: auto` picks the first credentialed in priority
  order; explicit `provider:` overrides per call.
- **LRU result cache** — TTL-based (`cache_ttl_secs`),
  query-keyed; cache hits don't count against per-provider
  rate limits.
- **Per-provider circuit breaker** — uses
  [`nexo-resilience`](https://github.com/lordmacu/nexo-rs/tree/main/crates/resilience).
  A flapping provider opens its breaker; the router auto-skips
  to the next.
- **Result sanitisation** — strips tracking params (`utm_*`,
  `fbclid`, …), HTML entities in titles, and oversized snippets.
- **Telemetry** —
  `nexo_web_search_calls_total{provider,result}`,
  `nexo_web_search_cache_total{provider,hit}`,
  `nexo_web_search_breaker_open_total{provider}`,
  `nexo_web_search_latency_ms{provider}` histogram.
- **Used by** the `web_search` agent tool ([source](https://github.com/lordmacu/nexo-rs/tree/main/crates/core/src/agent/web_search_tool.rs)) — agents call it through `nexo-core`'s tool registry.

## Public API

```rust
pub trait SearchProvider {
    async fn search(&self, args: WebSearchArgs) -> Result<SearchResult, SearchError>;
    fn name(&self) -> &'static str;
}

pub struct WebSearchRouter { /* … */ }

impl WebSearchRouter {
    pub fn new(providers: Vec<Arc<dyn SearchProvider>>) -> Self;
    pub async fn search(
        &self,
        args: WebSearchArgs,
        provider_hint: Option<&str>,
    ) -> Result<SearchResult, SearchError>;
}
```

## Configuration

```yaml
# config/agents.yaml — per-agent gating
agents:
  - id: ana
    web_search:
      enabled: true
      provider: auto              # auto | brave | tavily | duckduckgo | perplexity
      default_count: 5            # 1..=10
      cache_ttl_secs: 600         # 0 disables cache
      expand_default: false
```

API keys via env: `BRAVE_SEARCH_API_KEY`, `TAVILY_API_KEY`,
`PERPLEXITY_API_KEY`. DuckDuckGo doesn't need a key.

## Install

```toml
[dependencies]
nexo-web-search = "0.1"
```

## Documentation for this crate

- [Web search guide](https://lordmacu.github.io/nexo-rs/ops/web-search.html)
- [Web fetch (companion tool)](https://lordmacu.github.io/nexo-rs/ops/web-fetch.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
