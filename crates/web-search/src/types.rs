//! Public input/output types.

use serde::{Deserialize, Serialize};

/// JSON-shaped arguments the LLM passes when calling `web_search`.
///
/// Field shape stays stable across providers; each provider translates
/// into its own API. Optional fields default to `None` so the model can
/// emit just `{"query": "..."}` for the common case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchArgs {
    pub query: String,
    #[serde(default)]
    pub count: Option<u8>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub freshness: Option<Freshness>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub expand: bool,
}

impl WebSearchArgs {
    /// Returns the request count, clamped to `[1, 10]`. Drops out-of-range
    /// values silently rather than erroring — the LLM occasionally emits
    /// `count: 25` and the operator's expected behaviour is "give me 10",
    /// not "fail the turn".
    pub fn effective_count(&self, default: u8) -> u8 {
        let raw = self.count.unwrap_or(default);
        raw.clamp(1, 10)
    }
}

/// Time-window filter understood by every provider that exposes one.
/// Providers that don't support a given window (DDG) silently drop it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Freshness {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchHit {
    pub url: String,
    pub title: String,
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchResult {
    pub provider: String,
    pub query: String,
    pub results: Vec<WebSearchHit>,
    #[serde(default)]
    pub from_cache: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum WebSearchError {
    #[error("provider {0} unavailable (breaker open)")]
    ProviderUnavailable(String),
    #[error("no provider configured: no credentials and no fallback")]
    NoProviderConfigured,
    #[error("provider {provider} returned http {status}")]
    ProviderHttp { provider: String, status: u16 },
    #[error("invalid argument: {0}")]
    InvalidArg(&'static str),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("cache error: {0}")]
    Cache(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_round_trip() {
        let v = serde_json::json!({"query": "rust async", "count": 3});
        let parsed: WebSearchArgs = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.query, "rust async");
        assert_eq!(parsed.count, Some(3));
        assert!(!parsed.expand);
    }

    #[test]
    fn count_clamps_to_one_through_ten() {
        let mut a = WebSearchArgs {
            query: "x".into(),
            count: Some(0),
            provider: None,
            freshness: None,
            country: None,
            language: None,
            expand: false,
        };
        assert_eq!(a.effective_count(5), 1);
        a.count = Some(99);
        assert_eq!(a.effective_count(5), 10);
        a.count = None;
        assert_eq!(a.effective_count(5), 5);
    }

    #[test]
    fn freshness_parses_lowercase() {
        let f: Freshness = serde_json::from_str("\"day\"").unwrap();
        assert_eq!(f, Freshness::Day);
    }

    #[test]
    fn hit_omits_optional_fields_when_empty() {
        let h = WebSearchHit {
            url: "https://x".into(),
            title: "T".into(),
            snippet: "S".into(),
            site_name: None,
            published_at: None,
            body: None,
        };
        let s = serde_json::to_string(&h).unwrap();
        assert!(!s.contains("site_name"));
        assert!(!s.contains("body"));
    }
}
