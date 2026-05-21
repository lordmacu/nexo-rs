//! `DiscoveryClient` trait + `DefaultDiscoveryClient` orchestration.
//!
//! End-to-end glue:
//!   1. Cache check (`DiskCache::read_fresh`) — if fresh, short-
//!      circuit + optionally filter by query.
//!   2. Cold path: instantiate the 3 sources, run them in parallel
//!      via `sources::run_all`, fetch manifests for each item that
//!      ships a `manifest_url`, derive `compat` + overwrite
//!      `category` from the manifest, merge by `name` + promote
//!      trust tier, persist to disk, return.
//!   3. `refresh()` invalidates the cache so the next `search()`
//!      forces a cold path.
//!   4. `compat_check()` resolves the crate's repo via crates.io
//!      (or accepts an explicit repo from the caller) + fetches
//!      the manifest from raw GitHub.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tracing::warn;

use crate::cache::DiskCache;
use crate::compat;
use crate::config::DiscoveryConfig;
use crate::manifest_fetcher::{fetch_manifest, FetchedManifest};
use crate::merge::merge;
use crate::sources::crates_io::CratesIoSource;
use crate::sources::curated_index::CuratedIndexSource;
use crate::sources::github_topic::GithubTopicSource;
use crate::sources::{run_all, RunOutcome, Source};
use crate::types::{
    CachedCatalogue, CompatStatus, DiscoveredPlugin, ManifestSummary, PluginSource,
};

/// Public surface the admin RPC + CLI consume.
#[async_trait]
pub trait DiscoveryClient: Send + Sync {
    /// Return the merged catalogue. `query` filters client-side
    /// against `name + description + tags` substring; `None` =
    /// return everything.
    async fn search(&self, query: Option<&str>) -> anyhow::Result<SearchOutcome>;

    /// Drop the disk cache so the next `search` re-fetches every
    /// source. Daemon admin RPC `nexo/admin/plugins/refresh_index`
    /// drives this.
    async fn refresh(&self) -> anyhow::Result<()>;

    /// Resolve compat + manifest summary for a single crate. Used
    /// by the admin RPC pre-install dialog so the operator can
    /// confirm SDK / category before clicking Install.
    async fn compat_check(
        &self,
        crate_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<CompatCheckOutcome>;
}

/// Output of `search`. Mirrors the admin RPC wire shape minus the
/// `fetched_at_ms` (the adapter stamps that).
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    /// Merged + filtered catalogue.
    pub items: Vec<DiscoveredPlugin>,
    /// Unix milliseconds when the underlying catalogue snapshot was
    /// produced. Older value when served from cache; "now" on cold
    /// fetch.
    pub fetched_at_ms: u64,
    /// Sources that failed during the run that produced the
    /// returned items.
    pub partial_failures: Vec<crate::types::SourceError>,
}

/// Output of `compat_check`.
#[derive(Debug, Clone)]
pub struct CompatCheckOutcome {
    /// Compat result against the daemon's running version.
    pub compat: CompatStatus,
    /// Parsed manifest summary when fetched cleanly; `None`
    /// otherwise (404 / parse error / no manifest URL provided).
    pub manifest_summary: Option<ManifestSummary>,
}

/// Default implementation backed by HTTP + disk cache.
pub struct DefaultDiscoveryClient {
    config: DiscoveryConfig,
    http: reqwest::Client,
    cache: DiskCache,
}

impl DefaultDiscoveryClient {
    /// Build a client bound to a config. Constructs its own
    /// rustls-only `reqwest::Client` reused across manifest fetches
    /// + compat checks. Each source constructs its OWN client
    /// (with source-specific User-Agent + headers) inside
    /// `cold_fetch` — keeping the HTTP layer per-source means a
    /// single shared client can't accidentally smuggle a GitHub
    /// `Authorization` header to crates.io.
    pub fn new(config: DiscoveryConfig) -> Self {
        let cache_path = config.cache_file_path();
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "nexo-plugin-discovery/{} (+https://github.com/lordmacu/nexo-rs)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(config.http_timeout)
            .build()
            .expect("reqwest client (rustls) failed");
        Self {
            config,
            http,
            cache: DiskCache::new(cache_path),
        }
    }

    /// Arc-wrap helper for the admin adapter — the daemon registers
    /// one `Arc<dyn DiscoveryClient>` and shares it across handlers.
    pub fn into_arc(self) -> Arc<dyn DiscoveryClient> {
        Arc::new(self) as Arc<dyn DiscoveryClient>
    }

    fn build_sources(&self) -> Vec<Box<dyn Source>> {
        let timeout = self.config.http_timeout;
        // Source order matters for the merge step's "first non-None
        // wins" tiebreaker (description / manifest_url / homepage):
        //   1. CuratedIndex first — operator-curated metadata is the
        //      most authoritative (manifest_url points to the
        //      canonical raw-github path; category + tags hand-
        //      picked rather than auto-derived).
        //   2. CratesIo second — registry data is exact + version-
        //      authoritative; merge picks up `version` here.
        //   3. GithubTopic third — auto-derived signal lowest in
        //      priority because the repo's name → crate name mapping
        //      is heuristic; manifest URL falls back to the curated
        //      one when both sources match.
        let mut srcs: Vec<Box<dyn Source>> = Vec::with_capacity(3);
        srcs.push(Box::new(CuratedIndexSource::new(
            self.config.index_url.clone(),
            timeout,
        )));
        srcs.push(Box::new(CratesIoSource::new(
            self.config.crates_io_endpoint.clone(),
            timeout,
        )));
        srcs.push(Box::new(GithubTopicSource::new(
            self.config.github_endpoint.clone(),
            self.config.raw_github_endpoint.clone(),
            timeout,
            self.config.github_token.clone(),
        )));
        srcs
    }

    /// Cold-path: hit every source, fetch manifests, merge, persist.
    async fn cold_fetch(&self) -> SearchOutcome {
        let sources = self.build_sources();
        let RunOutcome {
            items,
            partial_failures,
        } = run_all(&sources, self.config.http_timeout).await;

        // Fetch manifests in parallel so the wall-clock stays
        // bounded by the slowest manifest fetch, not the sum of
        // them. We don't fail the whole run if a manifest 404s —
        // those entries just keep `CompatStatus::Unknown` /
        // `PluginCategory::Unknown`.
        let manifests = self.fetch_all_manifests(&items).await;
        let mut enriched: Vec<DiscoveredPlugin> = items
            .into_iter()
            .zip(manifests)
            .map(|(mut plugin, fetched)| {
                if let Some(f) = fetched {
                    plugin.category = f.category;
                    plugin.compat = compat::compat_check(
                        Some(&f.min_nexo_version),
                        &self.config.daemon_version,
                    );
                    if plugin.manifest_url.is_none() {
                        plugin.manifest_url = Some(f.source_url);
                    }
                }
                plugin
            })
            .collect();

        // Compute the curated-index name set BEFORE merge (merge
        // dedups by name so we'd lose `PluginSource::CuratedIndex`
        // entries to merge with their siblings otherwise).
        let indexed_names: HashSet<String> = enriched
            .iter()
            .filter(|p| {
                p.sources
                    .iter()
                    .any(|s| matches!(s, PluginSource::CuratedIndex))
            })
            .map(|p| p.name.clone())
            .collect();

        enriched = merge(enriched, &self.config.official_owners, &indexed_names);

        let fetched_at_ms = now_ms();
        let snap = CachedCatalogue {
            fetched_at_ms,
            items: enriched.clone(),
        };
        if let Err(e) = self.cache.write_atomic(&snap).await {
            warn!(
                target: "plugin_discovery::client",
                error = %e,
                "cache write failed — continuing with in-memory snapshot"
            );
        }

        SearchOutcome {
            items: enriched,
            fetched_at_ms,
            partial_failures,
        }
    }

    async fn fetch_all_manifests(
        &self,
        items: &[DiscoveredPlugin],
    ) -> Vec<Option<FetchedManifest>> {
        let mut futs = Vec::with_capacity(items.len());
        for item in items.iter() {
            let http = self.http.clone();
            let primary = item.manifest_url.clone();
            let fallbacks = derive_fallback_urls(item);
            let timeout = self.config.http_timeout;
            futs.push(async move {
                match primary {
                    Some(url) => fetch_manifest(&http, &url, &fallbacks, timeout).await,
                    None => {
                        // No URL hint — try the heuristic fallbacks
                        // (raw github main + master) if we have a
                        // repo_url. Nothing to lose; manifest fetch
                        // is soft-fail by design.
                        if let Some(first) = fallbacks.first() {
                            fetch_manifest(&http, first, &fallbacks[1..], timeout).await
                        } else {
                            None
                        }
                    }
                }
            });
        }
        futures::future::join_all(futs).await
    }

    /// Apply the query filter client-side. `query` matches as a
    /// case-insensitive substring against `name + description +
    /// tags`. `None` returns the input unchanged.
    fn filter_by_query(items: Vec<DiscoveredPlugin>, query: Option<&str>) -> Vec<DiscoveredPlugin> {
        let Some(q) = query else { return items };
        let q_lower = q.to_lowercase();
        if q_lower.is_empty() {
            return items;
        }
        items
            .into_iter()
            .filter(|p| {
                if p.name.to_lowercase().contains(&q_lower) {
                    return true;
                }
                if let Some(d) = p.description.as_deref() {
                    if d.to_lowercase().contains(&q_lower) {
                        return true;
                    }
                }
                p.tags.iter().any(|t| t.to_lowercase().contains(&q_lower))
            })
            .collect()
    }
}

#[async_trait]
impl DiscoveryClient for DefaultDiscoveryClient {
    async fn search(&self, query: Option<&str>) -> anyhow::Result<SearchOutcome> {
        let ttl_ms = self.config.cache_ttl.as_millis() as u64;
        let now = now_ms();
        if let Some(snap) = self.cache.read_fresh(now, ttl_ms).await? {
            let mut items = snap.items;
            // Recompute compat against the CURRENT daemon version. The
            // cached `compat` was computed when the snapshot was written
            // — possibly by an OLDER daemon — so without this the
            // "needs upgrade" badge would persist for the full cache TTL
            // after a daemon upgrade. `NeedsUpgrade` carries its
            // `required` range, so re-running the cheap version check
            // against the live daemon version clears (or keeps) it
            // correctly without a re-fetch.
            for p in &mut items {
                if let CompatStatus::NeedsUpgrade { required, .. } = &p.compat {
                    if let Ok(req) = required.parse::<semver::VersionReq>() {
                        p.compat = compat::compat_check(Some(&req), &self.config.daemon_version);
                    }
                }
            }
            let filtered = Self::filter_by_query(items, query);
            return Ok(SearchOutcome {
                items: filtered,
                fetched_at_ms: snap.fetched_at_ms,
                partial_failures: Vec::new(),
            });
        }
        let outcome = self.cold_fetch().await;
        let filtered = Self::filter_by_query(outcome.items, query);
        Ok(SearchOutcome {
            items: filtered,
            fetched_at_ms: outcome.fetched_at_ms,
            partial_failures: outcome.partial_failures,
        })
    }

    async fn refresh(&self) -> anyhow::Result<()> {
        self.cache.invalidate().await?;
        Ok(())
    }

    async fn compat_check(
        &self,
        crate_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<CompatCheckOutcome> {
        // Find the entry in the catalogue (which already carries
        // manifest_url). Cheaper than re-resolving the repo via
        // crates.io API every time.
        let SearchOutcome { items, .. } = self.search(None).await?;
        let target = items.into_iter().find(|p| p.name == crate_name);
        let Some(plugin) = target else {
            return Ok(CompatCheckOutcome {
                compat: CompatStatus::Unknown,
                manifest_summary: None,
            });
        };
        // Re-fetch manifest fresh so the operator's compat dialog
        // can't show a stale 24h-cached answer.
        //
        // Phase 98 follow-up #5 — when `version` is provided, try
        // tag-pinned URLs FIRST (`<raw>/<repo>/v<version>/nexo-plugin.toml`
        // + `<raw>/<repo>/<version>/nexo-plugin.toml`) before falling
        // back to the plugin's HEAD-pointing manifest URL. This lets
        // the admin UI show "version X needs SDK Y" without re-
        // fetching every prior release.
        let head_fallbacks = derive_fallback_urls(&plugin);
        let mut candidates: Vec<String> = Vec::new();
        if let Some(v) = version {
            for tag in [format!("v{v}"), v.to_string()] {
                candidates.extend(derive_tagged_urls(
                    &plugin,
                    &self.config.raw_github_endpoint,
                    &tag,
                ));
            }
        }
        if let Some(url) = plugin.manifest_url.as_deref() {
            candidates.push(url.to_string());
        }
        candidates.extend(head_fallbacks);
        let manifest = match candidates.first() {
            Some(first) => {
                let rest: Vec<String> = candidates.iter().skip(1).cloned().collect();
                fetch_manifest(&self.http, first, &rest, self.config.http_timeout).await
            }
            None => None,
        };
        let Some(f) = manifest else {
            return Ok(CompatCheckOutcome {
                compat: CompatStatus::Unknown,
                manifest_summary: None,
            });
        };
        let compat = compat::compat_check(Some(&f.min_nexo_version), &self.config.daemon_version);
        Ok(CompatCheckOutcome {
            compat,
            manifest_summary: Some(f.summary),
        })
    }
}

/// Phase 98 follow-up #5 — build a raw-github manifest URL pinned
/// to a specific tag (or branch). The fetcher tries this BEFORE
/// falling back to the plugin's HEAD-pointing `manifest_url`, so
/// `compat_check { crate_name, version: Some(v) }` actually sees
/// the manifest the operator is about to install rather than HEAD.
/// Empty Vec when `repo_url` is missing.
fn derive_tagged_urls(
    plugin: &DiscoveredPlugin,
    raw_github_endpoint: &str,
    tag: &str,
) -> Vec<String> {
    let Some(repo_url) = plugin.repo_url.as_deref() else {
        return Vec::new();
    };
    let prefix = "https://github.com/";
    let Some(rest) = repo_url.strip_prefix(prefix) else {
        return Vec::new();
    };
    let mut parts = rest.split('/');
    let (Some(org), Some(name)) = (parts.next(), parts.next()) else {
        return Vec::new();
    };
    let name = name.trim_end_matches(".git");
    if org.is_empty() || name.is_empty() || tag.is_empty() {
        return Vec::new();
    }
    let base = raw_github_endpoint.trim_end_matches('/');
    vec![format!("{base}/{org}/{name}/{tag}/nexo-plugin.toml")]
}

/// Derive raw-github manifest URLs from a `DiscoveredPlugin`'s
/// `repo_url`. Returns up to two candidates (`main` then `master`)
/// so the manifest fetcher can fall through.
fn derive_fallback_urls(plugin: &DiscoveredPlugin) -> Vec<String> {
    let Some(repo_url) = plugin.repo_url.as_deref() else {
        return Vec::new();
    };
    // `repo_url` may be `https://github.com/org/name` or its
    // `.git` variant; normalise to `org/name`.
    let prefix = "https://github.com/";
    let Some(rest) = repo_url.strip_prefix(prefix) else {
        return Vec::new();
    };
    let mut parts = rest.split('/');
    let Some(org) = parts.next() else {
        return Vec::new();
    };
    let Some(name) = parts.next() else {
        return Vec::new();
    };
    let name = name.trim_end_matches(".git");
    if org.is_empty() || name.is_empty() {
        return Vec::new();
    }
    vec![
        format!("https://raw.githubusercontent.com/{org}/{name}/main/nexo-plugin.toml"),
        format!("https://raw.githubusercontent.com/{org}/{name}/master/nexo-plugin.toml"),
    ]
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// Re-exports used by callers that don't want to peek inside the
// module surface.
pub use crate::types::SourceError;

#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::types::{PluginCategory, TrustTier};
    use nexo_tool_meta::admin::plugin_install::{InstallSource, PluginsInstallParams};

    fn stub(name: &str, description: Option<&str>, tags: &[&str]) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.into(),
            version: Some("0.1.0".into()),
            description: description.map(String::from),
            owner: "lordmacu".into(),
            sources: vec![PluginSource::CratesIo],
            repo_url: Some(format!("https://github.com/lordmacu/{name}")),
            homepage: None,
            tags: tags.iter().map(|s| (*s).to_string()).collect(),
            category: PluginCategory::Unknown,
            trust_tier: TrustTier::Unverified,
            compat: CompatStatus::Unknown,
            manifest_url: None,
            install_cmd: format!("cargo install {name} --version 0.1.0"),
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

    #[test]
    fn filter_by_query_matches_name_substring_case_insensitive() {
        let items = vec![
            stub("nexo-plugin-telegram", Some("Telegram bot"), &["messaging"]),
            stub("nexo-plugin-email", Some("Email channel"), &["messaging"]),
        ];
        let r = DefaultDiscoveryClient::filter_by_query(items.clone(), Some("TELEGRAM"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "nexo-plugin-telegram");
    }

    #[test]
    fn filter_by_query_matches_description_and_tag() {
        let items = vec![
            stub("nexo-plugin-rss", Some("RSS poller"), &["feeds"]),
            stub("nexo-plugin-mail", Some("Email"), &["messaging"]),
        ];
        // Match via description.
        let r = DefaultDiscoveryClient::filter_by_query(items.clone(), Some("rss poller"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "nexo-plugin-rss");
        // Match via tag.
        let r = DefaultDiscoveryClient::filter_by_query(items, Some("messaging"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "nexo-plugin-mail");
    }

    #[test]
    fn filter_by_query_none_returns_all() {
        let items = vec![stub("foo", None, &[])];
        let r = DefaultDiscoveryClient::filter_by_query(items.clone(), None);
        assert_eq!(r.len(), items.len());
    }

    #[test]
    fn filter_by_empty_query_returns_all() {
        let items = vec![stub("foo", None, &[]), stub("bar", None, &[])];
        let r = DefaultDiscoveryClient::filter_by_query(items.clone(), Some(""));
        assert_eq!(r.len(), items.len());
    }

    #[test]
    fn derive_fallback_urls_strips_git_suffix() {
        let mut p = stub("x", None, &[]);
        p.repo_url = Some("https://github.com/lordmacu/foo.git".into());
        let urls = derive_fallback_urls(&p);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].ends_with("/lordmacu/foo/main/nexo-plugin.toml"));
        assert!(urls[1].ends_with("/lordmacu/foo/master/nexo-plugin.toml"));
    }

    #[test]
    fn derive_fallback_urls_empty_when_no_repo() {
        let mut p = stub("x", None, &[]);
        p.repo_url = None;
        assert!(derive_fallback_urls(&p).is_empty());
    }
}
