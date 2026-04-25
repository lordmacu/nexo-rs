//! Tavily provider — POST `https://api.tavily.com/search`, JSON body.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::WebSearchProvider;
use crate::sanitise::sanitise_for_prompt;
use crate::types::{Freshness, WebSearchArgs, WebSearchError, WebSearchHit};

const ENDPOINT: &str = "https://api.tavily.com/search";

pub struct TavilyProvider {
    api_key: String,
    http: reqwest::Client,
    endpoint: String,
}

impl TavilyProvider {
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

    #[doc(hidden)]
    pub fn with_endpoint(api_key: String, timeout_ms: u64, endpoint: String) -> Self {
        let mut p = Self::new(api_key, timeout_ms);
        p.endpoint = endpoint;
        p
    }
}

fn time_range(f: Freshness) -> &'static str {
    match f {
        Freshness::Day => "day",
        Freshness::Week => "week",
        Freshness::Month => "month",
        Freshness::Year => "year",
    }
}

#[derive(Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
}
#[derive(Deserialize)]
struct TavilyResult {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    published_date: Option<String>,
}

#[async_trait]
impl WebSearchProvider for TavilyProvider {
    fn id(&self) -> &'static str {
        "tavily"
    }
    fn requires_credential(&self) -> bool {
        true
    }
    async fn search(&self, args: &WebSearchArgs) -> Result<Vec<WebSearchHit>, WebSearchError> {
        if args.query.trim().is_empty() {
            return Err(WebSearchError::InvalidArg("query is empty"));
        }
        let count = args.effective_count(5);
        let mut payload = json!({
            "api_key": self.api_key,
            "query": args.query,
            "max_results": count,
        });
        if let Some(f) = args.freshness {
            payload["time_range"] = json!(time_range(f));
        }
        let resp = self
            .http
            .post(&self.endpoint)
            .json(&payload)
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
        let body: TavilyResponse = resp
            .json()
            .await
            .map_err(|e| WebSearchError::Transport(e.to_string()))?;
        Ok(body
            .results
            .into_iter()
            .map(|r| WebSearchHit {
                site_name: super::brave_host(&r.url),
                url: sanitise_for_prompt(&r.url, 2 * 1024),
                title: sanitise_for_prompt(&r.title, 512),
                snippet: sanitise_for_prompt(&r.content, 4 * 1024),
                published_at: r.published_date,
                body: None,
            })
            .collect())
    }
}
