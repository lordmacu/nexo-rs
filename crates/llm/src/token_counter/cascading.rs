//! Token counter that falls back to tiktoken when the primary fails.
//!
//! `AnthropicTokenCounter` is exact but pays a network round-trip; on
//! 5xx / rate-limit / outage the agent loop must keep moving. We wrap
//! it behind a `CircuitBreaker` so consecutive failures open the
//! circuit and route subsequent counts through `TiktokenCounter`
//! (offline, approximate). Once the breaker half-opens and a probe
//! succeeds, the primary takes over again automatically.
//!
//! `is_exact()` reflects the *current* serving backend so emitted
//! metrics correctly label the count as approximate during fallback
//! windows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use agent_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitError};

use super::tiktoken_fallback::TiktokenCounter;
use super::TokenCounter;
use crate::prompt_block::PromptBlock;
use crate::retry::LlmError;
use crate::types::ChatMessage;

pub struct CascadingTokenCounter {
    primary: Arc<dyn TokenCounter>,
    fallback: Arc<TiktokenCounter>,
    breaker: Arc<CircuitBreaker>,
    /// Latches once the breaker has opened at least once during this
    /// process lifetime — used by `is_exact()` to flip exact→approximate
    /// labels in telemetry until the next process restart.
    /// Conservative: the breaker may close again, but the moment any
    /// approximation entered the metric stream, calling the whole
    /// counter "exact" would mislabel historical samples.
    ever_fell_back: AtomicBool,
}

impl CascadingTokenCounter {
    pub fn new(primary: Arc<dyn TokenCounter>) -> Self {
        let breaker = Arc::new(CircuitBreaker::new(
            "llm.token_counter",
            CircuitBreakerConfig {
                failure_threshold: 3,
                success_threshold: 2,
                initial_backoff: std::time::Duration::from_secs(30),
                max_backoff: std::time::Duration::from_secs(300),
            },
        ));
        Self {
            primary,
            fallback: Arc::new(TiktokenCounter::new()),
            breaker,
            ever_fell_back: AtomicBool::new(false),
        }
    }

    fn note_fallback(&self, reason: &str) {
        if !self.ever_fell_back.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                primary = self.primary.backend(),
                fallback = self.fallback.backend(),
                reason,
                "token counter fell back to tiktoken (this metric will be labeled approximate from now on)"
            );
        }
    }
}

#[async_trait]
impl TokenCounter for CascadingTokenCounter {
    async fn count_blocks(&self, blocks: &[PromptBlock]) -> Result<u32, LlmError> {
        let primary = Arc::clone(&self.primary);
        let blocks_owned: Vec<PromptBlock> = blocks.to_vec();
        let result: Result<u32, CircuitError<LlmError>> = self
            .breaker
            .call(|| {
                let p = Arc::clone(&primary);
                let bs = blocks_owned.clone();
                async move { p.count_blocks(&bs).await }
            })
            .await;
        match result {
            Ok(n) => Ok(n),
            Err(CircuitError::Open(_)) => {
                self.note_fallback("circuit_open");
                self.fallback.count_blocks(blocks).await
            }
            Err(CircuitError::Inner(e)) => {
                self.note_fallback("primary_error");
                tracing::debug!(error = %e, "primary token counter failed; using fallback");
                self.fallback.count_blocks(blocks).await
            }
        }
    }

    async fn count_messages(
        &self,
        model: &str,
        messages: &[ChatMessage],
    ) -> Result<u32, LlmError> {
        let primary = Arc::clone(&self.primary);
        let model_owned = model.to_string();
        let msgs: Vec<ChatMessage> = messages.to_vec();
        let result: Result<u32, CircuitError<LlmError>> = self
            .breaker
            .call(|| {
                let p = Arc::clone(&primary);
                let m = model_owned.clone();
                let ms = msgs.clone();
                async move { p.count_messages(&m, &ms).await }
            })
            .await;
        match result {
            Ok(n) => Ok(n),
            Err(CircuitError::Open(_)) => {
                self.note_fallback("circuit_open");
                self.fallback.count_messages(model, messages).await
            }
            Err(CircuitError::Inner(e)) => {
                self.note_fallback("primary_error");
                tracing::debug!(error = %e, "primary token counter failed; using fallback");
                self.fallback.count_messages(model, messages).await
            }
        }
    }

    fn is_exact(&self) -> bool {
        // Once we've ever fallen back, treat the whole counter as
        // approximate so dashboards don't conflate exact and approximate
        // samples in the same series.
        !self.ever_fell_back.load(Ordering::Relaxed) && self.primary.is_exact()
    }

    fn backend(&self) -> &'static str {
        if self.ever_fell_back.load(Ordering::Relaxed) {
            "cascading_degraded"
        } else {
            "cascading"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt_block::PromptBlock;

    /// A primary that always errors — exercises the fallback path.
    struct AlwaysFails;
    #[async_trait]
    impl TokenCounter for AlwaysFails {
        async fn count_blocks(&self, _blocks: &[PromptBlock]) -> Result<u32, LlmError> {
            Err(LlmError::Other(anyhow::anyhow!("forced failure")))
        }
        async fn count_messages(
            &self,
            _model: &str,
            _messages: &[ChatMessage],
        ) -> Result<u32, LlmError> {
            Err(LlmError::Other(anyhow::anyhow!("forced failure")))
        }
        fn is_exact(&self) -> bool {
            true
        }
        fn backend(&self) -> &'static str {
            "always_fails"
        }
    }

    /// A primary that always succeeds with a fixed value.
    struct AlwaysOk;
    #[async_trait]
    impl TokenCounter for AlwaysOk {
        async fn count_blocks(&self, _blocks: &[PromptBlock]) -> Result<u32, LlmError> {
            Ok(42)
        }
        async fn count_messages(
            &self,
            _model: &str,
            _messages: &[ChatMessage],
        ) -> Result<u32, LlmError> {
            Ok(42)
        }
        fn is_exact(&self) -> bool {
            true
        }
        fn backend(&self) -> &'static str {
            "always_ok"
        }
    }

    #[tokio::test]
    async fn primary_success_passes_through() {
        let casc = CascadingTokenCounter::new(Arc::new(AlwaysOk));
        let n = casc.count_blocks(&[]).await.unwrap();
        assert_eq!(n, 42);
        assert!(casc.is_exact());
        assert_eq!(casc.backend(), "cascading");
    }

    #[tokio::test]
    async fn primary_failure_falls_back_to_tiktoken() {
        let casc = CascadingTokenCounter::new(Arc::new(AlwaysFails));
        let blocks = vec![PromptBlock::plain("x", "Hello, world!")];
        let n = casc.count_blocks(&blocks).await.unwrap();
        assert_eq!(n, 4); // tiktoken known phrase
        assert!(!casc.is_exact(), "ever_fell_back should flip is_exact to false");
        assert_eq!(casc.backend(), "cascading_degraded");
    }

    #[tokio::test]
    async fn fallback_label_persists_after_first_trip() {
        let casc = CascadingTokenCounter::new(Arc::new(AlwaysFails));
        // Trip and let breaker open.
        for _ in 0..5 {
            let _ = casc.count_blocks(&[]).await;
        }
        assert!(!casc.is_exact());
        assert_eq!(casc.backend(), "cascading_degraded");
    }
}
