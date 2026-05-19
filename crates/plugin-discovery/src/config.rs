//! Runtime configuration for [`crate`]. Baked-in defaults match the
//! "first-party Nexo" deployment shape — operators override per-knob
//! via daemon main.rs.

use std::path::PathBuf;
use std::time::Duration;

/// Default crates.io REST API root. Overridable for tests + air-
/// gapped mirrors.
pub const DEFAULT_CRATES_IO_ENDPOINT: &str = "https://crates.io";

/// Default GitHub REST API root.
pub const DEFAULT_GITHUB_ENDPOINT: &str = "https://api.github.com";

/// Default curated index location. Repo seeded by Phase 98.16.
pub const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/lordmacu/nexo-plugin-index/main/index.json";

/// Cache TTL — 24 hours. Source rate-limits (crates.io 100/min,
/// GitHub unauth 10/min) make a longer-than-an-hour TTL essential
/// for SaaS workloads where the operator hits Available tab often.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

/// Per-source HTTP timeout. Long-tail of 10s covers GitHub's
/// occasional 5s+ latency on `/search/repositories` without making
/// the admin UI feel hung; the parallel runner aggregates around
/// this so one slow source can't stall the others.
pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// First-party owners. Drives the `TrustTier::Official` badge.
/// Allowlist seeded from `TrustedKeysConfig.authors` at boot in
/// daemon main.rs — these baked-in values are the seed used by the
/// crate's standalone test/dev path.
pub const DEFAULT_OFFICIAL_OWNERS: &[&str] = &["lordmacu", "nexo-rs"];

/// Configuration knob set passed to the discovery client at
/// construction. Every field has a sensible default so the daemon
/// only overrides what it knows differently (state_dir + sdk_version).
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// Directory the disk cache lives under (`<state_dir>/
    /// plugin-discovery/`). Created on first write.
    pub state_dir: PathBuf,
    /// How long a cache entry counts as fresh.
    pub cache_ttl: Duration,
    /// crates.io REST root.
    pub crates_io_endpoint: String,
    /// GitHub REST root.
    pub github_endpoint: String,
    /// Curated index `index.json` URL.
    pub index_url: String,
    /// Per-source HTTP timeout.
    pub http_timeout: Duration,
    /// Owner allowlist for `TrustTier::Official`.
    pub official_owners: Vec<String>,
    /// Daemon's running version (matches `CARGO_PKG_VERSION` of the
    /// `nexo-rs` binary, parsed at boot). Required — the compat gate
    /// in `crate::compat` compares fetched manifests'
    /// `[plugin] min_nexo_version` (`semver::VersionReq`) against
    /// this. Older daemons therefore see `NeedsUpgrade` on plugins
    /// that require a newer host.
    pub daemon_version: semver::Version,
    /// Optional GitHub PAT for the topic-search source. Lifts the
    /// 10-req/min unauth ceiling to 5000/hr. `None` → unauth mode.
    pub github_token: Option<String>,
}

impl DiscoveryConfig {
    /// Build a config with the bake-in defaults and the two
    /// required overrides (state_dir + daemon_version).
    pub fn with_defaults(state_dir: PathBuf, daemon_version: semver::Version) -> Self {
        Self {
            state_dir,
            cache_ttl: DEFAULT_CACHE_TTL,
            crates_io_endpoint: DEFAULT_CRATES_IO_ENDPOINT.into(),
            github_endpoint: DEFAULT_GITHUB_ENDPOINT.into(),
            index_url: DEFAULT_INDEX_URL.into(),
            http_timeout: DEFAULT_HTTP_TIMEOUT,
            official_owners: DEFAULT_OFFICIAL_OWNERS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            daemon_version,
            github_token: None,
        }
    }

    /// Disk cache file path. Centralized so tests can predict it.
    pub fn cache_file_path(&self) -> PathBuf {
        self.state_dir
            .join("plugin-discovery")
            .join("catalogue.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DiscoveryConfig {
        DiscoveryConfig::with_defaults(
            PathBuf::from("/tmp/nexo-state"),
            semver::Version::parse("0.2.0").unwrap(),
        )
    }

    #[test]
    fn defaults_match_baked_constants() {
        let c = cfg();
        assert_eq!(c.cache_ttl, DEFAULT_CACHE_TTL);
        assert_eq!(c.crates_io_endpoint, DEFAULT_CRATES_IO_ENDPOINT);
        assert_eq!(c.github_endpoint, DEFAULT_GITHUB_ENDPOINT);
        assert_eq!(c.index_url, DEFAULT_INDEX_URL);
        assert_eq!(c.http_timeout, DEFAULT_HTTP_TIMEOUT);
        assert!(c.github_token.is_none());
    }

    #[test]
    fn official_owners_seeded_from_constant() {
        let c = cfg();
        assert!(c.official_owners.iter().any(|o| o == "lordmacu"));
        assert!(c.official_owners.iter().any(|o| o == "nexo-rs"));
    }

    #[test]
    fn cache_file_path_lives_under_plugin_discovery_subdir() {
        let c = cfg();
        let p = c.cache_file_path();
        assert!(
            p.ends_with("plugin-discovery/catalogue.json"),
            "unexpected cache path: {}",
            p.display()
        );
    }
}
