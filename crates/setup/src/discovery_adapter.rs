//! Phase 98.11 — production `DiscoveryReader` adapter wiring the
//! daemon's admin RPC to `nexo-plugin-discovery::DefaultDiscoveryClient`.
//!
//! Lives in `nexo-setup` (not `nexo-core`) so the daemon binary can
//! construct a single `Arc<dyn DiscoveryReader>` at boot + thread
//! it through the existing admin-bootstrap inputs struct without
//! `nexo-core` taking a hard dep on the HTTP-fetching crate.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use nexo_core::agent::admin_rpc::domains::plugin_discovery::DiscoveryReader;
use nexo_plugin_discovery::client::{DefaultDiscoveryClient, DiscoveryClient};
use nexo_tool_meta::admin::plugin_discovery::{
    PluginsCompatCheckParams, PluginsCompatCheckResponse, PluginsRefreshIndexResponse,
    PluginsSearchParams, PluginsSearchResponse, SourceError,
};

/// Concrete adapter — delegates to `DefaultDiscoveryClient` +
/// applies the request-side filters (compat_only / category /
/// source) before returning. The discovery client itself only
/// supports query substring filtering; pushing the structured
/// filters server-side at this layer keeps `nexo-plugin-discovery`
/// reusable for future non-daemon callers (e.g. CLI direct mode).
pub struct DefaultDiscoveryAdapter {
    client: Arc<dyn DiscoveryClient>,
}

impl std::fmt::Debug for DefaultDiscoveryAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultDiscoveryAdapter").finish()
    }
}

impl DefaultDiscoveryAdapter {
    /// Wrap a discovery client. Takes `Arc<dyn DiscoveryClient>` so
    /// tests can inject fakes; production wires
    /// `Arc::new(DefaultDiscoveryClient::new(config))`.
    pub fn new(client: Arc<dyn DiscoveryClient>) -> Self {
        Self { client }
    }

    /// Convenience constructor for production: build the standard
    /// client from a `DiscoveryConfig` + wrap.
    pub fn from_default_client(
        config: nexo_plugin_discovery::config::DiscoveryConfig,
    ) -> Arc<dyn DiscoveryReader> {
        let client: Arc<dyn DiscoveryClient> = Arc::new(DefaultDiscoveryClient::new(config));
        Arc::new(Self::new(client))
    }
}

#[async_trait]
impl DiscoveryReader for DefaultDiscoveryAdapter {
    async fn search(
        &self,
        params: &PluginsSearchParams,
    ) -> anyhow::Result<PluginsSearchResponse> {
        let outcome = self
            .client
            .search(params.query.as_deref())
            .await?;
        let category_filter = params.category.as_deref();
        let source_filter = params.source.as_deref();
        let compat_only = params.compat_only;
        let items: Vec<_> = outcome
            .items
            .into_iter()
            .filter(|p| {
                if compat_only && !is_compat_pass(&p.compat) {
                    return false;
                }
                if let Some(cat) = category_filter {
                    if category_label(&p.category) != cat {
                        return false;
                    }
                }
                if let Some(src) = source_filter {
                    if !p.sources.iter().any(|s| source_label(s) == src) {
                        return false;
                    }
                }
                true
            })
            .collect();
        Ok(PluginsSearchResponse {
            items,
            fetched_at_ms: outcome.fetched_at_ms,
            partial_failures: outcome.partial_failures,
        })
    }

    async fn compat_check(
        &self,
        params: &PluginsCompatCheckParams,
    ) -> anyhow::Result<PluginsCompatCheckResponse> {
        let outcome = self
            .client
            .compat_check(&params.crate_name, params.version.as_deref())
            .await?;
        Ok(PluginsCompatCheckResponse {
            compat: outcome.compat,
            manifest_summary: outcome.manifest_summary,
        })
    }

    async fn refresh_index(&self) -> anyhow::Result<PluginsRefreshIndexResponse> {
        self.client.refresh().await?;
        // After invalidating the disk cache, a fresh `search` re-
        // runs every source — return the scrape diagnostics from
        // that run so the operator sees which sources contributed.
        let outcome = self.client.search(None).await?;
        let items_count = outcome.items.len();
        let mut sources_ok: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for item in outcome.items.iter() {
            for s in item.sources.iter() {
                let label = source_label(s).to_string();
                if seen.insert(label.clone()) {
                    sources_ok.push(label);
                }
            }
        }
        sources_ok.sort();
        Ok(PluginsRefreshIndexResponse {
            items_count,
            sources_ok,
            sources_err: outcome
                .partial_failures
                .into_iter()
                .map(|e| SourceError {
                    source: e.source,
                    message: e.message,
                })
                .collect(),
        })
    }
}

fn is_compat_pass(c: &nexo_tool_meta::admin::plugin_discovery::CompatStatus) -> bool {
    use nexo_tool_meta::admin::plugin_discovery::CompatStatus::*;
    // `Unknown` passes through `compat_only` — manifest fetch
    // failures shouldn't blank the catalogue for cautious operators
    // (they'll see the badge + tooltip).
    matches!(c, Compatible | Unknown)
}

fn category_label(c: &nexo_tool_meta::admin::plugin_discovery::PluginCategory) -> &'static str {
    use nexo_tool_meta::admin::plugin_discovery::PluginCategory::*;
    match c {
        Channel => "channel",
        Poller => "poller",
        Webhook => "webhook",
        Persona => "persona",
        Tool => "tool",
        Unknown => "unknown",
    }
}

fn source_label(s: &nexo_tool_meta::admin::plugin_discovery::PluginSource) -> &'static str {
    use nexo_tool_meta::admin::plugin_discovery::PluginSource::*;
    match s {
        CratesIo => "crates_io",
        GithubTopic { .. } => "github_topic",
        CuratedIndex => "curated_index",
    }
}

/// Helper for the daemon binary — returns `now()` in milliseconds.
/// Used when the daemon needs to stamp a `fetched_at_ms` on synth-
/// etic responses (e.g. during boot if discovery isn't ready yet).
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_plugin_discovery::client::{CompatCheckOutcome, SearchOutcome};
    use nexo_tool_meta::admin::plugin_discovery::{
        CompatStatus, DiscoveredPlugin, PluginCategory, PluginSource, TrustTier,
    };
    use nexo_tool_meta::admin::plugin_install::{InstallSource, PluginsInstallParams};
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubClient {
        last_query: Mutex<Option<String>>,
        refresh_calls: Mutex<u32>,
    }

    fn plugin(name: &str, category: PluginCategory, source: PluginSource, compat: CompatStatus) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.into(),
            version: Some("0.1.0".into()),
            description: None,
            owner: "lordmacu".into(),
            sources: vec![source],
            repo_url: None,
            homepage: None,
            tags: vec![],
            category,
            trust_tier: TrustTier::Official,
            compat,
            manifest_url: None,
            install_cmd: format!("cargo install {name}"),
            install_params: PluginsInstallParams {
                crate_name: name.into(),
                version: Some("0.1.0".into()),
                repo: None,
                source: InstallSource::Release,
                force: false,
                require_signature: false,
                skip_signature_verify: false,
            },
        }
    }

    #[async_trait]
    impl DiscoveryClient for StubClient {
        async fn search(&self, query: Option<&str>) -> anyhow::Result<SearchOutcome> {
            *self.last_query.lock().unwrap() = query.map(String::from);
            Ok(SearchOutcome {
                items: vec![
                    plugin(
                        "nexo-plugin-telegram",
                        PluginCategory::Channel,
                        PluginSource::CratesIo,
                        CompatStatus::Compatible,
                    ),
                    plugin(
                        "nexo-poller-rss",
                        PluginCategory::Poller,
                        PluginSource::CuratedIndex,
                        CompatStatus::NeedsUpgrade {
                            required: ">=1.0".into(),
                            current: "0.1.0".into(),
                        },
                    ),
                    plugin(
                        "nexo-plugin-foo",
                        PluginCategory::Tool,
                        PluginSource::GithubTopic {
                            repo: "x/y".into(),
                        },
                        CompatStatus::Unknown,
                    ),
                ],
                fetched_at_ms: 123,
                partial_failures: vec![],
            })
        }
        async fn refresh(&self) -> anyhow::Result<()> {
            *self.refresh_calls.lock().unwrap() += 1;
            Ok(())
        }
        async fn compat_check(
            &self,
            _crate_name: &str,
            _version: Option<&str>,
        ) -> anyhow::Result<CompatCheckOutcome> {
            Ok(CompatCheckOutcome {
                compat: CompatStatus::Compatible,
                manifest_summary: None,
            })
        }
    }

    fn adapter() -> (Arc<StubClient>, DefaultDiscoveryAdapter) {
        let stub = Arc::new(StubClient::default());
        let adapter = DefaultDiscoveryAdapter::new(stub.clone() as Arc<dyn DiscoveryClient>);
        (stub, adapter)
    }

    #[tokio::test]
    async fn search_passes_query_through_to_client() {
        let (stub, ad) = adapter();
        let params = PluginsSearchParams {
            query: Some("telegram".into()),
            ..Default::default()
        };
        let _ = ad.search(&params).await.unwrap();
        assert_eq!(stub.last_query.lock().unwrap().as_deref(), Some("telegram"));
    }

    #[tokio::test]
    async fn search_compat_only_drops_needs_upgrade_keeps_unknown() {
        let (_stub, ad) = adapter();
        let params = PluginsSearchParams {
            compat_only: true,
            ..Default::default()
        };
        let resp = ad.search(&params).await.unwrap();
        // Telegram (Compatible) + foo (Unknown) stay; rss
        // (NeedsUpgrade) drops.
        let names: Vec<_> = resp.items.iter().map(|p| p.name.clone()).collect();
        assert!(names.contains(&"nexo-plugin-telegram".to_string()));
        assert!(names.contains(&"nexo-plugin-foo".to_string()));
        assert!(!names.contains(&"nexo-poller-rss".to_string()));
    }

    #[tokio::test]
    async fn search_category_filter_isolates_one_bucket() {
        let (_stub, ad) = adapter();
        let params = PluginsSearchParams {
            category: Some("poller".into()),
            ..Default::default()
        };
        let resp = ad.search(&params).await.unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "nexo-poller-rss");
    }

    #[tokio::test]
    async fn search_source_filter_isolates_curated_index() {
        let (_stub, ad) = adapter();
        let params = PluginsSearchParams {
            source: Some("curated_index".into()),
            ..Default::default()
        };
        let resp = ad.search(&params).await.unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "nexo-poller-rss");
    }

    #[tokio::test]
    async fn refresh_index_invalidates_then_rescans() {
        let (stub, ad) = adapter();
        let resp = ad.refresh_index().await.unwrap();
        assert_eq!(*stub.refresh_calls.lock().unwrap(), 1);
        assert_eq!(resp.items_count, 3);
        assert!(resp.sources_ok.contains(&"crates_io".into()));
        assert!(resp.sources_ok.contains(&"curated_index".into()));
        assert!(resp.sources_ok.contains(&"github_topic".into()));
    }

    #[tokio::test]
    async fn compat_check_delegates_to_client() {
        let (_stub, ad) = adapter();
        let params = PluginsCompatCheckParams {
            crate_name: "nexo-plugin-x".into(),
            version: None,
        };
        let resp = ad.compat_check(&params).await.unwrap();
        assert_eq!(resp.compat, CompatStatus::Compatible);
    }
}
