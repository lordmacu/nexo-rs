# nexo-llm

> Multi-provider LLM client trait + concrete implementations for Anthropic / OpenAI-compat / MiniMax / Gemini / DeepSeek, with shared streaming pipeline, rate limiter, retry policies, and CircuitBreaker hardening.

This crate is part of **[Nexo](https://github.com/lordmacu/nexo-rs)** — a multi-agent Rust framework with a NATS event bus, pluggable LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek), per-agent credentials, MCP support, and channel plugins for WhatsApp, Telegram, Email, and Browser (CDP).

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>

## What this crate does

- **`LlmClient` trait** — uniform `chat`, `chat_stream`,
  `embed` surface across providers. New providers impl the
  trait + register via `LlmProviderFactory`.
- **Concrete implementations**:
  - `AnthropicClient` — native Anthropic API with prompt
    caching (`cache_control`), the OAuth subscription flow
    (Phase 15: anthropic-cli credential reader + browser
    PKCE), MarkdownV2-safe rendering.
  - `OpenAiCompatClient` — OpenAI-compat for any endpoint that
    speaks `/v1/chat/completions`. Covers MiniMax / DeepSeek /
    Mistral.rs / Ollama / vLLM / LM Studio / TGI in one impl.
  - `GeminiClient` — Google Generative Language API.
  - `MinimaxClient` — native MiniMax API path with their
    OAuth flow.
- **Shared streaming pipeline** — `parse_openai_sse`,
  `parse_anthropic_sse`, `parse_gemini_sse` produce a uniform
  `Stream<Item = StreamChunk>`. `record_usage_tap` +
  `stream_metrics_tap` instrument every stream.
- **Rate limiter** — per-provider token bucket so a
  bursty agent loop doesn't trigger upstream 429s.
- **Retry policies** — `with_retry(op, policy)` with explicit
  rules per HTTP class:
  - 429 → 5 attempts, 1s → 60s exponential, honours
    `Retry-After`.
  - 5xx → 3 attempts, 2s → 30s exponential.
  - 4xx (other) → no retry.
- **CircuitBreaker** — per-`LlmClient` instance from
  [`nexo-resilience`](https://github.com/lordmacu/nexo-rs/tree/main/crates/resilience)
  wraps every chat completion + token-counter call.
- **Tool calling** — uniform `ToolDef` + tool-call
  serialisation across providers (handles the 4 different
  on-wire shapes).
- **Token counter** — `TokenCounter` for budget estimation
  before sending; cascading provider used by Phase 18
  context optimisation.
- **Streaming telemetry**:
  `nexo_llm_stream_ttft_seconds_*{provider}` histogram +
  `nexo_llm_stream_chunks_total{provider,kind}` counter.

## Public API

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError>;
    async fn chat_stream(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<StreamChunk, LlmError>>, LlmError>;
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, LlmError>;
}

pub enum AnyLlmClient {
    Anthropic(AnthropicClient),
    OpenAiCompat(OpenAiCompatClient),
    Gemini(GeminiClient),
    Minimax(MinimaxClient),
}
```

## Install

```toml
[dependencies]
nexo-llm = "0.1"
```

## Documentation for this crate

- [MiniMax M2.5](https://lordmacu.github.io/nexo-rs/llm/minimax.html)
- [Anthropic / Claude](https://lordmacu.github.io/nexo-rs/llm/anthropic.html)
- [OpenAI-compatible](https://lordmacu.github.io/nexo-rs/llm/openai.html)
- [DeepSeek](https://lordmacu.github.io/nexo-rs/llm/deepseek.html)
- [Rate limiting & retry](https://lordmacu.github.io/nexo-rs/llm/retry.html)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
