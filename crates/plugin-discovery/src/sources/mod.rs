//! Discovery sources.
//!
//! Each source produces a `Vec<DiscoveredPlugin>` independently;
//! the runner in 98.8 fans them out in parallel + merges by
//! `name`. A source MUST be best-effort: rate limits, 5xx, network
//! errors all map to `SourceError` rather than panicking — the
//! runner aggregates partial failures so a single broken source
//! never blanks the catalogue.

pub mod crates_io;
pub mod curated_index;
pub mod github_topic;

use async_trait::async_trait;

use crate::types::{DiscoveredPlugin, SourceError};

/// Minimum surface a discovery source exposes. Implementors
/// encapsulate their own HTTP client (already configured with
/// timeout + User-Agent at construction).
#[async_trait]
pub trait Source: Send + Sync {
    /// Stable identifier shown in `SourceError.source` + admin RPC
    /// `partial_failures.source`. Lowercase snake_case.
    fn name(&self) -> &'static str;

    /// Fetch the source's contribution to the catalogue. Returns
    /// `Ok(items)` on success (empty Vec is legitimate — source had
    /// nothing to contribute), `Err(SourceError)` on transport /
    /// parse / rate-limit failure.
    async fn fetch(&self) -> Result<Vec<DiscoveredPlugin>, SourceError>;
}

/// Build a typed `SourceError` for a source. Trimmed to 256 chars
/// to keep the UI banner readable and protect against pathological
/// upstream messages.
pub fn source_error(source: &str, message: impl Into<String>) -> SourceError {
    let mut message = message.into();
    if message.len() > 256 {
        message.truncate(256);
    }
    SourceError {
        source: source.to_string(),
        message,
    }
}

/// Aggregated result from [`run_all`]. Items come from any source
/// that succeeded; failures land in `partial_failures` so the UI's
/// `<PartialFailureBanner>` can surface them without blanking the
/// catalogue.
#[derive(Debug, Default, Clone)]
pub struct RunOutcome {
    /// Per-source contributions concatenated (NO merge yet — merge
    /// happens in `crate::merge` after manifests are fetched).
    pub items: Vec<DiscoveredPlugin>,
    /// One entry per source that failed (timeout / HTTP error /
    /// breaker-open / parse error). Empty when every source
    /// succeeded.
    pub partial_failures: Vec<SourceError>,
}

/// Drive every source in parallel, enforce a per-source timeout,
/// and route each call through a circuit breaker keyed by source
/// name so a chronically flaky source doesn't burn bandwidth on
/// every refresh.
///
/// Behaviour invariants:
///   1. One slow source NEVER stalls the others — `tokio::join!`
///      lets faster sources land first; the timeout wraps each
///      future individually.
///   2. A breaker `Open` short-circuits the call without HTTP —
///      surfaces in `partial_failures` as `"<name>: circuit open"`.
///   3. A timeout surfaces as `"<name>: timed out after Ns"`.
///   4. Per-source success/failure transitions the breaker.
pub async fn run_all(
    sources: &[Box<dyn Source>],
    per_source_timeout: std::time::Duration,
) -> RunOutcome {
    use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitError};

    let mut handles = Vec::with_capacity(sources.len());
    for src in sources.iter() {
        let name = src.name();
        let cfg = CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            initial_backoff: std::time::Duration::from_secs(60),
            max_backoff: std::time::Duration::from_secs(600),
        };
        let breaker = CircuitBreaker::new(format!("plugin_discovery::{name}"), cfg);
        let timeout = per_source_timeout;
        let fut = async move {
            let res: Result<Vec<DiscoveredPlugin>, CircuitError<SourceError>> = breaker
                .call(|| async {
                    match tokio::time::timeout(timeout, src.fetch()).await {
                        Ok(inner) => inner,
                        Err(_elapsed) => Err(source_error(
                            name,
                            format!("timed out after {}s", timeout.as_secs()),
                        )),
                    }
                })
                .await;
            (name, res)
        };
        handles.push(fut);
    }
    let results = futures::future::join_all(handles).await;
    let mut outcome = RunOutcome::default();
    for (name, res) in results {
        match res {
            Ok(items) => outcome.items.extend(items),
            Err(CircuitError::Open(_)) => {
                outcome.partial_failures.push(source_error(
                    name,
                    "circuit open — source flaky, see breaker logs",
                ));
            }
            Err(CircuitError::Inner(e)) => {
                // `e` is already a SourceError; preserve message.
                outcome.partial_failures.push(e);
            }
        }
    }
    outcome
}
