use async_trait::async_trait;

use nexo_tool_meta::marketing::EnrichmentResult;

/// Outcome of running the chain — either we got a result that
/// passed the confidence threshold (`Hit`) or we walked every
/// source and nothing met the bar (`AllExhausted`). Caller
/// decides what to do with `AllExhausted` (typically: mark the
/// person `personal_only_giveup` and surface a manual prompt
/// in the operator UI).
#[derive(Debug, Clone, PartialEq)]
pub enum FallbackOutcome {
    Hit {
        result: EnrichmentResult,
        /// Index of the source in the chain that produced the
        /// hit. Useful for telemetry ("75% of enrichments
        /// resolved at source #2 — promote it to #0").
        source_index: usize,
    },
    AllExhausted {
        /// Every result the chain produced, in priority order.
        /// Empty when no source produced anything.
        attempts: Vec<EnrichmentResult>,
    },
}

/// Per-source contract. Implementations live in the consumer
/// (the marketing extension's `identity/adapters/*`) — the SDK
/// only owns the trait + the chain runner so the contract
/// stays consistent across consumers.
#[async_trait]
pub trait EnrichmentSource: Send + Sync {
    /// Source name for telemetry + audit ("display_name",
    /// "signature", "llm_extractor", "cross_thread", "apollo").
    fn name(&self) -> &str;

    /// Best-effort estimate of the source's cost. The chain
    /// uses this only for telemetry — caller orders sources
    /// by priority directly.
    fn cost_estimate(&self) -> SourceCost;

    /// Run the source against the given input. Return `Ok(None)`
    /// for a soft miss (source ran but didn't infer anything;
    /// chain continues), `Ok(Some(result))` for a hit (chain
    /// stops if confidence ≥ threshold), `Err(_)` for a hard
    /// failure (chain logs + continues).
    async fn extract(
        &self,
        input: &EnrichmentInput<'_>,
    ) -> Result<Option<EnrichmentResult>, EnrichmentSourceError>;
}

/// Cost classes — purely informational. The chain doesn't
/// budget by cost; the consumer orders sources cheap-first by
/// putting them earlier in the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCost {
    /// Pure CPU, no I/O.
    Free,
    /// Local DB lookup or in-memory cache hit.
    Cheap,
    /// LLM call or remote HTTP.
    Moderate,
    /// Paid third-party API (Apollo / Hunter / similar).
    Paid,
}

/// What the chain hands to each source. Cheap to construct;
/// holds borrowed references so adapters don't pay an alloc
/// per call.
#[derive(Debug, Clone, Copy)]
pub struct EnrichmentInput<'a> {
    pub from_email: &'a str,
    pub from_display_name: Option<&'a str>,
    pub subject: &'a str,
    pub body_excerpt: &'a str,
    pub reply_to: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum EnrichmentSourceError {
    /// Source needed a network call that failed (HTTP error,
    /// LLM timeout, paid-API quota). `source_name` avoids the
    /// thiserror `#[source]` attribute keyword collision.
    #[error("source {source_name:?} unavailable: {reason}")]
    SourceUnavailable { source_name: String, reason: String },
    /// Source's input failed validation (empty email, etc.).
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// Chain runner. Walks sources in declaration order; stops at
/// the first source whose result has confidence ≥
/// `confidence_threshold`. Hard errors are logged + skipped so
/// one flaky LLM doesn't kill the whole chain.
pub struct FallbackChain {
    sources: Vec<Box<dyn EnrichmentSource>>,
    confidence_threshold: f32,
}

impl FallbackChain {
    /// Build a chain from an ordered source list + a confidence
    /// threshold (typical: 0.7 — operator confirms anything
    /// below).
    pub fn new(sources: Vec<Box<dyn EnrichmentSource>>, confidence_threshold: f32) -> Self {
        Self {
            sources,
            confidence_threshold,
        }
    }

    pub fn confidence_threshold(&self) -> f32 {
        self.confidence_threshold
    }

    pub fn source_names(&self) -> Vec<&str> {
        self.sources.iter().map(|s| s.name()).collect()
    }

    pub async fn run(&self, input: &EnrichmentInput<'_>) -> FallbackOutcome {
        let mut attempts: Vec<EnrichmentResult> = Vec::new();
        for (idx, source) in self.sources.iter().enumerate() {
            match source.extract(input).await {
                Ok(Some(result)) => {
                    if result.confidence >= self.confidence_threshold {
                        return FallbackOutcome::Hit {
                            result,
                            source_index: idx,
                        };
                    }
                    attempts.push(result);
                }
                Ok(None) => {
                    // Soft miss — continue.
                }
                Err(e) => {
                    // Hard error — log + continue. Don't abort
                    // the chain; one source failing shouldn't
                    // tank the resolver.
                    tracing::warn!(
                        source = source.name(),
                        error = %e,
                        "enrichment source failed; continuing"
                    );
                }
            }
        }
        FallbackOutcome::AllExhausted { attempts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSource {
        name: &'static str,
        result: Option<EnrichmentResult>,
        error: Option<String>,
    }

    impl StubSource {
        fn hit(name: &'static str, conf: f32) -> Self {
            Self {
                name,
                result: Some(EnrichmentResult {
                    source: name.into(),
                    confidence: conf,
                    person_inferred: None,
                    company_inferred: None,
                    note: Some(format!("from {name}")),
                }),
                error: None,
            }
        }
        fn miss(name: &'static str) -> Self {
            Self {
                name,
                result: None,
                error: None,
            }
        }
        fn errs(name: &'static str, reason: &'static str) -> Self {
            Self {
                name,
                result: None,
                error: Some(reason.to_string()),
            }
        }
    }

    #[async_trait]
    impl EnrichmentSource for StubSource {
        fn name(&self) -> &str {
            self.name
        }
        fn cost_estimate(&self) -> SourceCost {
            SourceCost::Free
        }
        async fn extract(
            &self,
            _input: &EnrichmentInput<'_>,
        ) -> Result<Option<EnrichmentResult>, EnrichmentSourceError> {
            if let Some(reason) = &self.error {
                return Err(EnrichmentSourceError::SourceUnavailable {
                    source_name: self.name.to_string(),
                    reason: reason.clone(),
                });
            }
            Ok(self.result.clone())
        }
    }

    fn input<'a>() -> EnrichmentInput<'a> {
        EnrichmentInput {
            from_email: "juan@gmail.com",
            from_display_name: Some("Juan García (Acme)"),
            subject: "Hi",
            body_excerpt: "Hola",
            reply_to: None,
        }
    }

    #[tokio::test]
    async fn first_hit_above_threshold_wins() {
        let chain = FallbackChain::new(
            vec![
                Box::new(StubSource::miss("display_name")),
                Box::new(StubSource::hit("signature", 0.85)),
                Box::new(StubSource::hit("llm", 0.95)), // never reached
            ],
            0.7,
        );
        let out = chain.run(&input()).await;
        match out {
            FallbackOutcome::Hit {
                result,
                source_index,
            } => {
                assert_eq!(result.source, "signature");
                assert_eq!(source_index, 1);
            }
            _ => panic!("expected Hit"),
        }
    }

    #[tokio::test]
    async fn below_threshold_continues_to_next_source() {
        let chain = FallbackChain::new(
            vec![
                Box::new(StubSource::hit("signature", 0.50)),
                Box::new(StubSource::hit("llm", 0.85)),
            ],
            0.7,
        );
        let out = chain.run(&input()).await;
        match out {
            FallbackOutcome::Hit {
                result,
                source_index,
            } => {
                assert_eq!(result.source, "llm");
                assert_eq!(source_index, 1);
            }
            _ => panic!("expected Hit"),
        }
    }

    #[tokio::test]
    async fn all_misses_returns_exhausted() {
        let chain = FallbackChain::new(
            vec![
                Box::new(StubSource::miss("a")),
                Box::new(StubSource::miss("b")),
            ],
            0.7,
        );
        let out = chain.run(&input()).await;
        assert!(
            matches!(out, FallbackOutcome::AllExhausted { ref attempts } if attempts.is_empty())
        );
    }

    #[tokio::test]
    async fn below_threshold_attempts_collected_in_exhausted() {
        let chain = FallbackChain::new(
            vec![
                Box::new(StubSource::hit("a", 0.40)),
                Box::new(StubSource::hit("b", 0.55)),
            ],
            0.7,
        );
        let out = chain.run(&input()).await;
        match out {
            FallbackOutcome::AllExhausted { attempts } => {
                assert_eq!(attempts.len(), 2);
                assert_eq!(attempts[0].source, "a");
                assert_eq!(attempts[1].source, "b");
            }
            _ => panic!("expected AllExhausted"),
        }
    }

    #[tokio::test]
    async fn hard_error_skipped_chain_continues() {
        let chain = FallbackChain::new(
            vec![
                Box::new(StubSource::errs("flaky-llm", "timeout")),
                Box::new(StubSource::hit("signature", 0.85)),
            ],
            0.7,
        );
        let out = chain.run(&input()).await;
        assert!(matches!(
            out,
            FallbackOutcome::Hit {
                source_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn empty_chain_returns_exhausted() {
        let chain = FallbackChain::new(vec![], 0.7);
        let out = chain.run(&input()).await;
        assert!(matches!(out, FallbackOutcome::AllExhausted { .. }));
    }
}
