//! Pre-flight token counting for LLM requests.
//!
//! Two backends today:
//! * [`AnthropicTokenCounter`] — calls Anthropic's `/v1/messages/count_tokens`
//!   endpoint. Exact (matches billing). LRU-cached on `blake3(payload)`
//!   so the stable tools+identity prefix counts as a single network
//!   round-trip per process lifetime. Wrap the counter behind a
//!   `CircuitBreaker` so a failing endpoint cannot block the agent loop.
//! * [`TiktokenCounter`] — offline approximation using tiktoken's
//!   `cl100k_base` encoding. Drift vs Anthropic real billing is in the
//!   5–15% range — fine for budget gating with a conservative
//!   `compact_at_pct`, not for hard limits.
//!
//! `is_exact()` lets callers tag emitted metrics so dashboards can
//! distinguish billing-accurate counts from approximations.

use async_trait::async_trait;

use crate::prompt_block::PromptBlock;
use crate::retry::LlmError;
use crate::types::ChatMessage;

pub mod anthropic_api;
pub mod cascading;
pub mod tiktoken_fallback;

pub use anthropic_api::AnthropicTokenCounter;
pub use cascading::CascadingTokenCounter;
pub use tiktoken_fallback::TiktokenCounter;

/// Counts the input-token cost of a prompt before the request is sent.
/// Used for pre-flight budget enforcement and compaction triggers.
#[async_trait]
pub trait TokenCounter: Send + Sync {
    /// Count tokens for a structured system-prompt block list. Providers
    /// that bill the system prompt separately (Anthropic) tally each
    /// block individually; providers without that distinction flatten
    /// and count once.
    async fn count_blocks(&self, blocks: &[PromptBlock]) -> Result<u32, LlmError>;

    /// Count tokens for a model-specific message list. The `model` arg
    /// matters for backends whose tokenizer varies per model (Anthropic
    /// `count_tokens` requires it; tiktoken ignores it).
    async fn count_messages(&self, model: &str, messages: &[ChatMessage]) -> Result<u32, LlmError>;

    /// True when this counter matches provider billing exactly. Callers
    /// that emit telemetry should label the metric with this flag so a
    /// dashboard can distinguish exact from approximate counts.
    fn is_exact(&self) -> bool;

    /// Stable backend identifier for telemetry (`"anthropic_api"`,
    /// `"tiktoken"`, …).
    fn backend(&self) -> &'static str;
}

/// Build the right counter for a given provider name + API config.
///
/// Selection rules for `backend = "auto"`:
/// * `provider = "anthropic"` AND `api_key` non-empty → `AnthropicTokenCounter`
/// * Anything else → `TiktokenCounter` (offline, approximate)
///
/// Explicit `"anthropic_api"` or `"tiktoken"` short-circuit the auto
/// rules. An invalid backend string falls back to tiktoken with a
/// `tracing::warn!` once.
pub fn build(
    backend: &str,
    provider: &str,
    base_url: &str,
    api_key: &str,
    cache_capacity: u32,
) -> std::sync::Arc<dyn TokenCounter> {
    use std::sync::Arc;
    // Helper: wrap an exact primary in the cascade so a failing API
    // can't take down the agent loop. Tiktoken stays bare — there's
    // nothing to fall back from.
    let cascade = |primary: Arc<dyn TokenCounter>| -> Arc<dyn TokenCounter> {
        Arc::new(CascadingTokenCounter::new(primary))
    };
    match backend {
        "anthropic_api" => cascade(Arc::new(AnthropicTokenCounter::new(
            base_url,
            api_key,
            cache_capacity,
        ))),
        "tiktoken" => Arc::new(TiktokenCounter::new()),
        "auto" => {
            if provider == "anthropic" && !api_key.trim().is_empty() {
                cascade(Arc::new(AnthropicTokenCounter::new(
                    base_url,
                    api_key,
                    cache_capacity,
                )))
            } else {
                Arc::new(TiktokenCounter::new())
            }
        }
        other => {
            tracing::warn!(
                backend = other,
                "unknown token_counter backend — falling back to tiktoken"
            );
            Arc::new(TiktokenCounter::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_picks_anthropic_when_auto_and_keyed() {
        let c = build("auto", "anthropic", "https://api.anthropic.com", "sk-x", 16);
        // Wrapped in cascade for resilience.
        assert_eq!(c.backend(), "cascading");
        // The primary is exact and the cascade hasn't tripped → still exact.
        assert!(c.is_exact());
    }

    #[test]
    fn build_falls_back_to_tiktoken_when_no_key() {
        let c = build("auto", "anthropic", "", "", 16);
        assert_eq!(c.backend(), "tiktoken");
        assert!(!c.is_exact());
    }

    #[test]
    fn build_explicit_tiktoken_overrides_provider() {
        let c = build("tiktoken", "anthropic", "", "sk-x", 16);
        assert_eq!(c.backend(), "tiktoken");
    }

    #[test]
    fn build_unknown_backend_falls_back_to_tiktoken() {
        let c = build("nonsense", "openai", "", "", 16);
        assert_eq!(c.backend(), "tiktoken");
    }

    #[test]
    fn build_explicit_anthropic_works_for_other_providers() {
        let c = build(
            "anthropic_api",
            "openai",
            "https://api.anthropic.com",
            "k",
            16,
        );
        assert_eq!(c.backend(), "cascading");
    }
}
