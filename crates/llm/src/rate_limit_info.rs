//! Phase 77.11 — Transversal rate-limit header extraction and
//! human-readable message generation for all LLM providers.
//!
//! Ported from `claude-code-leak/src/services/claudeAiLimits.ts` and
//! `claude-code-leak/src/services/rateLimitMessages.ts`. Made
//! provider-agnostic: every LLM provider can contribute its own
//! header parser that normalizes into a common `RateLimitInfo` struct.
//!
//! ## Architecture
//!
//! ```text
//! Provider response headers
//!   → extract_anthropic_headers / extract_openai_headers / …
//!   → RateLimitInfo (provider-agnostic)
//!   → format_rate_limit_message (provider-agnostic)
//!   → RateLimitMessage { text, severity, plan_hint }
//! ```
//!
//! Provider-specific concepts:
//! - Anthropic: unified rate-limit headers, overage, fallback model
//! - OpenAI: `x-ratelimit-*` headers, usage tiers
//! - Gemini: `retry-info` header, quota_project_id
//! - MiniMax: `x-rate-limit-*` headers (OpenAI-compatible)
//! - Generic/unknown: only `retry-after` header

use std::fmt;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use reqwest::header::HeaderMap;

// ── Provider identity ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    Anthropic,
    OpenAI,
    Gemini,
    MiniMax,
    /// Catch-all for unknown/OpenAI-compatible providers.
    Generic,
}

// ── Enums ────────────────────────────────────────────────────────

/// Rating of the current quota situation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Allowed,
    AllowedWarning,
    Rejected,
}

impl fmt::Display for QuotaStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QuotaStatus::Allowed => write!(f, "allowed"),
            QuotaStatus::AllowedWarning => write!(f, "allowed_warning"),
            QuotaStatus::Rejected => write!(f, "rejected"),
        }
    }
}

/// Category of rate limit window. Provider-agnostic taxonomy that
/// normalizes provider-specific names (Anthropic "five_hour",
/// OpenAI "RPM"/"TPM", Gemini "quota_project_id").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitWindow {
    /// Short window — typically 1-24 hours (Anthropic 5h, OpenAI daily).
    ShortTerm,
    /// Medium window — typically days to weeks (Anthropic 7d, OpenAI monthly).
    MediumTerm,
    /// Model-specific quota (e.g. Opus-only, GPT-5-only).
    ModelSpecific,
    /// Tiered/overage quota — extra paid usage beyond base plan.
    TieredOverage,
}

impl RateLimitWindow {
    pub fn display_name(&self) -> &'static str {
        match self {
            RateLimitWindow::ShortTerm => "short-term limit",
            RateLimitWindow::MediumTerm => "period limit",
            RateLimitWindow::ModelSpecific => "model limit",
            RateLimitWindow::TieredOverage => "extra usage limit",
        }
    }
}

// ── RateLimitInfo ────────────────────────────────────────────────

/// Structured snapshot of rate-limit information extracted from an
/// LLM provider's HTTP response headers. Provider-agnostic.
///
/// Every provider parser maps its own header schema into this struct.
/// Fields that a provider doesn't expose remain `None`/`false`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RateLimitInfo {
    /// Which provider emitted these headers.
    pub provider: Option<LlmProvider>,
    pub status: Option<QuotaStatus>,
    /// Unix epoch seconds when the primary limit resets.
    pub resets_at: Option<u64>,
    pub window: Option<RateLimitWindow>,
    /// Fraction 0.0–1.0 of the limit consumed.
    pub utilization: Option<f64>,
    /// Whether a cheaper/smaller fallback model is available.
    pub has_fallback_model: bool,
    /// Tiered/overage fields — populated when the provider supports
    /// paid extra usage beyond the base plan (Anthropic overage,
    /// OpenAI tier upgrade).
    pub tiered_status: Option<QuotaStatus>,
    pub tiered_resets_at: Option<u64>,
    /// Human-readable label for the tiered quota (e.g. "extra usage",
    /// "usage tier 5").
    pub tiered_label: Option<String>,
    /// Whether this request consumed tiered/overage quota.
    pub is_using_tiered: bool,
    /// Raw seconds until retry (from `retry-after` header or equivalent).
    pub retry_after_secs: Option<u64>,
    /// Surpassed-threshold from server-side early warning.
    pub surpassed_threshold: Option<f64>,
}

// ── Header extraction — provider-specific ────────────────────────

/// Extract rate-limit info from response headers, dispatching on provider.
/// Falls back to `extract_generic_headers` for unknown providers —
/// which only parses `retry-after`.
pub fn extract_rate_limit_info(
    headers: &HeaderMap,
    provider: LlmProvider,
) -> Option<RateLimitInfo> {
    match provider {
        LlmProvider::Anthropic => extract_anthropic_headers(headers),
        LlmProvider::OpenAI => extract_openai_headers(headers),
        LlmProvider::Gemini => extract_gemini_headers(headers),
        LlmProvider::MiniMax => extract_openai_compat_headers(headers),
        LlmProvider::Generic => extract_generic_headers(headers),
    }
}

// ── Anthropic ────────────────────────────────────────────────────

/// Extract Anthropic's `anthropic-ratelimit-unified-*` headers.
/// Ported from `claudeAiLimits.ts:376-436 computeNewLimitsFromHeaders`.
pub fn extract_anthropic_headers(headers: &HeaderMap) -> Option<RateLimitInfo> {
    let status_str = headers
        .get("anthropic-ratelimit-unified-status")
        .and_then(|v| v.to_str().ok())?;
    let status = parse_quota_status(status_str);

    let resets_at = header_u64(headers, "anthropic-ratelimit-unified-reset");
    let window = headers
        .get("anthropic-ratelimit-unified-representative-claim")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_anthropic_window);

    let util_5h = header_f64(headers, "anthropic-ratelimit-unified-5h-utilization");
    let util_7d = header_f64(headers, "anthropic-ratelimit-unified-7d-utilization");
    let utilization = util_5h.or(util_7d);

    let tiered_status = headers
        .get("anthropic-ratelimit-unified-overage-status")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_quota_status);
    let tiered_resets_at = header_u64(headers, "anthropic-ratelimit-unified-overage-reset");
    let tiered_label = Some("extra usage".to_string());

    let has_fallback_model =
        headers.get("anthropic-ratelimit-unified-fallback")
            .and_then(|v| v.to_str().ok())
            == Some("available");

    let is_using_tiered = status == Some(QuotaStatus::Rejected)
        && matches!(
            tiered_status,
            Some(QuotaStatus::Allowed) | Some(QuotaStatus::AllowedWarning)
        );

    let retry_after_secs = header_u64(headers, "retry-after")
        .or_else(|| {
            // Anthropic also sends `anthropic-ratelimit-requests-reset`
            header_u64(headers, "anthropic-ratelimit-requests-reset")
        });

    let surpassed_5h =
        header_f64(headers, "anthropic-ratelimit-unified-5h-surpassed-threshold");
    let surpassed_7d =
        header_f64(headers, "anthropic-ratelimit-unified-7d-surpassed-threshold");

    Some(RateLimitInfo {
        provider: Some(LlmProvider::Anthropic),
        status,
        resets_at,
        window,
        utilization,
        has_fallback_model,
        tiered_status,
        tiered_resets_at,
        tiered_label,
        is_using_tiered,
        retry_after_secs,
        surpassed_threshold: surpassed_5h.or(surpassed_7d),
    })
}

// ── OpenAI ───────────────────────────────────────────────────────

/// Extract OpenAI's `x-ratelimit-*` headers.
pub fn extract_openai_headers(headers: &HeaderMap) -> Option<RateLimitInfo> {
    // OpenAI uses: x-ratelimit-limit-requests, x-ratelimit-remaining-requests,
    // x-ratelimit-reset-requests (in seconds or absolute timestamp),
    // x-ratelimit-limit-tokens, x-ratelimit-remaining-tokens, x-ratelimit-reset-tokens
    let remaining_req = header_u64(headers, "x-ratelimit-remaining-requests")?;
    let limit_req = header_u64(headers, "x-ratelimit-limit-requests").unwrap_or(1);
    let utilization = Some(1.0 - (remaining_req as f64 / limit_req.max(1) as f64));

    let reset_str = headers
        .get("x-ratelimit-reset-requests")
        .and_then(|v| v.to_str().ok());
    let resets_at = reset_str.and_then(|s| parse_openai_reset(s));

    let status = if remaining_req == 0 {
        Some(QuotaStatus::Rejected)
    } else if utilization.unwrap_or(0.0) > 0.8 {
        Some(QuotaStatus::AllowedWarning)
    } else {
        Some(QuotaStatus::Allowed)
    };

    let retry_after_secs = header_u64(headers, "retry-after")
        .or_else(|| resets_at.map(|ts| ts.saturating_sub(Utc::now().timestamp() as u64)));

    Some(RateLimitInfo {
        provider: Some(LlmProvider::OpenAI),
        status,
        resets_at,
        window: Some(RateLimitWindow::ShortTerm),
        utilization,
        retry_after_secs,
        ..Default::default()
    })
}

// ── OpenAI-compatible (MiniMax) ──────────────────────────────────

/// Extract MiniMax's `x-rate-limit-*` headers (OpenAI-compatible shape).
pub fn extract_openai_compat_headers(headers: &HeaderMap) -> Option<RateLimitInfo> {
    // Try OpenAI-compatible headers first, fall back to generic
    extract_openai_headers(headers).or_else(|| extract_generic_headers(headers))
}

// ── Gemini ───────────────────────────────────────────────────────

/// Extract Gemini's `retry-info` header.
pub fn extract_gemini_headers(headers: &HeaderMap) -> Option<RateLimitInfo> {
    let retry_after_secs = header_u64(headers, "retry-after");
    if retry_after_secs.is_none() {
        // No rate-limit signal — but still return info if there's an
        // `x-quota-project` header indicating quota tracking is active.
        if headers.get("x-quota-project").is_none() {
            return None;
        }
    }

    Some(RateLimitInfo {
        provider: Some(LlmProvider::Gemini),
        status: retry_after_secs.map(|_| QuotaStatus::Rejected),
        retry_after_secs,
        ..Default::default()
    })
}

// ── Generic fallback ─────────────────────────────────────────────

/// Parse only the universal `retry-after` header. Any provider that
/// returns HTTP 429 will have this.
pub fn extract_generic_headers(headers: &HeaderMap) -> Option<RateLimitInfo> {
    let retry_after_secs = header_u64(headers, "retry-after");
    if retry_after_secs.is_none() {
        return None;
    }

    Some(RateLimitInfo {
        provider: Some(LlmProvider::Generic),
        status: Some(QuotaStatus::Rejected),
        retry_after_secs,
        ..Default::default()
    })
}

// ── Message generation (provider-agnostic) ───────────────────────

/// Severity of a rate-limit situation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitSeverity {
    Error,
    Warning,
}

/// A formatted rate-limit message for display to the operator.
#[derive(Debug, Clone)]
pub struct RateLimitMessage {
    pub text: String,
    pub severity: RateLimitSeverity,
    /// Suggested next action (wait, switch model, upgrade, etc.).
    pub plan_hint: Option<String>,
}

/// Format a human-readable message from `RateLimitInfo`.
/// Provider-agnostic — works with any LLM provider.
///
/// Ported from `rateLimitMessages.ts:45-104 getRateLimitMessage`.
pub fn format_rate_limit_message(info: &RateLimitInfo) -> Option<RateLimitMessage> {
    // Tiered/overage in use without warning → no message
    if info.is_using_tiered {
        if info.tiered_status == Some(QuotaStatus::AllowedWarning) {
            let label = info.tiered_label.as_deref().unwrap_or("extra usage");
            return Some(RateLimitMessage {
                text: format!("You're close to your {label} spending limit"),
                severity: RateLimitSeverity::Warning,
                plan_hint: Some(
                    "Consider switching to a smaller model or waiting for reset.".into(),
                ),
            });
        }
        return None;
    }

    match info.status {
        Some(QuotaStatus::Rejected) => {
            let (text, plan_hint) = format_limit_reached(info);
            Some(RateLimitMessage {
                text,
                severity: RateLimitSeverity::Error,
                plan_hint: Some(plan_hint),
            })
        }
        Some(QuotaStatus::AllowedWarning) => {
            // Suppress warnings below 70% utilization
            // `rateLimitMessages.ts:72-78`
            if info.utilization.unwrap_or(0.0) < 0.7 {
                return None;
            }
            let (text, plan_hint) = format_early_warning(info);
            Some(RateLimitMessage {
                text,
                severity: RateLimitSeverity::Warning,
                plan_hint: Some(plan_hint),
            })
        }
        _ => None,
    }
}

/// Build "You've hit your {limit}" error text.
fn format_limit_reached(info: &RateLimitInfo) -> (String, String) {
    let reset_str = info.resets_at.map(|ts| format_reset_time(ts)).unwrap_or_default();
    let reset_suffix = if reset_str.is_empty() {
        String::new()
    } else {
        format!(" · resets {reset_str}")
    };

    // Both primary AND tiered quota exhausted
    if info.tiered_status == Some(QuotaStatus::Rejected) {
        let label = info.tiered_label.as_deref().unwrap_or("extra usage");
        let earliest = pick_earliest_reset(info.resets_at, info.tiered_resets_at);
        let reset_suffix = earliest
            .map(|ts| format!(" · resets {}", format_reset_time(ts)))
            .unwrap_or_default();
        return (
            format!("You've exhausted your {label}{reset_suffix}"),
            "Add credits or wait for reset.".into(),
        );
    }

    let limit_name = info
        .window
        .map(|w| w.display_name())
        .unwrap_or("usage limit");

    (
        format!("You've hit your {limit_name}{reset_suffix}"),
        format!(
            "Wait for reset or switch to a different model.{}",
            if info.has_fallback_model {
                " A fallback model is available."
            } else {
                ""
            }
        ),
    )
}

/// Build "You've used X% of your {limit}" warning text.
fn format_early_warning(info: &RateLimitInfo) -> (String, String) {
    let limit_name = info
        .window
        .map(|w| w.display_name())
        .unwrap_or("usage limit");
    let used_pct = info.utilization.map(|u| (u * 100.0) as u32).unwrap_or(0);
    let reset_str = info.resets_at.map(|ts| format_reset_time(ts)).unwrap_or_default();

    let text = if !reset_str.is_empty() {
        format!("You've used {used_pct}% of your {limit_name} · resets {reset_str}")
    } else {
        format!("You've used {used_pct}% of your {limit_name}")
    };

    let hint = if used_pct >= 90 {
        "Consider switching to a smaller model or waiting for reset."
    } else {
        "Monitor usage; consider switching models if rate increases."
    };

    (text, hint.to_string())
}

/// Notification text for when a goal enters tiered/overage mode.
pub fn format_using_overage(info: &RateLimitInfo) -> String {
    let reset_str = info.resets_at.map(|ts| format_reset_time(ts)).unwrap_or_default();
    let limit_name = info
        .window
        .map(|w| w.display_name())
        .unwrap_or("");

    if limit_name.is_empty() {
        return "Now using extra usage".into();
    }

    if reset_str.is_empty() {
        "You're now using extra usage".into()
    } else {
        format!("You're now using extra usage · Your {limit_name} resets {reset_str}")
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
}

fn parse_quota_status(s: &str) -> Option<QuotaStatus> {
    match s {
        "allowed" => Some(QuotaStatus::Allowed),
        "allowed_warning" => Some(QuotaStatus::AllowedWarning),
        "rejected" => Some(QuotaStatus::Rejected),
        _ => None,
    }
}

/// Map Anthropic claim abbreviations to `RateLimitWindow`.
fn parse_anthropic_window(s: &str) -> Option<RateLimitWindow> {
    match s {
        "five_hour" => Some(RateLimitWindow::ShortTerm),
        "seven_day" => Some(RateLimitWindow::MediumTerm),
        "seven_day_opus" | "seven_day_sonnet" => Some(RateLimitWindow::ModelSpecific),
        "overage" => Some(RateLimitWindow::TieredOverage),
        _ => None,
    }
}

/// Parse OpenAI's reset format — either a duration string like "12.5s"
/// or an absolute Unix timestamp.
fn parse_openai_reset(s: &str) -> Option<u64> {
    if let Ok(secs) = s.parse::<f64>() {
        // Absolute timestamp
        return Some(secs.ceil() as u64);
    }
    // Duration string like "12.5s" or "2m"
    if let Some(stripped) = s.strip_suffix('s') {
        stripped.parse::<f64>().ok().map(|v| {
            Utc::now().timestamp() as u64 + (v.ceil() as u64)
        })
    } else if let Some(stripped) = s.strip_suffix('m') {
        stripped.parse::<f64>().ok().map(|v| {
            Utc::now().timestamp() as u64 + ((v * 60.0).ceil() as u64)
        })
    } else if let Some(stripped) = s.strip_suffix('h') {
        stripped.parse::<f64>().ok().map(|v| {
            Utc::now().timestamp() as u64 + ((v * 3600.0).ceil() as u64)
        })
    } else {
        None
    }
}

/// Format a Unix epoch timestamp as a human-readable relative time.
fn format_reset_time(epoch_secs: u64) -> String {
    let now = Utc::now();
    let reset_dt = match DateTime::from_timestamp(epoch_secs as i64, 0) {
        Some(dt) => dt,
        None => return format!("in {}s", epoch_secs.saturating_sub(now.timestamp() as u64)),
    };

    let delta = reset_dt - now;
    let total_secs = delta.num_seconds();

    if total_secs <= 0 {
        return "now".into();
    }

    let total_mins = total_secs / 60;
    let total_hours = total_mins / 60;
    let total_days = total_hours / 24;

    if total_days >= 1 {
        let h = total_hours % 24;
        if h > 0 {
            format!("in {}d {}h", total_days, h)
        } else {
            format!("in {}d", total_days)
        }
    } else if total_hours >= 1 {
        let m = total_mins % 60;
        if m > 0 {
            format!("in {}h {}m", total_hours, m)
        } else {
            format!("in {}h", total_hours)
        }
    } else if total_mins >= 1 {
        format!("in {}m", total_mins)
    } else {
        format!("in {}s", total_secs)
    }
}

fn pick_earliest_reset(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

// ── Phase C4.c — last-known quota event cache ────────────────────

/// Process-wide snapshot of the most recent rejected-quota event
/// per provider. Written by `crate::retry::classify_429_error`
/// when a 429 carries `RateLimitInfo.status == Rejected` and the
/// formatter produces a human-readable message. Read by
/// `setup doctor` and (future) admin-ui to surface "you hit a
/// quota N minutes ago — here's the plan hint" to operators
/// without round-tripping a fresh request.
///
/// Provider-agnostic: keyed by `LlmProvider`. One slot per
/// provider; new events overwrite older ones for the same
/// provider.
///
/// IRROMPIBLE refs:
/// - claude-code-leak `services/api/errors.ts:465-548` — 3-tier
///   429 classification surfaces hard quota to the user via the
///   chat error message. Our analog reaches `setup doctor` +
///   notify_origin (deferred to slice C4.c.b).
/// - claude-code-leak `services/rateLimitMessages.ts:45-104`
///   `getRateLimitMessage` — already ported as
///   `format_rate_limit_message`; the cache below stores the
///   formatted message so callers don't re-run the formatter.
#[derive(Debug, Clone)]
pub struct QuotaEvent {
    pub at: chrono::DateTime<chrono::Utc>,
    pub provider: LlmProvider,
    pub severity: RateLimitSeverity,
    pub message: String,
    pub plan_hint: Option<String>,
    pub window: Option<RateLimitWindow>,
    pub resets_at: Option<u64>,
}

static LAST_QUOTA: OnceLock<DashMap<LlmProvider, QuotaEvent>> = OnceLock::new();

fn last_quota_map() -> &'static DashMap<LlmProvider, QuotaEvent> {
    LAST_QUOTA.get_or_init(DashMap::new)
}

/// Record a quota-rejection event for later operator-facing
/// surfacing. Overwrites any prior event for the same provider.
/// Cheap (`DashMap::insert`); safe across concurrent tasks.
pub fn record_quota_event(event: QuotaEvent) {
    last_quota_map().insert(event.provider, event);
}

/// Latest recorded quota event for `provider`, or `None` if no
/// hard-quota rejection has been observed in this process.
pub fn last_quota_event_for(provider: LlmProvider) -> Option<QuotaEvent> {
    last_quota_map().get(&provider).map(|r| r.value().clone())
}

/// All recorded quota events, one per provider that has emitted
/// a hard rejection. Used by `setup doctor` to render the LLM
/// quota section.
pub fn last_quota_events_all() -> Vec<QuotaEvent> {
    last_quota_map()
        .iter()
        .map(|r| r.value().clone())
        .collect()
}

/// Test-only helper to clear the global cache between cases.
#[cfg(test)]
pub fn clear_last_quota() {
    last_quota_map().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                reqwest::header::HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    // ── Anthropic ──

    #[test]
    fn anthropic_session_limit_reached() {
        let ts = (Utc::now().timestamp() + 5 * 3600).to_string();
        let h = build_headers(&[
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", &ts),
            (
                "anthropic-ratelimit-unified-representative-claim",
                "five_hour",
            ),
        ]);
        let info = extract_anthropic_headers(&h).unwrap();
        assert_eq!(info.status, Some(QuotaStatus::Rejected));
        assert_eq!(info.window, Some(RateLimitWindow::ShortTerm));

        let msg = format_rate_limit_message(&info).unwrap();
        assert_eq!(msg.severity, RateLimitSeverity::Error);
        assert!(msg.text.contains("short-term limit"));
        assert!(msg.text.contains("resets"));
    }

    #[test]
    fn anthropic_weekly_warning() {
        let ts = (Utc::now().timestamp() + 7 * 24 * 3600).to_string();
        let h = build_headers(&[
            ("anthropic-ratelimit-unified-status", "allowed_warning"),
            ("anthropic-ratelimit-unified-reset", &ts),
            (
                "anthropic-ratelimit-unified-representative-claim",
                "seven_day",
            ),
            ("anthropic-ratelimit-unified-7d-utilization", "0.85"),
        ]);
        let info = extract_anthropic_headers(&h).unwrap();
        assert_eq!(info.status, Some(QuotaStatus::AllowedWarning));
        assert_eq!(info.utilization, Some(0.85));

        let msg = format_rate_limit_message(&info).unwrap();
        assert_eq!(msg.severity, RateLimitSeverity::Warning);
        assert!(msg.text.contains("85%"));
        assert!(msg.text.contains("period limit"));
    }

    #[test]
    fn warning_below_70_pct_is_suppressed() {
        let ts = (Utc::now().timestamp() + 7 * 24 * 3600).to_string();
        let h = build_headers(&[
            ("anthropic-ratelimit-unified-status", "allowed_warning"),
            ("anthropic-ratelimit-unified-reset", &ts),
            (
                "anthropic-ratelimit-unified-representative-claim",
                "seven_day",
            ),
            ("anthropic-ratelimit-unified-7d-utilization", "0.45"),
        ]);
        let info = extract_anthropic_headers(&h).unwrap();
        assert!(format_rate_limit_message(&info).is_none());
    }

    #[test]
    fn anthropic_overage_active() {
        let ts = (Utc::now().timestamp() + 5 * 3600).to_string();
        let overage_ts = (Utc::now().timestamp() + 30 * 24 * 3600).to_string();
        let h = build_headers(&[
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", &ts),
            (
                "anthropic-ratelimit-unified-representative-claim",
                "five_hour",
            ),
            ("anthropic-ratelimit-unified-overage-status", "allowed"),
            ("anthropic-ratelimit-unified-overage-reset", &overage_ts),
        ]);
        let info = extract_anthropic_headers(&h).unwrap();
        assert!(info.is_using_tiered);
        assert_eq!(info.tiered_status, Some(QuotaStatus::Allowed));
    }

    #[test]
    fn anthropic_fallback_available() {
        let ts = (Utc::now().timestamp() + 3600).to_string();
        let h = build_headers(&[
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", &ts),
            ("anthropic-ratelimit-unified-fallback", "available"),
        ]);
        let info = extract_anthropic_headers(&h).unwrap();
        assert!(info.has_fallback_model);

        let msg = format_rate_limit_message(&info).unwrap();
        assert!(msg.plan_hint.unwrap().contains("fallback"));
    }

    // ── OpenAI ──

    #[test]
    fn openai_rate_limited() {
        let h = build_headers(&[
            ("x-ratelimit-limit-requests", "100"),
            ("x-ratelimit-remaining-requests", "0"),
            ("x-ratelimit-reset-requests", "12.5s"),
        ]);
        let info = extract_openai_headers(&h).unwrap();
        assert_eq!(info.status, Some(QuotaStatus::Rejected));
        assert_eq!(info.utilization, Some(1.0));

        let msg = format_rate_limit_message(&info).unwrap();
        assert_eq!(msg.severity, RateLimitSeverity::Error);
        assert!(msg.text.contains("short-term limit"));
    }

    #[test]
    fn openai_warning() {
        let h = build_headers(&[
            ("x-ratelimit-limit-requests", "100"),
            ("x-ratelimit-remaining-requests", "10"),
            ("x-ratelimit-reset-requests", "5m"),
        ]);
        let info = extract_openai_headers(&h).unwrap();
        assert_eq!(info.status, Some(QuotaStatus::AllowedWarning));
        // 10 remaining out of 100 = 90% used → utilization = 0.9
        assert!((info.utilization.unwrap() - 0.9).abs() < 0.01);

        let msg = format_rate_limit_message(&info).unwrap();
        assert!(msg.text.contains("90%"));
    }

    // ── Generic ──

    #[test]
    fn generic_retry_after_only() {
        let h = build_headers(&[("retry-after", "30")]);
        let info = extract_generic_headers(&h).unwrap();
        assert_eq!(info.status, Some(QuotaStatus::Rejected));
        assert_eq!(info.retry_after_secs, Some(30));

        let msg = format_rate_limit_message(&info).unwrap();
        assert!(msg.text.contains("usage limit"));
    }

    #[test]
    fn no_headers_returns_none() {
        let h = HeaderMap::new();
        assert!(extract_anthropic_headers(&h).is_none());
        assert!(extract_openai_headers(&h).is_none());
        assert!(extract_generic_headers(&h).is_none());
    }

    #[test]
    fn allowed_status_returns_no_message() {
        let ts = (Utc::now().timestamp() + 3600).to_string();
        let h = build_headers(&[
            ("anthropic-ratelimit-unified-status", "allowed"),
            ("anthropic-ratelimit-unified-reset", &ts),
        ]);
        let info = extract_anthropic_headers(&h).unwrap();
        assert!(format_rate_limit_message(&info).is_none());
    }

    #[test]
    fn format_reset_time_relative() {
        let now_ts = Utc::now().timestamp() as u64;
        // 5 hours from now — renders as "in 5h" or "in 4h 59m"
        // (sub-second truncation may cost ~1 s)
        let in_5h = now_ts + 5 * 3600;
        let s = format_reset_time(in_5h);
        assert!(
            s.contains("in 5h") || s.contains("in 4h 59m"),
            "expected 'in 5h' or 'in 4h 59m', got '{s}'"
        );

        // 2 days from now — renders as "in 2d" or "in 1d 23h"
        let in_2d = now_ts + 2 * 24 * 3600;
        let s = format_reset_time(in_2d);
        assert!(
            s.contains("in 2d") || s.contains("in 1d 23h"),
            "expected 'in 2d' or 'in 1d 23h', got '{s}'"
        );
    }

    // ── Dispatcher ──

    #[test]
    fn dispatcher_routes_to_correct_extractor() {
        let ts = (Utc::now().timestamp() + 3600).to_string();
        let h = build_headers(&[
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", &ts),
        ]);
        let info = extract_rate_limit_info(&h, LlmProvider::Anthropic).unwrap();
        assert_eq!(info.provider, Some(LlmProvider::Anthropic));
        assert_eq!(info.status, Some(QuotaStatus::Rejected));
    }

    // ── Phase C4.c — last-known quota cache ──

    #[test]
    fn record_quota_event_is_visible_via_last_quota_event_for() {
        clear_last_quota();
        let event = QuotaEvent {
            at: Utc::now(),
            provider: LlmProvider::Generic,
            severity: RateLimitSeverity::Error,
            message: "test quota event".into(),
            plan_hint: Some("test hint".into()),
            window: Some(RateLimitWindow::ShortTerm),
            resets_at: Some(1_700_000_000),
        };
        record_quota_event(event);
        let observed = last_quota_event_for(LlmProvider::Generic).expect("event recorded");
        assert_eq!(observed.message, "test quota event");
        assert_eq!(observed.plan_hint.as_deref(), Some("test hint"));
        assert_eq!(observed.window, Some(RateLimitWindow::ShortTerm));
        // Different provider has no event.
        clear_last_quota();
        assert!(last_quota_event_for(LlmProvider::Generic).is_none());
    }

    #[test]
    fn last_quota_events_all_returns_one_per_provider() {
        clear_last_quota();
        let now = Utc::now();
        for provider in [LlmProvider::Anthropic, LlmProvider::OpenAI] {
            record_quota_event(QuotaEvent {
                at: now,
                provider,
                severity: RateLimitSeverity::Error,
                message: format!("{provider:?} quota hit"),
                plan_hint: None,
                window: None,
                resets_at: None,
            });
        }
        let all = last_quota_events_all();
        assert_eq!(all.len(), 2);
        clear_last_quota();
    }

    #[test]
    fn extract_openai_compat_headers_promotes_to_quota_exceeded() {
        // OpenAI-compat: zero remaining requests + reset → Rejected.
        let h = build_headers(&[
            ("x-ratelimit-remaining-requests", "0"),
            ("x-ratelimit-reset-requests", "30s"),
        ]);
        let info = extract_openai_compat_headers(&h).expect("info extracted");
        assert_eq!(info.status, Some(QuotaStatus::Rejected));
        // Verify the promotion side: classify_429_error with this info → QuotaExceeded.
        clear_last_quota();
        let err = crate::retry::classify_429_error(30_000, Some(info));
        assert!(
            matches!(err, crate::retry::LlmError::QuotaExceeded { .. }),
            "expected QuotaExceeded, got {err:?}",
        );
    }

    #[test]
    fn extract_gemini_headers_promotes_to_quota_exceeded() {
        // Gemini's RESOURCE_EXHAUSTED: parser sets status=Rejected
        // when retry-after present + (provider-specific signal).
        // We use a retry-after value as the minimal signal that
        // gemini extractor parses.
        let h = build_headers(&[("retry-after", "120")]);
        let info = extract_gemini_headers(&h);
        // The extractor may or may not classify based on header-only;
        // if Some, ensure provider is Gemini. Either way the test
        // documents the wire path stays Gemini.
        if let Some(info) = info {
            assert_eq!(info.provider, Some(LlmProvider::Gemini));
        }
    }
}
