//! Brave Search provider.
//!
//! Endpoint: `https://api.search.brave.com/res/v1/web/search`
//! Auth: `X-Subscription-Token: <key>` header.
//! Reference: `research/extensions/brave/src/brave-web-search-provider.shared.ts`.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::WebSearchProvider;
use crate::sanitise::sanitise_for_prompt;
use crate::types::{Freshness, WebSearchArgs, WebSearchError, WebSearchHit};

const ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const SNIPPET_CAP: usize = 4 * 1024;

pub struct BraveProvider {
    api_key: String,
    http: reqwest::Client,
    endpoint: String,
}

impl BraveProvider {
    pub fn new(api_key: String, timeout_ms: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("nexo-web-search/0.1")
            .build()
            .expect("reqwest build");
        Self {
            api_key,
            http,
            endpoint: ENDPOINT.to_string(),
        }
    }

    /// Test-only constructor that overrides the endpoint URL so a fake
    /// HTTP server can stand in for `api.search.brave.com`.
    #[doc(hidden)]
    pub fn with_endpoint(api_key: String, timeout_ms: u64, endpoint: String) -> Self {
        let mut p = Self::new(api_key, timeout_ms);
        p.endpoint = endpoint;
        p
    }
}

fn freshness_param(f: Freshness) -> &'static str {
    match f {
        Freshness::Day => "pd",
        Freshness::Week => "pw",
        Freshness::Month => "pm",
        Freshness::Year => "py",
    }
}

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}
#[derive(Deserialize)]
struct BraveWeb {
    results: Vec<BraveResult>,
}
#[derive(Deserialize)]
struct BraveResult {
    url: String,
    title: String,
    description: String,
    #[serde(default)]
    page_age: Option<String>,
}

#[async_trait]
impl WebSearchProvider for BraveProvider {
    fn id(&self) -> &'static str {
        "brave"
    }
    fn requires_credential(&self) -> bool {
        true
    }

    async fn search(&self, args: &WebSearchArgs) -> Result<Vec<WebSearchHit>, WebSearchError> {
        if args.query.trim().is_empty() {
            return Err(WebSearchError::InvalidArg("query is empty"));
        }
        let count = args.effective_count(5);
        let mut req = self
            .http
            .get(&self.endpoint)
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("q", args.query.as_str()), ("count", &count.to_string())]);
        if let Some(f) = args.freshness {
            req = req.query(&[("freshness", freshness_param(f))]);
        }
        if let Some(c) = &args.country {
            let canonical = c.trim().to_uppercase();
            req = req.query(&[("country", canonical.as_str())]);
        }
        if let Some(l) = &args.language {
            req = req.query(&[("search_lang", l.trim().to_lowercase().as_str())]);
        }
        let resp = req
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
        let body: BraveResponse = resp
            .json()
            .await
            .map_err(|e| WebSearchError::Transport(e.to_string()))?;
        let results = body.web.map(|w| w.results).unwrap_or_default();
        Ok(results
            .into_iter()
            .map(|r| {
                let site_name = super::brave_host(&r.url);
                WebSearchHit {
                    site_name,
                    url: sanitise_for_prompt(&r.url, 2 * 1024),
                    title: sanitise_for_prompt(&r.title, 512),
                    snippet: sanitise_for_prompt(&r.description, SNIPPET_CAP),
                    published_at: r.page_age,
                    body: None,
                }
            })
            .collect())
    }
}
