//! crates.io REST source.
//!
//! Hits `GET <endpoint>/api/v1/crates?q=<q>&per_page=100&page=N`
//! for each of two queries (`nexo-plugin` + `nexo-poller`),
//! follows pagination until exhausted, filters out crates without a
//! `max_stable_version` (means every version is yanked), and
//! constructs one `DiscoveredPlugin` per entry.

use std::time::Duration;

use serde::Deserialize;
use tracing::warn;

use crate::sources::{source_error, Source};
use crate::types::{
    CompatStatus, DiscoveredPlugin, PluginCategory, PluginSource, SourceError, TrustTier,
};
use nexo_tool_meta::admin::plugin_install::{InstallSource, PluginsInstallParams};

use async_trait::async_trait;

/// Source name for `partial_failures` + telemetry.
pub const SOURCE_NAME: &str = "crates_io";

/// Hard cap on pages walked per query — defensive bound in case
/// crates.io ever returns inconsistent pagination metadata. With
/// `per_page = 100` that's 1000 results per query, comfortably
/// above the realistic catalogue size for years.
const MAX_PAGES: u32 = 10;

/// Per-query keyword. The discovery design uses two — `nexo-plugin`
/// covers channel/tool plugins, `nexo-poller` covers the poller-v2
/// subprocess flavor (Phase 96). Future shapes can be added without
/// touching the source layer.
const QUERIES: &[&str] = &["nexo-plugin", "nexo-poller"];

/// crates.io REST source. Construct once per discovery run +
/// reused across both queries.
pub struct CratesIoSource {
    http: reqwest::Client,
    endpoint: String,
}

impl CratesIoSource {
    /// Build a fresh source. Pass the configured endpoint
    /// (production = `https://crates.io`, tests = wiremock URL).
    /// `http_timeout` flows from `DiscoveryConfig.http_timeout`.
    pub fn new(endpoint: impl Into<String>, http_timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "nexo-plugin-discovery/{} (+https://github.com/lordmacu/nexo-rs)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(http_timeout)
            .build()
            // `reqwest::Client::builder().build()` only fails when
            // the underlying TLS impl can't initialise — pure-rustls
            // never does, so this unwrap is documented-safe.
            .expect("reqwest client build (rustls) failed");
        Self {
            http,
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl Source for CratesIoSource {
    fn name(&self) -> &'static str {
        SOURCE_NAME
    }

    async fn fetch(&self) -> Result<Vec<DiscoveredPlugin>, SourceError> {
        let mut acc: Vec<DiscoveredPlugin> = Vec::new();
        for query in QUERIES {
            let mut page: u32 = 1;
            loop {
                let url = format!(
                    "{}/api/v1/crates?q={}&per_page=100&page={}",
                    self.endpoint.trim_end_matches('/'),
                    query,
                    page
                );
                let resp = self
                    .http
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| source_error(SOURCE_NAME, format!("GET {url}: {e}")))?;
                if !resp.status().is_success() {
                    return Err(source_error(
                        SOURCE_NAME,
                        format!("GET {url}: status {}", resp.status()),
                    ));
                }
                let parsed: CratesIoSearchPage = resp
                    .json()
                    .await
                    .map_err(|e| source_error(SOURCE_NAME, format!("parse {url}: {e}")))?;
                let page_len = parsed.crates.len();
                for raw in parsed.crates.into_iter() {
                    if let Some(plugin) = map_crate(raw) {
                        acc.push(plugin);
                    }
                }
                if page_len < 100 || page >= MAX_PAGES {
                    break;
                }
                page += 1;
            }
        }
        Ok(acc)
    }
}

// ── wire shapes (private — not part of the public API) ───────────

#[derive(Debug, Deserialize)]
struct CratesIoSearchPage {
    crates: Vec<CratesIoCrate>,
}

#[derive(Debug, Deserialize)]
struct CratesIoCrate {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    max_stable_version: Option<String>,
    #[serde(default)]
    keywords: Option<Vec<String>>,
}

/// Map a crates.io result to `DiscoveredPlugin`. Returns `None`
/// when the crate has no non-yanked version (all yanked → omit
/// from catalogue rather than expose an un-installable entry).
fn map_crate(raw: CratesIoCrate) -> Option<DiscoveredPlugin> {
    let CratesIoCrate {
        name,
        description,
        repository,
        homepage,
        max_stable_version,
        keywords,
    } = raw;
    let Some(version) = max_stable_version else {
        warn!(
            target: "plugin_discovery::crates_io",
            crate_name = %name,
            "skipping — every version yanked (no max_stable_version)"
        );
        return None;
    };
    let owner = owner_from_repo(repository.as_deref()).unwrap_or_else(|| "unknown".to_string());
    let install_params = PluginsInstallParams {
        crate_name: name.clone(),
        version: Some(version.clone()),
        repo: repo_slug(repository.as_deref()),
        source: InstallSource::Release,
        force: false,
        require_signature: false,
        skip_signature_verify: false,
    };
    let install_cmd = format!("cargo install {name} --version {version}");
    Some(DiscoveredPlugin {
        name,
        version: Some(version),
        description,
        owner,
        sources: vec![PluginSource::CratesIo],
        repo_url: repository,
        homepage,
        tags: keywords.unwrap_or_default(),
        // Categories are derived from manifest sections in 98.8 —
        // the source layer can't see the TOML yet. Leave as
        // Unknown; merge step overwrites once the manifest fetch
        // completes.
        category: PluginCategory::Unknown,
        // Trust tier resolved at merge time too (needs the
        // `official_owners` allowlist + the curated-index name
        // set; both live above the source layer).
        trust_tier: TrustTier::Unverified,
        // Compat check needs the manifest; defer to merge.
        compat: CompatStatus::Unknown,
        manifest_url: None,
        install_cmd,
        install_params,
    })
}

/// Pull the GitHub org from a repo URL. crates.io stores user-
/// supplied URLs verbatim so we accept the common shapes:
///   - `https://github.com/<org>/<name>` (with or without `.git`)
///   - `https://github.com/<org>/<name>/tree/main`
/// Anything else → `None`.
fn owner_from_repo(url: Option<&str>) -> Option<String> {
    let u = url?;
    let prefix = "https://github.com/";
    let rest = u.strip_prefix(prefix)?;
    let org = rest.split('/').next()?;
    if org.is_empty() {
        None
    } else {
        Some(org.to_string())
    }
}

/// Build the `<org>/<name>` slug consumed by
/// `PluginsInstallParams.repo`. Returns `None` when we can't tell
/// — the install handler then falls back to its
/// `lordmacu/<crate>` default.
fn repo_slug(url: Option<&str>) -> Option<String> {
    let u = url?;
    let prefix = "https://github.com/";
    let rest = u.strip_prefix(prefix)?;
    let mut parts = rest.split('/');
    let org = parts.next()?;
    let name = parts.next()?;
    if org.is_empty() || name.is_empty() {
        return None;
    }
    // Trim `.git` suffix or any path fragment.
    let name = name.trim_end_matches(".git");
    Some(format!("{org}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn page_body(items: &[(&str, &str, &str)]) -> serde_json::Value {
        // (name, version, repo)
        let crates: Vec<_> = items
            .iter()
            .map(|(name, version, repo)| {
                serde_json::json!({
                    "name": name,
                    "description": "demo",
                    "repository": repo,
                    "homepage": null,
                    "max_stable_version": version,
                    "keywords": ["nexo", "demo"],
                })
            })
            .collect();
        serde_json::json!({ "crates": crates })
    }

    /// Build a page response of EXACTLY 100 entries so the
    /// pagination loop continues to the next page.
    fn full_page_body(prefix: &str, version: &str, repo: &str) -> serde_json::Value {
        let crates: Vec<_> = (0..100)
            .map(|i| {
                serde_json::json!({
                    "name": format!("{prefix}-{i}"),
                    "description": "demo",
                    "repository": repo,
                    "homepage": null,
                    "max_stable_version": version,
                    "keywords": [],
                })
            })
            .collect();
        serde_json::json!({ "crates": crates })
    }

    #[tokio::test]
    async fn happy_path_single_page_returns_items() {
        let server = MockServer::start().await;
        let body = page_body(&[
            (
                "nexo-plugin-telegram",
                "0.3.0",
                "https://github.com/lordmacu/nexo-rs-plugin-telegram",
            ),
            (
                "nexo-plugin-whatsapp",
                "0.1.3",
                "https://github.com/lordmacu/nexo-rs-plugin-whatsapp",
            ),
        ]);
        // Both queries return the same 2 entries; the merge step
        // dedups by name (verified in 98.8). Source layer just
        // emits whatever crates.io says.
        Mock::given(method("GET"))
            .and(path("/api/v1/crates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let src = CratesIoSource::new(server.uri(), Duration::from_secs(5));
        let items = src.fetch().await.expect("fetch ok");
        // 2 entries × 2 queries (no dedup at source layer).
        assert_eq!(items.len(), 4);
        let tele = items
            .iter()
            .find(|p| p.name == "nexo-plugin-telegram")
            .expect("telegram present");
        assert_eq!(tele.owner, "lordmacu");
        assert_eq!(tele.version.as_deref(), Some("0.3.0"));
        assert_eq!(tele.sources, vec![PluginSource::CratesIo]);
        assert_eq!(
            tele.install_cmd,
            "cargo install nexo-plugin-telegram --version 0.3.0"
        );
        // `install_params.repo` must be the slug, not the full URL.
        assert_eq!(
            tele.install_params.repo.as_deref(),
            Some("lordmacu/nexo-rs-plugin-telegram")
        );
    }

    #[tokio::test]
    async fn yanked_only_crate_is_filtered_out() {
        let server = MockServer::start().await;
        // Crate with `max_stable_version: null` → every version
        // yanked. Must NOT appear in results.
        let body = serde_json::json!({
            "crates": [
                {
                    "name": "nexo-plugin-broken",
                    "description": "all yanked",
                    "repository": "https://github.com/lordmacu/broken",
                    "max_stable_version": serde_json::Value::Null,
                    "keywords": []
                }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/api/v1/crates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let src = CratesIoSource::new(server.uri(), Duration::from_secs(5));
        let items = src.fetch().await.expect("fetch ok");
        assert!(
            items.is_empty(),
            "yanked-only crate must be filtered out, got {items:?}"
        );
    }

    #[tokio::test]
    async fn paginates_until_short_page() {
        let server = MockServer::start().await;
        // Page 1 full (100 entries) → pagination MUST request
        // page 2; page 2 short → loop exits. Verified per query —
        // each query independently paginates.
        Mock::given(method("GET"))
            .and(path("/api/v1/crates"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(full_page_body(
                "nexo-plugin",
                "0.1.0",
                "https://github.com/x/y",
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/crates"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&[(
                "nexo-plugin-final",
                "0.2.0",
                "https://github.com/x/y",
            )])))
            .mount(&server)
            .await;
        let src = CratesIoSource::new(server.uri(), Duration::from_secs(5));
        let items = src.fetch().await.expect("fetch ok");
        // 100 (page 1) + 1 (page 2) = 101 entries per query × 2
        // queries = 202 total. The "nexo-plugin-final" name appears
        // once per query (so twice total).
        assert_eq!(items.len(), 202);
        assert!(items.iter().any(|p| p.name == "nexo-plugin-final"));
    }

    #[tokio::test]
    async fn http_500_surfaces_as_source_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/crates"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let src = CratesIoSource::new(server.uri(), Duration::from_secs(5));
        let err = src.fetch().await.expect_err("5xx must surface");
        assert_eq!(err.source, SOURCE_NAME);
        assert!(err.message.contains("status 500"), "{}", err.message);
    }

    #[test]
    fn owner_from_repo_handles_common_shapes() {
        assert_eq!(
            owner_from_repo(Some("https://github.com/lordmacu/foo")),
            Some("lordmacu".into())
        );
        assert_eq!(
            owner_from_repo(Some("https://github.com/lordmacu/foo.git")),
            Some("lordmacu".into())
        );
        assert_eq!(
            owner_from_repo(Some("https://github.com/lordmacu/foo/tree/main")),
            Some("lordmacu".into())
        );
        assert_eq!(owner_from_repo(Some("https://gitlab.com/foo/bar")), None);
        assert_eq!(owner_from_repo(None), None);
    }

    #[test]
    fn repo_slug_trims_git_suffix() {
        assert_eq!(
            repo_slug(Some("https://github.com/lordmacu/foo.git")).as_deref(),
            Some("lordmacu/foo")
        );
        assert_eq!(
            repo_slug(Some("https://github.com/lordmacu/foo/tree/main")).as_deref(),
            Some("lordmacu/foo")
        );
        assert!(repo_slug(Some("https://github.com/")).is_none());
        assert!(repo_slug(None).is_none());
    }
}
