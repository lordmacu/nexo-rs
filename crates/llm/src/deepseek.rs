//! DeepSeek connector.
//!
//! DeepSeek's HTTP API is OpenAI-compatible: same `/v1/chat/completions`
//! shape, same SSE streaming format, same Bearer auth. We don't need a
//! second HTTP client — the connector is a thin [`LlmProviderFactory`]
//! that defaults `base_url` to `https://api.deepseek.com/v1` when the
//! operator leaves it blank, then hands construction off to
//! [`OpenAiClient`].
//!
//! ## YAML
//!
//! ```yaml
//! providers:
//!   deepseek:
//!     api_key: ${DEEPSEEK_API_KEY}
//!     # base_url defaults to https://api.deepseek.com/v1 when omitted.
//!     # Override only for self-hosted gateways or testing fixtures.
//! ```
//!
//! Per-agent:
//!
//! ```yaml
//! agents:
//!   - id: ana
//!     model:
//!       provider: deepseek
//!       model: deepseek-chat       # or deepseek-reasoner
//! ```
//!
//! ## Notes
//!
//! - **Models:** `deepseek-chat` (general) and `deepseek-reasoner`
//!   (reasoning-tuned). Pass the exact id under `model.model`.
//! - **Streaming:** identical to OpenAI's SSE; `OpenAiClient::chat_stream`
//!   handles it transparently.
//! - **Tool calling:** DeepSeek follows the OpenAI tool-calling spec for
//!   `deepseek-chat`. `deepseek-reasoner` does not currently expose tool
//!   use — log a warning at boot if an agent paired with reasoner has
//!   `allowed_tools` populated.
//! - **Rate limits:** standard 429 handling via `RetryConfig`; DeepSeek
//!   honours the `retry-after` header so the existing retry plumbing
//!   works without changes.

use std::sync::Arc;

use agent_config::types::llm::{LlmProviderConfig, RateLimitConfig, RetryConfig};

use crate::client::LlmClient;
use crate::openai_compat::OpenAiClient;
use crate::registry::LlmProviderFactory;

/// DeepSeek's public API base URL. Used when the operator leaves
/// `providers.deepseek.base_url` empty in `llm.yaml`.
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com/v1";

/// Factory for `provider: deepseek` in `model.provider`.
pub struct DeepSeekFactory;

impl LlmProviderFactory for DeepSeekFactory {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn build(
        &self,
        provider_cfg: &LlmProviderConfig,
        model: &str,
        retry: RetryConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        // OpenAiClient already defaults base_url to OpenAI's endpoint
        // when blank. We pre-fill DeepSeek's so an operator who only
        // sets `api_key` gets the right destination without learning
        // the URL by heart.
        let cfg = if provider_cfg.base_url.trim().is_empty() {
            LlmProviderConfig {
                api_key: provider_cfg.api_key.clone(),
                group_id: provider_cfg.group_id.clone(),
                base_url: DEFAULT_BASE_URL.to_string(),
                rate_limit: RateLimitConfig {
                    requests_per_second: provider_cfg.rate_limit.requests_per_second,
                    quota_alert_threshold: provider_cfg.rate_limit.quota_alert_threshold,
                },
                auth: provider_cfg.auth.clone(),
                api_flavor: provider_cfg.api_flavor.clone(),
                embedding_model: provider_cfg.embedding_model.clone(),
                safety_settings: provider_cfg.safety_settings.clone(),
            }
        } else {
            provider_cfg.clone()
        };
        Ok(Arc::new(OpenAiClient::new(&cfg, model, retry)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_config::types::llm::{LlmProviderConfig, RateLimitConfig};

    fn empty_cfg() -> LlmProviderConfig {
        LlmProviderConfig {
            api_key: "sk-deepseek-test".into(),
            group_id: None,
            base_url: String::new(),
            rate_limit: RateLimitConfig {
                requests_per_second: 1.0,
                quota_alert_threshold: None,
            },
            auth: None,
            api_flavor: None,
            embedding_model: None,
            safety_settings: None,
        }
    }

    #[test]
    fn factory_name_is_deepseek() {
        assert_eq!(DeepSeekFactory.name(), "deepseek");
    }

    #[test]
    fn build_succeeds_with_blank_base_url() {
        // The factory must default the URL so the operator can
        // configure DeepSeek with just an API key.
        let cfg = empty_cfg();
        DeepSeekFactory
            .build(&cfg, "deepseek-chat", RetryConfig::default())
            .expect("DeepSeek client should build with blank base_url");
    }

    #[test]
    fn build_preserves_explicit_base_url() {
        let mut cfg = empty_cfg();
        cfg.base_url = "https://gateway.example.com/v1".into();
        // We can't introspect OpenAiClient.base_url from here (it's
        // private), but the build path must succeed and not panic on
        // the alternate URL.
        DeepSeekFactory
            .build(&cfg, "deepseek-chat", RetryConfig::default())
            .expect("DeepSeek client should respect operator-set base_url");
    }
}
