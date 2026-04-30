use std::time::Duration;

use nexo_config::types::llm::RetryConfig;

/// Parse an HTTP `Retry-After` header into milliseconds. Handles both the
/// seconds form (`"42"`) and the HTTP-date form (`"Wed, 21 Oct 2026 …"`).
/// On an unparseable value returns the `fallback_ms` so callers don't
/// accidentally hammer a throttled endpoint (Anthropic's spec allows
/// HTTP-dates and the old default of 1 second was way too aggressive).
pub fn parse_retry_after_ms(
    headers: &reqwest::header::HeaderMap,
    header_name: &str,
    fallback_ms: u64,
) -> u64 {
    let Some(raw) = headers.get(header_name).and_then(|v| v.to_str().ok()) else {
        return fallback_ms;
    };
    if let Ok(secs) = raw.parse::<u64>() {
        return secs.saturating_mul(1000);
    }
    if let Ok(when) = chrono::DateTime::parse_from_rfc2822(raw) {
        let now = chrono::Utc::now();
        let delta = when.with_timezone(&chrono::Utc) - now;
        let ms = delta.num_milliseconds().max(0) as u64;
        return ms.max(1_000);
    }
    fallback_ms
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("rate limited — retry after {retry_after_ms}ms")]
    RateLimit {
        retry_after_ms: u64,
        /// Structured rate-limit info extracted from provider headers.
        /// `None` when the provider didn't return rate-limit headers
        /// or extraction failed.
        rate_limit_info: Option<crate::rate_limit_info::RateLimitInfo>,
    },

    #[error("server error {status}: {body}")]
    ServerError { status: u16, body: String },

    /// Credential rejected by the provider (401 invalid_grant, expired
    /// setup-token, wrong API key). Never retried — the operator must
    /// re-authenticate. Not counted as a circuit-breaker failure
    /// because the provider is healthy; our key is not.
    #[error("credential invalid: {hint}")]
    CredentialInvalid { hint: String },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Classify an HTTP status into the retry strategy.
pub enum RetryClass {
    /// 429 — rate limited. Up to 5 attempts, exponential backoff.
    RateLimit,
    /// 5xx — server error. Up to 3 attempts, exponential backoff.
    Server,
    /// Any other error — do not retry.
    Fatal,
}

pub fn classify(status: u16) -> RetryClass {
    match status {
        429 => RetryClass::RateLimit,
        500..=599 => RetryClass::Server,
        _ => RetryClass::Fatal,
    }
}

/// Decorrelated jitter: `next_backoff ∈ [base, max(base, last * multiplier)]`
/// with a uniform draw. Keeps the retry loop bounded while decorrelating
/// fleet retries to avoid the thundering herd after a widespread 429.
fn jittered_backoff(base_ms: u64, last_ms: u64, multiplier: f32, max_ms: u64) -> u64 {
    let hi = ((last_ms as f32) * multiplier).max(base_ms as f32) as u64;
    let hi = hi.min(max_ms).max(base_ms);
    if hi <= base_ms {
        return base_ms.min(max_ms);
    }
    // Cheap entropy from wall-clock nanos; doesn't need to be
    // cryptographically random — just enough to desynchronize callers.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let span = hi - base_ms + 1;
    base_ms + (nanos % span)
}

/// Execute `f` with retry according to `config`.
/// `f` returns `Result<T, LlmError>`.
pub async fn with_retry<T, F, Fut>(config: &RetryConfig, mut f: F) -> Result<T, LlmError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, LlmError>>,
{
    let mut attempt = 0u32;
    let mut backoff_ms = config.initial_backoff_ms;

    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(LlmError::RateLimit {
                retry_after_ms,
                ref rate_limit_info,
            }) => {
                attempt += 1;
                if attempt >= 5 {
                    return Err(LlmError::RateLimit {
                        retry_after_ms,
                        rate_limit_info: rate_limit_info.clone(),
                    });
                }
                let wait = retry_after_ms.max(backoff_ms);
                // Enrich retry log with human-readable quota info when available.
                if let Some(info) = rate_limit_info {
                    if let Some(msg) = crate::rate_limit_info::format_rate_limit_message(info) {
                        tracing::warn!(
                            attempt,
                            wait_ms = wait,
                            severity = ?msg.severity,
                            plan_hint = msg.plan_hint,
                            "LLM rate limited — retrying: {}",
                            msg.text
                        );
                    } else {
                        tracing::warn!(attempt, wait_ms = wait, "LLM rate limited — retrying");
                    }
                } else {
                    tracing::warn!(attempt, wait_ms = wait, "LLM rate limited — retrying");
                }
                tokio::time::sleep(Duration::from_millis(wait)).await;
                backoff_ms = jittered_backoff(
                    config.initial_backoff_ms,
                    backoff_ms,
                    config.backoff_multiplier,
                    config.max_backoff_ms,
                );
            }
            Err(LlmError::ServerError { status, ref body }) => {
                attempt += 1;
                if attempt >= 3 {
                    return Err(LlmError::ServerError {
                        status,
                        body: body.clone(),
                    });
                }
                tracing::warn!(attempt, status, "LLM server error — retrying");
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = jittered_backoff(
                    config.initial_backoff_ms,
                    backoff_ms,
                    config.backoff_multiplier,
                    config.max_backoff_ms,
                );
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::jittered_backoff;

    #[test]
    fn jitter_bounded_by_range() {
        for _ in 0..50 {
            let b = jittered_backoff(100, 400, 2.0, 10_000);
            assert!((100..=800).contains(&b), "got {b}");
        }
    }

    #[test]
    fn jitter_respects_max() {
        let b = jittered_backoff(100, 10_000, 2.0, 5_000);
        assert!((100..=5_000).contains(&b), "got {b}");
    }
}
