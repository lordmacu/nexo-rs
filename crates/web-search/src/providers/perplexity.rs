//! Perplexity Sonar provider — POST `https://api.perplexity.ai/chat/completions`.
//! Hits come from the `citations` field of the assistant's reply.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::WebSearchProvider;
use crate::sanitise::sanitise_for_prompt;
use crate::types::{Freshness, WebSearchArgs, WebSearchError, WebSearchHit};

const ENDPOINT: &str = "https://api.perplexity.ai/chat/completions";

pub struct PerplexityProvider {
    api_key: String,
    model: String,
    http: reqwest::Client,
    endpoint: String,
}

impl PerplexityProvider {
    pub fn new(api_key: String, model: String, timeout_ms: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent("nexo-web-search/0.1")
            .build()
            .expect("reqwest build");
        Self {
            api_key,
            model,
            http,
            endpoint: ENDPOINT.to_string(),
        }
    }

    #[doc(hidden)]
    pub fn with_endpoint(
        api_key: String,
        model: String,
        timeout_ms: u64,
        endpoint: String,
    ) -> Self {
        let mut p = Self::new(api_key, model, timeout_ms);
        p.endpoint = endpoint;
        p
    }
}

fn recency(f: Freshness) -> &'static str {
    match f {
        Freshness::Day => "day",
        Freshness::Week => "week",
        Freshness::Month => "month",
        Freshness::Year => "year",
    }
}

#[derive(Deserialize)]
struct PerplexityResponse {
    #[serde(default)]
    citations: Vec<String>,
    #[serde(default)]
    choices: Vec<PerplexityChoice>,
}
#[derive(Deserialize)]
struct PerplexityChoice {
    #[serde(default)]
    message: Option<PerplexityMessage>,
}
#[derive(Deserialize)]
struct PerplexityMessage {
    #[serde(default)]
    content: String,
}

#[async_trait]
impl WebSearchProvider for PerplexityProvider {
    fn id(&self) -> &'static str {
        "perplexity"
    }
    fn requires_credential(&self) -> bool {
        true
    }
    async fn search(&self, args: &WebSearchArgs) -> Result<Vec<WebSearchHit>, WebSearchError> {
        if args.query.trim().is_empty() {
            return Err(WebSearchError::InvalidArg("query is empty"));
        }
        let mut payload = json!({
            "model": self.model,
            "messages": [
                {"role": "user", "content": args.query}
            ],
            "max_tokens": 1024,
        });
        if let Some(f) = args.freshness {
            payload["search_recency_filter"] = json!(recency(f));
        }
        let resp = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
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
        let body: PerplexityResponse = resp
            .json()
            .await
            .map_err(|e| WebSearchError::Transport(e.to_string()))?;
        let snippet = body
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message)
            .map(|m| m.content)
            .unwrap_or_default();
        let snippet = sanitise_for_prompt(&snippet, 4 * 1024);
        let count = args.effective_count(5) as usize;
        Ok(body
            .citations
            .into_iter()
            .take(count)
            .map(|url| WebSearchHit {
                site_name: super::brave_host(&url),
                title: super::brave_host(&url).unwrap_or_else(|| url.clone()),
                url: sanitise_for_prompt(&url, 2 * 1024),
                snippet: snippet.clone(),
                published_at: None,
                body: None,
            })
            .collect())
    }
}
