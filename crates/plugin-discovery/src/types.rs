//! Local re-exports of the `nexo_tool_meta::admin::plugin_discovery`
//! wire shapes plus internal construction helpers.
//!
//! The wire crate is the single source of truth for the JSON
//! payloads; this module just gives the discovery client ergonomic
//! aliases + a `cached_at_ms` helper used by [`crate::cache`].

pub use nexo_tool_meta::admin::plugin_discovery::{
    CompatStatus, DiscoveredPlugin, ManifestSummary, PluginCategory, PluginSource, SourceError,
    TrustTier,
};

/// Snapshot of the merged catalogue persisted to disk. Wraps the
/// wire-level `Vec<DiscoveredPlugin>` plus the fetch timestamp so
/// the cache layer can decide TTL freshness without re-parsing each
/// entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CachedCatalogue {
    /// Unix milliseconds when the catalogue was assembled. Drives
    /// the TTL check in [`crate::cache::DiskCache::read_fresh`].
    pub fetched_at_ms: u64,
    /// Merged catalogue rows. Empty when every source failed at
    /// fetch time; partial failures surface separately on the
    /// admin RPC response, never on disk.
    pub items: Vec<DiscoveredPlugin>,
}

impl CachedCatalogue {
    /// True when the snapshot's `fetched_at_ms` is older than
    /// `ttl_ms` relative to `now_ms`. Used by the cache reader to
    /// decide whether to serve disk or fall through to network
    /// fetch.
    pub fn is_stale(&self, now_ms: u64, ttl_ms: u64) -> bool {
        now_ms.saturating_sub(self.fetched_at_ms) >= ttl_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_catalogue_is_stale_after_ttl_window() {
        let snap = CachedCatalogue {
            fetched_at_ms: 1_000,
            items: vec![],
        };
        // Equal-to TTL boundary is stale (`>=`).
        assert!(snap.is_stale(1_000 + 100, 100));
        // Inside TTL is fresh.
        assert!(!snap.is_stale(1_000 + 99, 100));
        // Clock skew (now < fetched_at) is treated as fresh via
        // `saturating_sub` — better than panicking on bad clocks.
        assert!(!snap.is_stale(500, 100));
    }
}
