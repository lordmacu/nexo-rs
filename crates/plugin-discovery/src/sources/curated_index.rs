//! Curated index source.
//!
//! Fetches `<index_url>` (default
//! `https://raw.githubusercontent.com/lordmacu/nexo-plugin-index/main/index.json`)
//! and parses an `IndexFile { schema_version, updated_at, plugins }`.
//! The repo is created in Phase 98.16; until then the source 404s
//! and contributes nothing — runner aggregates the partial failure
//! so the missing repo doesn't blank the catalogue.

use std::time::Duration;

use serde::Deserialize;

use crate::sources::{source_error, Source};
use crate::types::{
    CompatStatus, DiscoveredPlugin, PluginCategory, PluginSource, SourceError, TrustTier,
};
use nexo_tool_meta::admin::plugin_install::{InstallSource, PluginsInstallParams};

use async_trait::async_trait;

/// Source name for `partial_failures` + telemetry.
pub const SOURCE_NAME: &str = "curated_index";

/// Latest supported schema version. Bumped here AND on the
/// `nexo-plugin-index` repo when the JSON shape changes; older
/// daemons treat newer schemas as "unsupported" rather than risk
/// silently dropping fields.
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Curated index HTTP source.
pub struct CuratedIndexSource {
    http: reqwest::Client,
    url: String,
}

impl CuratedIndexSource {
    /// Build a fresh source bound to a single `index.json` URL.
    pub fn new(url: impl Into<String>, http_timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "nexo-plugin-discovery/{} (+https://github.com/lordmacu/nexo-rs)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(http_timeout)
            .build()
            .expect("reqwest client build (rustls) failed");
        Self {
            http,
            url: url.into(),
        }
    }
}

#[async_trait]
impl Source for CuratedIndexSource {
    fn name(&self) -> &'static str {
        SOURCE_NAME
    }

    async fn fetch(&self) -> Result<Vec<DiscoveredPlugin>, SourceError> {
        let resp = self
            .http
            .get(&self.url)
            .send()
            .await
            .map_err(|e| source_error(SOURCE_NAME, format!("GET {}: {e}", self.url)))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            // Index repo legitimately missing during the rollout
            // window (Phase 98.16 hasn't shipped, or an operator
            // overrode `index_url` to one they own and haven't
            // populated yet). Treat as a soft failure so the rest
            // of the catalogue keeps working.
            return Err(source_error(
                SOURCE_NAME,
                "index not found (repo seeded by Phase 98.16; this is non-fatal)",
            ));
        }
        if !status.is_success() {
            return Err(source_error(
                SOURCE_NAME,
                format!("status {status} fetching {}", self.url),
            ));
        }
        let parsed: IndexFile = resp
            .json()
            .await
            .map_err(|e| source_error(SOURCE_NAME, format!("parse {}: {e}", self.url)))?;
        if parsed.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(source_error(
                SOURCE_NAME,
                format!(
                    "unsupported schema_version={} (this daemon expects {})",
                    parsed.schema_version, SUPPORTED_SCHEMA_VERSION
                ),
            ));
        }
        Ok(parsed.plugins.into_iter().map(map_entry).collect())
    }
}

// ── wire shapes (private) ────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct IndexFile {
    schema_version: u32,
    #[serde(default)]
    #[allow(dead_code)]
    updated_at: Option<String>,
    #[serde(default)]
    plugins: Vec<IndexEntry>,
}

#[derive(Debug, Deserialize)]
struct IndexEntry {
    name: String,
    owner: String,
    repo: String,
    #[serde(default)]
    manifest_url: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    icon_url: Option<String>,
}

fn map_entry(raw: IndexEntry) -> DiscoveredPlugin {
    let IndexEntry {
        name,
        owner,
        repo,
        manifest_url,
        category,
        tags,
        description,
        ..
    } = raw;
    let install_params = PluginsInstallParams {
        crate_name: name.clone(),
        version: None,
        repo: Some(repo.clone()),
        source: InstallSource::Release,
        force: false,
        require_signature: false,
        skip_signature_verify: false,
    };
    let install_cmd = format!("cargo install {name}");
    let category = category
        .and_then(|c| parse_category(&c))
        .unwrap_or(PluginCategory::Unknown);
    DiscoveredPlugin {
        name,
        version: None,
        description,
        owner,
        sources: vec![PluginSource::CuratedIndex],
        repo_url: Some(format!("https://github.com/{repo}")),
        homepage: None,
        tags,
        category,
        // Trust tier promotion happens at merge time — being in the
        // index alone earns `CommunityIndexed`, official allowlist
        // membership upgrades to `Official`.
        trust_tier: TrustTier::CommunityIndexed,
        compat: CompatStatus::Unknown,
        manifest_url,
        install_cmd,
        install_params,
    }
}

fn parse_category(s: &str) -> Option<PluginCategory> {
    match s {
        "channel" => Some(PluginCategory::Channel),
        "poller" => Some(PluginCategory::Poller),
        "webhook" => Some(PluginCategory::Webhook),
        "persona" => Some(PluginCategory::Persona),
        "tool" => Some(PluginCategory::Tool),
        "unknown" => Some(PluginCategory::Unknown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture_body() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "updated_at": "2026-05-19T00:00:00Z",
            "plugins": [
                {
                    "name": "nexo-plugin-telegram",
                    "owner": "lordmacu",
                    "repo": "lordmacu/nexo-rs-plugin-telegram",
                    "manifest_url": "https://raw.githubusercontent.com/lordmacu/nexo-rs-plugin-telegram/main/nexo-plugin.toml",
                    "category": "channel",
                    "tags": ["messaging", "telegram"],
                    "description": "Telegram bot channel"
                },
                {
                    "name": "nexo-poller-rss",
                    "owner": "lordmacu",
                    "repo": "lordmacu/nexo-rs-poller-rss",
                    "category": "poller",
                    "tags": ["rss", "feeds"],
                    "description": "RSS feed poller"
                }
            ]
        })
    }

    #[tokio::test]
    async fn happy_path_parses_two_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_body()))
            .mount(&server)
            .await;
        let src = CuratedIndexSource::new(
            format!("{}/index.json", server.uri()),
            Duration::from_secs(5),
        );
        let items = src.fetch().await.expect("ok");
        assert_eq!(items.len(), 2);
        let tele = items
            .iter()
            .find(|p| p.name == "nexo-plugin-telegram")
            .expect("telegram");
        assert_eq!(tele.category, PluginCategory::Channel);
        // Curated index entries land in `CommunityIndexed` at the
        // source layer; merge promotes to `Official` if the owner
        // is in the allowlist.
        assert_eq!(tele.trust_tier, TrustTier::CommunityIndexed);
        assert_eq!(tele.sources, vec![PluginSource::CuratedIndex]);
        let rss = items
            .iter()
            .find(|p| p.name == "nexo-poller-rss")
            .expect("rss");
        assert_eq!(rss.category, PluginCategory::Poller);
    }

    #[tokio::test]
    async fn missing_index_repo_returns_soft_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let src = CuratedIndexSource::new(
            format!("{}/index.json", server.uri()),
            Duration::from_secs(5),
        );
        let err = src.fetch().await.expect_err("404 must surface");
        assert_eq!(err.source, SOURCE_NAME);
        assert!(
            err.message.contains("not found") && err.message.contains("non-fatal"),
            "expected 404 hint, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn malformed_json_surfaces_parse_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("not actually json")
                    .insert_header("content-type", "application/json"),
            )
            .mount(&server)
            .await;
        let src = CuratedIndexSource::new(
            format!("{}/index.json", server.uri()),
            Duration::from_secs(5),
        );
        let err = src.fetch().await.expect_err("must surface parse fail");
        assert!(err.message.contains("parse"), "{}", err.message);
    }

    #[tokio::test]
    async fn unsupported_schema_version_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "schema_version": 999,
                "updated_at": null,
                "plugins": []
            })))
            .mount(&server)
            .await;
        let src = CuratedIndexSource::new(
            format!("{}/index.json", server.uri()),
            Duration::from_secs(5),
        );
        let err = src.fetch().await.expect_err("999 must be rejected");
        assert!(err.message.contains("schema_version"), "{}", err.message);
        assert!(err.message.contains("999"));
    }
}
