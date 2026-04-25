//! DuckDuckGo HTML scraper. No API key. The free fallback.
//!
//! Bot challenges are a real failure mode (DDG flips a captcha when it
//! sees suspicious traffic patterns). Detect them and surface as 429
//! so the breaker opens and the router rotates to another provider.

use std::time::Duration;

use async_trait::async_trait;
use scraper::{Html, Selector};

use crate::provider::WebSearchProvider;
use crate::sanitise::sanitise_for_prompt;
use crate::types::{WebSearchArgs, WebSearchError, WebSearchHit};

const ENDPOINT: &str = "https://html.duckduckgo.com/html";

pub struct DuckDuckGoProvider {
    http: reqwest::Client,
    endpoint: String,
}

impl DuckDuckGoProvider {
    pub fn new(timeout_ms: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("Mozilla/5.0 (compatible; nexo-web-search/0.1)")
            .build()
            .expect("reqwest build");
        Self {
            http,
            endpoint: ENDPOINT.to_string(),
        }
    }

    #[doc(hidden)]
    pub fn with_endpoint(timeout_ms: u64, endpoint: String) -> Self {
        let mut p = Self::new(timeout_ms);
        p.endpoint = endpoint;
        p
    }

    /// Public for tests: parse an HTML body lifted from
    /// `html.duckduckgo.com` into hits.
    pub fn parse_html(html: &str, max_hits: usize) -> Result<Vec<WebSearchHit>, WebSearchError> {
        if is_bot_challenge(html) {
            return Err(WebSearchError::ProviderHttp {
                provider: "duckduckgo".into(),
                status: 429,
            });
        }
        let doc = Html::parse_document(html);
        let result_sel = Selector::parse(".result").unwrap();
        let title_sel = Selector::parse(".result__title a").unwrap();
        let snippet_sel = Selector::parse(".result__snippet").unwrap();

        let mut hits = Vec::new();
        for el in doc.select(&result_sel) {
            if hits.len() >= max_hits {
                break;
            }
            let Some(title_el) = el.select(&title_sel).next() else {
                continue;
            };
            let raw_url = title_el.value().attr("href").unwrap_or("").to_string();
            let url = decode_ddg_url(&raw_url);
            if url.is_empty() {
                continue;
            }
            let title: String = title_el.text().collect();
            let snippet: String = el
                .select(&snippet_sel)
                .next()
                .map(|s| s.text().collect())
                .unwrap_or_default();
            hits.push(WebSearchHit {
                site_name: super::brave_host(&url),
                url: sanitise_for_prompt(&url, 2 * 1024),
                title: sanitise_for_prompt(&title, 512),
                snippet: sanitise_for_prompt(&snippet, 4 * 1024),
                published_at: None,
                body: None,
            });
        }
        Ok(hits)
    }
}

fn is_bot_challenge(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("anomaly")
        || lower.contains("captcha")
        || (lower.contains("blocked") && lower.contains("requests"))
}

fn decode_ddg_url(raw: &str) -> String {
    // DDG wraps target URLs in `/l/?uddg=<encoded>`. Pull out `uddg`
    // when present, otherwise pass through unchanged.
    let normalised = if let Some(rest) = raw.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        raw.to_string()
    };
    if let Some(qs) = normalised.split_once('?').map(|(_, q)| q) {
        for pair in qs.split('&') {
            if let Some(v) = pair.strip_prefix("uddg=") {
                return percent_decode(v);
            }
        }
    }
    normalised
}

fn percent_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[async_trait]
impl WebSearchProvider for DuckDuckGoProvider {
    fn id(&self) -> &'static str {
        "duckduckgo"
    }
    fn requires_credential(&self) -> bool {
        false
    }
    async fn search(&self, args: &WebSearchArgs) -> Result<Vec<WebSearchHit>, WebSearchError> {
        if args.query.trim().is_empty() {
            return Err(WebSearchError::InvalidArg("query is empty"));
        }
        let count = args.effective_count(5) as usize;
        let resp = self
            .http
            .get(&self.endpoint)
            .query(&[("q", args.query.as_str())])
            .send()
            .await
            .map_err(|e| WebSearchError::Transport(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(WebSearchError::ProviderHttp {
                provider: self.id().to_string(),
                status: status.as_u16(),
            });
        }
        let html = resp
            .text()
            .await
            .map_err(|e| WebSearchError::Transport(e.to_string()))?;
        Self::parse_html(&html, count)
    }
}
