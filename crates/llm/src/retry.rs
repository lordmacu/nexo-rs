use std::time::Duration;

use nexo_config::types::llm::RetryConfig;

use crate::rate_limit_info::{
    format_rate_limit_message, record_quota_event, LlmProvider, QuotaEvent, QuotaStatus,
    RateLimitInfo, RateLimitSeverity, RateLimitWindow,
};

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

    /// Phase C4.c — hard quota rejection. Distinct from
    /// `RateLimit` (transient burst, retry will succeed) because
    /// retry will NOT help: the operator must wait until reset
    /// or upgrade their plan / switch model.
    ///
    /// Promoted from a 429 by [`classify_429_error`] when the
    /// extracted [`RateLimitInfo::status`] is
    /// `Some(QuotaStatus::Rejected)` AND the formatter produces
    /// a human-readable message. `with_retry` short-circuits
    /// this variant — no retry attempts, no backoff, propagated
    /// to the caller immediately.
    ///
    /// IRROMPIBLE refs:
    /// - claude-code-leak `services/api/errors.ts:465-548` —
    ///   3-tier 429 classification (rateLimitType +
    ///   overageStatus → hard quota; entitlement reject;
    ///   plain infra-capacity 429).
    /// - claude-code-leak `services/rateLimitMessages.ts:45-104`
    ///   `getRateLimitMessage` — already ported as
    ///   `format_rate_limit_message`.
    ///
    /// Provider-agnostic: `provider` field carries the source
    /// (Anthropic / OpenAI / Gemini / MiniMax / Generic for
    /// xAI / DeepSeek / Mistral compat-mode).
    #[error("LLM quota exceeded ({}): {message}",
        provider.map(|p| format!("{p:?}")).unwrap_or_else(|| "unknown".into()))]
    QuotaExceeded {
        retry_after_ms: Option<u64>,
        severity: RateLimitSeverity,
        message: String,
        plan_hint: Option<String>,
        provider: Option<LlmProvider>,
        window: Option<RateLimitWindow>,
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

/// Construct the appropriate 429 error variant from a provider's
/// extracted `RateLimitInfo`. When `info` is `Some` and its
/// `status == Rejected` AND the formatter produces a message,
/// returns [`LlmError::QuotaExceeded`] (do not retry) AND records
/// the event in the process-wide cache via
/// [`record_quota_event`]. Otherwise returns [`LlmError::RateLimit`]
/// (retry transient bursts).
///
/// Provider-agnostic: works regardless of which extractor produced
/// the `RateLimitInfo`. Same shape across Anthropic / MiniMax /
/// OpenAI / Gemini / DeepSeek / xAI / Mistral.
pub fn classify_429_error(retry_after_ms: u64, info: Option<RateLimitInfo>) -> LlmError {
    let Some(info) = info else {
        return LlmError::RateLimit {
            retry_after_ms,
            rate_limit_info: None,
        };
    };

    if matches!(info.status, Some(QuotaStatus::Rejected)) {
        if let Some(msg) = format_rate_limit_message(&info) {
            let provider = info.provider.unwrap_or(LlmProvider::Generic);
            let event = QuotaEvent {
                at: chrono::Utc::now(),
                provider,
                severity: msg.severity,
                message: msg.text.clone(),
                plan_hint: msg.plan_hint.clone(),
                window: info.window,
                resets_at: info.resets_at,
            };
            record_quota_event(event);
            return LlmError::QuotaExceeded {
                retry_after_ms: Some(retry_after_ms),
                severity: msg.severity,
                message: msg.text,
                plan_hint: msg.plan_hint,
                provider: info.provider,
                window: info.window,
            };
        }
    }

    LlmError::RateLimit {
        retry_after_ms,
        rate_limit_info: Some(info),
    }
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
            // Phase C4.c — hard quota rejection. Retry will not
            // help; propagate immediately so the operator-facing
            // surface (notify_origin / setup doctor / admin-ui)
            // can render the plan_hint without delay.
            Err(e @ LlmError::QuotaExceeded { .. }) => return Err(e),
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
    use super::*;
    use crate::rate_limit_info::{
        clear_last_quota, last_quota_event_for, LlmProvider, QuotaStatus, RateLimitInfo,
        RateLimitWindow,
    };

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

    // ── Phase C4.c — QuotaExceeded promotion ──

    fn rejected_anthropic_info() -> RateLimitInfo {
        RateLimitInfo {
            provider: Some(LlmProvider::Anthropic),
            status: Some(QuotaStatus::Rejected),
            resets_at: Some(1_700_000_000),
            window: Some(RateLimitWindow::ShortTerm),
            utilization: Some(1.0),
            ..Default::default()
        }
    }

    #[test]
    fn quota_exceeded_promoted_when_status_rejected() {
        clear_last_quota();
        let info = rejected_anthropic_info();
        let err = classify_429_error(60_000, Some(info));
        match err {
            LlmError::QuotaExceeded {
                retry_after_ms,
                provider,
                window,
                ..
            } => {
                assert_eq!(retry_after_ms, Some(60_000));
                assert_eq!(provider, Some(LlmProvider::Anthropic));
                assert_eq!(window, Some(RateLimitWindow::ShortTerm));
            }
            other => panic!("expected QuotaExceeded, got {other:?}"),
        }
        // Side effect: cache populated.
        assert!(last_quota_event_for(LlmProvider::Anthropic).is_some());
    }

    #[test]
    fn rate_limit_kept_when_status_allowed_warning() {
        clear_last_quota();
        let info = RateLimitInfo {
            provider: Some(LlmProvider::Anthropic),
            status: Some(QuotaStatus::AllowedWarning),
            utilization: Some(0.85),
            ..Default::default()
        };
        let err = classify_429_error(30_000, Some(info));
        assert!(
            matches!(err, LlmError::RateLimit { .. }),
            "AllowedWarning must NOT promote to QuotaExceeded: got {err:?}",
        );
    }

    #[test]
    fn rate_limit_kept_when_no_info() {
        let err = classify_429_error(30_000, None);
        match err {
            LlmError::RateLimit {
                retry_after_ms,
                rate_limit_info,
            } => {
                assert_eq!(retry_after_ms, 30_000);
                assert!(rate_limit_info.is_none());
            }
            other => panic!("expected plain RateLimit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn with_retry_does_not_retry_quota_exceeded() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let calls_for_closure = std::sync::Arc::clone(&calls);
        let cfg = RetryConfig {
            max_attempts: 5,
            initial_backoff_ms: 1,
            max_backoff_ms: 10,
            backoff_multiplier: 2.0,
        };
        let result: Result<(), LlmError> = with_retry(&cfg, move || {
            let calls = std::sync::Arc::clone(&calls_for_closure);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(LlmError::QuotaExceeded {
                    retry_after_ms: Some(60_000),
                    severity: crate::rate_limit_info::RateLimitSeverity::Error,
                    message: "quota hit".into(),
                    plan_hint: None,
                    provider: Some(LlmProvider::Anthropic),
                    window: Some(RateLimitWindow::ShortTerm),
                })
            }
        })
        .await;
        assert!(matches!(result, Err(LlmError::QuotaExceeded { .. })));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "QuotaExceeded must short-circuit — only 1 call expected",
        );
    }

    #[test]
    fn quota_exceeded_display_includes_provider_label() {
        let err = LlmError::QuotaExceeded {
            retry_after_ms: Some(1_000),
            severity: crate::rate_limit_info::RateLimitSeverity::Error,
            message: "5h limit hit".into(),
            plan_hint: None,
            provider: Some(LlmProvider::Anthropic),
            window: Some(RateLimitWindow::ShortTerm),
        };
        let s = format!("{err}");
        assert!(s.contains("Anthropic"), "expected provider label: {s}");
        assert!(s.contains("5h limit hit"), "expected message: {s}");
    }
}
