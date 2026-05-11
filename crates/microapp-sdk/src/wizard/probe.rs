//! Server-side LLM key validation.
//!
//! Wizards collect a provider API key in the browser. Probing the
//! key server-side via `GET {base_url}/models` (with
//! `Authorization: Bearer <key>`) gives three properties the
//! browser-side path can't:
//!
//! - The raw key never lands in the browser's network panel /
//!   DevTools (the response stays inside Rust; only
//!   `ok / status / latency / model_count` ship back).
//! - CORS surprises don't gate the wizard on the LLM provider's
//!   browser policy.
//! - The key never appears in tracing — `Debug` is redacted on
//!   [`ProbeRequest`] and any echo of the key in error bodies is
//!   replaced with `<redacted>` before the error string leaves
//!   Rust.
//!
//! Lifted verbatim from `agent-creator-microapp::onboarding::llm_probe`
//! so any microapp wiring its own first-run flow reuses the same
//! redaction + url-builder semantics.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default per-probe timeout. The wizard UI considers >5 s a
/// failure regardless of HTTP status — most key-validation calls
/// land in <500 ms, slow networks within 2 s.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Wire shape consumed by `POST /api/onboarding/llm/probe` (or
/// equivalent microapp-defined route). `api_key` is
/// `Deserialize`-only — never serialise this struct back out.
/// The manual `Debug` redacts the key.
#[derive(Deserialize)]
pub struct ProbeRequest {
    /// Provider id slug (`"minimax"`, `"openai"`, …). Carried
    /// through to logs / responses so the operator can correlate;
    /// the prober itself doesn't branch on it.
    pub provider_id: String,
    /// Base URL up to (but not including) `/models`. Trailing
    /// slash tolerated.
    pub base_url: String,
    /// Plaintext API key. Never logged. Never echoed.
    pub api_key: String,
    /// Optional model slug the wizard wants to surface — kept for
    /// forward compat (operators may want a per-model probe in
    /// future). Currently unused by [`probe`].
    #[serde(default)]
    pub model_hint: Option<String>,
}

impl std::fmt::Debug for ProbeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeRequest")
            .field("provider_id", &self.provider_id)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model_hint", &self.model_hint)
            .finish()
    }
}

/// Wire shape returned by [`probe`]. The browser uses `ok` to
/// gate the "Save and continue" button; `error` is shown inline
/// when `ok == false`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProbeResult {
    /// `true` iff HTTP status was in `200..300`.
    pub ok: bool,
    /// HTTP status code returned by the provider, or `0` when the
    /// request failed before producing one (DNS, timeout,
    /// connect refused).
    pub status: u16,
    /// Round-trip wall-clock duration, milliseconds.
    pub latency_ms: u64,
    /// Number of entries in the response's `data` array, when the
    /// body was valid JSON with the OpenAI/MiniMax shape. `None`
    /// is non-fatal — the probe still reports `ok: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_count: Option<usize>,
    /// Operator-facing error string. Always key-redacted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Run a probe. Single GET, no retry. The reqwest client is
/// borrowed (caller-owned) so the same connection pool is reused
/// across probes. Errors are sanitised: any substring of the API
/// key in the response body or the reqwest error message is
/// replaced with `<redacted>`.
///
/// `timeout` defaults to [`DEFAULT_PROBE_TIMEOUT`] — pass
/// `Some(...)` only when the caller has a tighter / looser
/// requirement.
pub async fn probe(
    req: &ProbeRequest,
    http: &reqwest::Client,
    timeout: Option<Duration>,
) -> ProbeResult {
    let url = build_models_url(&req.base_url);
    let start = Instant::now();
    let response = http
        .get(&url)
        .bearer_auth(&req.api_key)
        .timeout(timeout.unwrap_or(DEFAULT_PROBE_TIMEOUT))
        .send()
        .await;

    match response {
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.bytes().await.unwrap_or_default();
            let latency_ms = start.elapsed().as_millis() as u64;
            let ok = (200..300).contains(&status);
            if ok {
                let model_count = parse_model_count(&body);
                ProbeResult {
                    ok: true,
                    status,
                    latency_ms,
                    model_count,
                    error: None,
                }
            } else {
                let raw_text = String::from_utf8_lossy(&body).into_owned();
                let trimmed = raw_text.chars().take(400).collect::<String>();
                let safe = redact_key(&trimmed, &req.api_key);
                ProbeResult {
                    ok: false,
                    status,
                    latency_ms,
                    model_count: None,
                    error: Some(format!("HTTP {status}: {safe}")),
                }
            }
        }
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            let raw = e.to_string();
            let safe = redact_key(&raw, &req.api_key);
            ProbeResult {
                ok: false,
                status: 0,
                latency_ms,
                model_count: None,
                error: Some(safe),
            }
        }
    }
}

/// Append `/models` to `base_url`, tolerating a trailing slash
/// (`https://x.com/v1` and `https://x.com/v1/` both yield
/// `https://x.com/v1/models`). MiniMax + OpenAI-compatible
/// providers all expose this path.
fn build_models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    format!("{trimmed}/models")
}

/// Parse `{ "data": [...] }` (OpenAI / MiniMax shape) and return
/// the array length. Returns `None` if the body isn't JSON or
/// `data` isn't an array — non-fatal, the probe still reports
/// `ok: true`.
fn parse_model_count(body: &[u8]) -> Option<usize> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let arr = v.get("data")?.as_array()?;
    Some(arr.len())
}

/// Replace every occurrence of `key` (and its first 8 chars as a
/// fingerprint defence) with `<redacted>`. Cheap: most error
/// strings don't contain the key at all.
fn redact_key(haystack: &str, key: &str) -> String {
    if key.is_empty() {
        return haystack.to_string();
    }
    let mut out = haystack.replace(key, "<redacted>");
    if key.len() > 8 {
        out = out.replace(&key[..8], "<redacted>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with(api_key: &str, base_url: &str) -> ProbeRequest {
        ProbeRequest {
            provider_id: "minimax".into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model_hint: None,
        }
    }

    #[test]
    fn build_models_url_handles_trailing_slash() {
        assert_eq!(
            build_models_url("https://api.minimax.chat/v1"),
            "https://api.minimax.chat/v1/models"
        );
        assert_eq!(
            build_models_url("https://api.minimax.chat/v1/"),
            "https://api.minimax.chat/v1/models"
        );
    }

    #[test]
    fn parse_model_count_reads_data_array_length() {
        let body = br#"{"data":[{"id":"a"},{"id":"b"},{"id":"c"}]}"#;
        assert_eq!(parse_model_count(body), Some(3));
    }

    #[test]
    fn parse_model_count_returns_none_on_unexpected_shape() {
        assert_eq!(parse_model_count(b"not json"), None);
        assert_eq!(parse_model_count(br#"{"models":[]}"#), None);
        assert_eq!(parse_model_count(br#"{"data":"oops"}"#), None);
    }

    #[test]
    fn redact_key_replaces_full_and_prefix() {
        let key = "sk-supersecretkey-1234567890abcdef";
        let body = format!("error: invalid token {key} (origin: foo)");
        let redacted = redact_key(&body, key);
        assert!(!redacted.contains(key));
        assert!(redacted.contains("<redacted>"));

        let prefix_only = format!("token starts with {} which is wrong", &key[..8]);
        let redacted2 = redact_key(&prefix_only, key);
        assert!(!redacted2.contains(&key[..8]));
    }

    #[test]
    fn redact_key_noop_on_empty_key() {
        assert_eq!(redact_key("hello", ""), "hello");
    }

    #[test]
    fn debug_redacts_api_key() {
        let r = req_with("sk-leak-this-not", "https://x/v1");
        let s = format!("{r:?}");
        assert!(s.contains("<redacted>"));
        assert!(!s.contains("sk-leak-this-not"));
    }

    #[tokio::test]
    async fn probe_timeout_returns_error_under_seven_seconds() {
        // Bind a TCP listener that accepts but never responds. The
        // 5 s probe timeout fires; the test asserts we don't wait
        // the full reqwest default (30+ s).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _accept_task = tokio::spawn(async move {
            let _ = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(15)).await;
        });
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_PROBE_TIMEOUT)
            .build()
            .unwrap();
        let req = req_with("sk-test", &format!("http://{addr}/v1"));
        let started = Instant::now();
        let result = probe(&req, &http, None).await;
        let elapsed = started.elapsed();
        assert!(!result.ok, "timeout should not be reported as ok");
        assert!(
            elapsed < Duration::from_secs(7),
            "probe waited too long: {elapsed:?}"
        );
        assert!(result.error.is_some());
    }
}
