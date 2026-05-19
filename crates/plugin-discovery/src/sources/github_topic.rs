//! GitHub topic search source.
//!
//! `GET <endpoint>/search/repositories?q=topic:nexo-plugin&per_page=100`.
//! Unauthenticated by default (10 req/min ceiling); admin op can
//! pass `DiscoveryConfig.github_token` for the 5000/hr authenticated
//! ceiling. Rate-limit / 5xx responses surface as `SourceError` so
//! the runner aggregates partial failures cleanly.

use std::time::Duration;

use serde::Deserialize;

use crate::sources::{source_error, Source};
use crate::types::{
    CompatStatus, DiscoveredPlugin, PluginCategory, PluginSource, SourceError, TrustTier,
};
use nexo_tool_meta::admin::plugin_install::{InstallSource, PluginsInstallParams};

use async_trait::async_trait;

/// Source name for `partial_failures` + telemetry.
pub const SOURCE_NAME: &str = "github_topic";

/// Topic queried. Plugin authors opt in by adding the topic to
/// their repo on GitHub.
const TOPIC: &str = "nexo-plugin";

/// GitHub topic search source.
pub struct GithubTopicSource {
    http: reqwest::Client,
    endpoint: String,
}

impl GithubTopicSource {
    /// Build a fresh source. `token` lifts the rate ceiling; `None`
    /// = unauth.
    pub fn new(endpoint: impl Into<String>, http_timeout: Duration, token: Option<String>) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
        );
        if let Some(tok) = token.as_deref() {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {tok}")) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        }
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "nexo-plugin-discovery/{} (+https://github.com/lordmacu/nexo-rs)",
                env!("CARGO_PKG_VERSION")
            ))
            .default_headers(headers)
            .timeout(http_timeout)
            .build()
            .expect("reqwest client build (rustls) failed");
        Self {
            http,
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl Source for GithubTopicSource {
    fn name(&self) -> &'static str {
        SOURCE_NAME
    }

    async fn fetch(&self) -> Result<Vec<DiscoveredPlugin>, SourceError> {
        let url = format!(
            "{}/search/repositories?q=topic:{}&per_page=100",
            self.endpoint.trim_end_matches('/'),
            TOPIC
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| source_error(SOURCE_NAME, format!("GET {url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            // GitHub returns 403 + a `X-RateLimit-Remaining: 0`
            // header when the unauth quota is exhausted. We surface
            // a friendlier message so the operator banner makes
            // sense without forcing them to read curl docs.
            let message = if status == reqwest::StatusCode::FORBIDDEN {
                "rate-limited (set GITHUB_TOKEN env to lift the unauth ceiling)".to_string()
            } else {
                format!("status {status}")
            };
            return Err(source_error(SOURCE_NAME, message));
        }
        let parsed: GithubSearchResponse = resp
            .json()
            .await
            .map_err(|e| source_error(SOURCE_NAME, format!("parse {url}: {e}")))?;
        Ok(parsed.items.into_iter().filter_map(map_repo).collect())
    }
}

// ── wire shapes (private) ────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GithubSearchResponse {
    #[serde(default)]
    items: Vec<GithubRepo>,
}

#[derive(Debug, Deserialize)]
struct GithubRepo {
    full_name: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    topics: Option<Vec<String>>,
    owner: GithubOwner,
}

#[derive(Debug, Deserialize)]
struct GithubOwner {
    login: String,
}

fn map_repo(raw: GithubRepo) -> Option<DiscoveredPlugin> {
    let GithubRepo {
        full_name,
        name,
        description,
        homepage,
        html_url,
        default_branch,
        topics,
        owner,
    } = raw;
    if !full_name.contains('/') {
        return None;
    }
    // Heuristic crate name: `nexo-plugin-X` (or whatever the repo
    // says) — most extracted plugins follow `nexo-rs-plugin-X` for
    // the repo and `nexo-plugin-X` for the crate. We assume the
    // repo's `name` (not full_name) maps to the crate; if the
    // operator's convention differs the manifest fetch in 98.8
    // overrides via the `[plugin.cargo].crate_name` field once
    // that's wired.
    let crate_name = derive_crate_name(&name);
    let branch = default_branch.as_deref().unwrap_or("main");
    let manifest_url =
        format!("https://raw.githubusercontent.com/{full_name}/{branch}/nexo-plugin.toml");
    let install_params = PluginsInstallParams {
        crate_name: crate_name.clone(),
        version: None,
        repo: Some(full_name.clone()),
        source: InstallSource::Release,
        force: false,
        require_signature: false,
        skip_signature_verify: false,
    };
    let install_cmd = format!("cargo install {crate_name}");
    Some(DiscoveredPlugin {
        name: crate_name,
        version: None,
        description,
        owner: owner.login,
        sources: vec![PluginSource::GithubTopic {
            repo: full_name.clone(),
        }],
        repo_url: html_url,
        homepage,
        tags: topics.unwrap_or_default(),
        category: PluginCategory::Unknown,
        trust_tier: TrustTier::Unverified,
        compat: CompatStatus::Unknown,
        manifest_url: Some(manifest_url),
        install_cmd,
        install_params,
    })
}

/// Map a GitHub repo name to the assumed crate name.
///   - `nexo-rs-plugin-telegram` → `nexo-plugin-telegram`
///   - `nexo-plugin-X` → unchanged
///   - anything else → unchanged (manifest fetch can override).
fn derive_crate_name(repo_name: &str) -> String {
    if let Some(rest) = repo_name.strip_prefix("nexo-rs-plugin-") {
        return format!("nexo-plugin-{rest}");
    }
    if let Some(rest) = repo_name.strip_prefix("nexo-rs-poller-") {
        return format!("nexo-poller-{rest}");
    }
    repo_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn happy_body() -> serde_json::Value {
        serde_json::json!({
            "items": [
                {
                    "full_name": "lordmacu/nexo-rs-plugin-telegram",
                    "name": "nexo-rs-plugin-telegram",
                    "description": "Telegram bot channel plugin",
                    "html_url": "https://github.com/lordmacu/nexo-rs-plugin-telegram",
                    "homepage": null,
                    "default_branch": "main",
                    "topics": ["nexo-plugin", "telegram", "messaging"],
                    "owner": { "login": "lordmacu" }
                },
                {
                    "full_name": "someone/extra-plugin",
                    "name": "extra-plugin",
                    "description": "Community plugin",
                    "html_url": "https://github.com/someone/extra-plugin",
                    "homepage": null,
                    "default_branch": "master",
                    "topics": ["nexo-plugin"],
                    "owner": { "login": "someone" }
                }
            ]
        })
    }

    #[tokio::test]
    async fn happy_path_maps_two_repos() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(happy_body()))
            .mount(&server)
            .await;
        let src = GithubTopicSource::new(server.uri(), Duration::from_secs(5), None);
        let items = src.fetch().await.expect("fetch ok");
        assert_eq!(items.len(), 2);
        let tele = items
            .iter()
            .find(|p| p.name == "nexo-plugin-telegram")
            .expect("telegram name derived from nexo-rs-plugin-…");
        assert_eq!(tele.owner, "lordmacu");
        assert_eq!(
            tele.manifest_url.as_deref(),
            Some(
                "https://raw.githubusercontent.com/lordmacu/nexo-rs-plugin-telegram/main/nexo-plugin.toml"
            )
        );
        // GithubTopic source uses the `full_name` repo on the
        // source variant.
        match &tele.sources[0] {
            PluginSource::GithubTopic { repo } => {
                assert_eq!(repo, "lordmacu/nexo-rs-plugin-telegram");
            }
            other => panic!("expected GithubTopic, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rate_limit_surfaces_with_helpful_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let src = GithubTopicSource::new(server.uri(), Duration::from_secs(5), None);
        let err = src.fetch().await.expect_err("403 must surface");
        assert_eq!(err.source, SOURCE_NAME);
        assert!(
            err.message.contains("rate-limited") && err.message.contains("GITHUB_TOKEN"),
            "expected friendly rate-limit hint, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn empty_items_yields_empty_vec() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "items": [] })),
            )
            .mount(&server)
            .await;
        let src = GithubTopicSource::new(server.uri(), Duration::from_secs(5), None);
        let items = src.fetch().await.expect("ok");
        assert!(items.is_empty());
    }

    #[test]
    fn derive_crate_name_strips_nexo_rs_prefix() {
        assert_eq!(
            derive_crate_name("nexo-rs-plugin-telegram"),
            "nexo-plugin-telegram"
        );
        assert_eq!(derive_crate_name("nexo-rs-poller-rss"), "nexo-poller-rss");
        assert_eq!(
            derive_crate_name("nexo-plugin-already"),
            "nexo-plugin-already"
        );
        assert_eq!(derive_crate_name("foo"), "foo");
    }
}
