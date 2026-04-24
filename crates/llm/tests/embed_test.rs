//! Tests for the `LlmClient::embed` path.
//!
//! Full HTTP-level coverage arrives with Phase 10 (embeddings + sqlite-vec); for
//! now we pin the default-error behaviour and the zero-input shortcut so
//! providers that opt out return predictable errors to callers.

use agent_config::types::llm::{LlmProviderConfig, RateLimitConfig, RetryConfig};
use agent_llm::{
    ChatRequest, ChatResponse, FinishReason, LlmClient, OpenAiClient, ResponseContent, TokenUsage,
};
use async_trait::async_trait;

struct NoEmbedProvider;

#[async_trait]
impl LlmClient for NoEmbedProvider {
    async fn chat(&self, _: ChatRequest) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            content: ResponseContent::Text("ok".into()),
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Stop,
        })
    }
    fn model_id(&self) -> &str {
        "no-embed-model"
    }
    fn provider(&self) -> &str {
        "no-embed-provider"
    }
}

#[tokio::test]
async fn default_embed_returns_error_with_provider_name() {
    let client = NoEmbedProvider;
    let err = client
        .embed(&["hi".to_string()])
        .await
        .expect_err("default embed must error");
    let msg = err.to_string();
    assert!(
        msg.contains("no-embed-provider"),
        "error should name the provider, got: {msg}"
    );
    assert!(msg.contains("not supported"));
}

#[tokio::test]
async fn openai_embed_empty_input_short_circuits() {
    let cfg = LlmProviderConfig {
        api_key: "unused".into(),
        group_id: None,
        base_url: "http://127.0.0.1:1".into(), // unroutable — must not be hit
        rate_limit: RateLimitConfig::default(),
        auth: None,
        api_flavor: None,
        embedding_model: None,
        safety_settings: None,
    };
    let client = OpenAiClient::new(&cfg, "text-embedding-3-small", RetryConfig::default());
    let out = client.embed(&[]).await.unwrap();
    assert!(out.is_empty());
}
