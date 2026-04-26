# nexo-resilience

> Reusable resilience primitives for Nexo — `CircuitBreaker`, retry policies, and rate limiters wrapping every external call (LLM providers, MCP servers, channel APIs, web search).

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`CircuitBreaker`** — three-state breaker (closed / open /
  half-open) with configurable failure threshold, success
  threshold (for half-open recovery), and exponential backoff
  between probe attempts. Lock-free hot path: `allow()` is a
  pair of atomic loads.
- **`circuit.call(closure)`** — wraps an async closure;
  returns `CircuitError::Open(name)` when the breaker is
  tripped, `CircuitError::Inner(e)` on real failures. Counts
  successes + failures automatically.
- **Per-call site naming** — every breaker is named
  (`"telegram.api.telegram.org"`, `"plugins.google"`,
  `"llm.anthropic"`, `"web_search.brave"`) so log lines + the
  `nexo_*_breaker_open_total` counters stay correlatable.
- **Retry policies** — `with_retry(op, policy)` for
  exponential / fixed / decorrelated-jitter retry with bounded
  total time. Honours `Retry-After` headers when the inner
  error carries one (LLM 429s).
- **Rate limiters** — token-bucket + sliding-window; used by
  the per-binding `sender_rate_limit` so a flooding sender
  can't exhaust an agent's quota.
- **No async runtime dependency** — pure `std::sync` atomics
  + tokio for sleeps. Bench-covered in
  [`crates/resilience/benches/circuit_breaker.rs`](https://github.com/lordmacu/nexo-rs/tree/main/crates/resilience/benches/circuit_breaker.rs)
  (Phase 35.1) — closed-state `allow()` is sub-100ns.

## Public API

```rust
pub struct CircuitBreaker { /* … */ }

impl CircuitBreaker {
    pub fn new(name: impl Into<String>, cfg: CircuitBreakerConfig) -> Self;
    pub fn name(&self) -> &str;
    pub fn allow(&self) -> bool;
    pub fn on_success(&self);
    pub fn on_failure(&self);
    pub fn trip(&self);
    pub fn reset(&self);
    pub async fn call<F, Fut, T, E>(&self, f: F) -> Result<T, CircuitError<E>>
    where F: FnOnce() -> Fut, Fut: Future<Output = Result<T, E>>;
}

pub struct CircuitBreakerConfig {
    pub failure_threshold: u32,
    pub success_threshold: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}
```

## Where it's used

| Caller | Purpose |
|---|---|
| `nexo-llm` (Anthropic / OpenAI-compat / Gemini / DeepSeek / MiniMax) | Wraps every chat completion + token-counter call |
| `nexo-mcp` HTTP client | Wraps remote MCP server calls |
| `nexo-web-search` per-provider | One breaker per provider; router skips open breakers |
| `nexo-plugin-telegram` | Wraps every Bot API call |
| `nexo-plugin-google` | Wraps OAuth endpoints + general Google API |

## Install

```toml
[dependencies]
nexo-resilience = "0.1"
```

## Documentation for this crate

- [Fault tolerance](https://lordmacu.github.io/nexo-rs/architecture/fault-tolerance.html)
- [Benchmarks](https://lordmacu.github.io/nexo-rs/ops/benchmarks.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
