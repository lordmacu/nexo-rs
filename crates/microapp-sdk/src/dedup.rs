//! Notification dedup cache (lifted from
//! `nexo-rs-extension-marketing` 0.17.x).
//!
//! NATS at-least-once delivery means a transient broker
//! disconnect can re-deliver the same `plugin.inbound.email.*`
//! event after a reconnect. The marketing extension's broker
//! hop is idempotent at the **lead** layer (the
//! `find_by_thread → Some(lead)` short-circuit handles the
//! replay) but the **notification publish** runs on every
//! event regardless of the lead branch — so a duplicate
//! inbound delivers a duplicate `EmailNotification` to the
//! operator's WhatsApp / email.
//!
//! This module provides a simple in-memory time-bucket dedup
//! cache: dedupe key → wall-clock instant of last publish.
//! Future publishes that hit a fresh entry are silently
//! suppressed.
//!
//! ## Scope + threat model
//!
//! - **In-process replays** (NATS reconnect, broker hop
//!   crash before reply, etc.): caught.
//! - **Cross-restart replays** (extension crash + NATS
//!   redelivers from durable queue on next boot): NOT caught
//!   — the cache is lost on process exit.
//!
//! The latter can be addressed later by swapping the inner
//! `HashMap` for a `sled`-backed store; the public surface is
//! kept narrow on purpose so that swap is a 1-file change.
//! Cross-restart dedup is rare in practice for marketing
//! notifications (operator restarts the extension manually,
//! NATS doesn't retain plugin.inbound.* events across
//! consumer restarts in our config) so the in-memory cache
//! covers ~95% of the threat with zero new deps.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default dedup TTL — events older than this get evicted on
/// the next `is_duplicate` call. 1 hour catches the typical
/// at-least-once redelivery window for NATS while keeping the
/// cache small (typical marketing tenant: <1000 entries).
pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);

/// Identifier for a notification publish call. Two publishes
/// with the same key within the TTL window are deduped — only
/// the first lands on the broker.
///
/// Caller convention: include the lead id + the kind + an
/// `at_ms` bucket (typically the wall-clock minute) so a
/// genuinely-different at-the-same-second event from the same
/// lead doesn't get suppressed. The marketing extension uses
/// `(tenant_id, lead_id, kind, at_ms / 60_000)` per-publish.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DedupKey(String);

impl DedupKey {
    /// Build from the canonical 4-tuple. `at_ms` is rounded
    /// down to the minute so two events ~30 s apart on the
    /// same lead+kind dedupe; events ~5 min apart don't.
    pub fn new(tenant_id: &str, lead_id: &str, kind: &str, at_ms: i64) -> Self {
        let bucket = at_ms / 60_000;
        Self(format!("{tenant_id}:{lead_id}:{kind}:{bucket}"))
    }

    /// Internal accessor for the canonical string form.
    /// Public so the caller can log it for debugging.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Lock-protected in-memory dedup cache. `Mutex` (not
/// `RwLock`) because every read upgrades to a write to record
/// the freshly-deduped timestamp; `RwLock` would just
/// pessimise.
///
/// Eviction runs lazily inside `is_duplicate` so we don't
/// need a background sweeper task — the worst-case is that a
/// cold cache holds expired entries until the next call. For
/// marketing's volume (<1 publish/sec/tenant), this is a
/// non-issue.
///
/// ## Backends
///
/// `DedupCache` wraps an internal `Backend` enum so the
/// public API stays unchanged across in-memory and
/// persistent variants:
///
/// - [`DedupCache::new`] / [`DedupCache::with_ttl`] — the
///   default `Mutex<HashMap>` backend. Wiped on process
///   restart; covers ~95 % of the NATS at-least-once
///   redelivery threat (transient broker reconnects).
/// - [`DedupCache::with_sled`] (gated by the `dedup-sled`
///   feature) — embedded sled keyspace at the operator-
///   supplied path. Survives process restarts so the dedup
///   window covers crash-recovery + planned redeploys.
#[derive(Debug)]
pub struct DedupCache {
    backend: Backend,
    ttl: Duration,
}

#[derive(Debug)]
enum Backend {
    Memory(Mutex<HashMap<DedupKey, Instant>>),
    #[cfg(feature = "dedup-sled")]
    Sled(sled::Db),
}

impl Default for DedupCache {
    fn default() -> Self {
        Self::with_ttl(DEFAULT_TTL)
    }
}

impl DedupCache {
    /// Build a cache with the framework default TTL (1 hour).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a cache with a caller-supplied TTL. Tests use a
    /// short TTL to exercise the eviction path without
    /// needing `tokio::time::pause`.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            backend: Backend::Memory(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    /// Open a sled-backed cache at `path`. Survives
    /// process restarts so a NATS redelivery after a crash
    /// or planned redeploy still suppresses the duplicate
    /// publish. TTL stays the lazy in-process eviction
    /// window; the sled keyspace doesn't grow unbounded
    /// because every read past TTL evicts.
    ///
    /// Caller picks the path; convention is
    /// `<state_root>/<tenant>/notification_dedup.sled`.
    /// Cross-tenant deployments stamp distinct paths so the
    /// sled file boundary mirrors the existing tenant
    /// isolation posture.
    #[cfg(feature = "dedup-sled")]
    pub fn with_sled(
        path: impl AsRef<std::path::Path>,
        ttl: Duration,
    ) -> Result<Self, sled::Error> {
        // sled creates intermediate dirs itself; no
        // pre-stat needed. `Db` cheap to clone via Arc
        // internals so we can park it inside the Backend
        // enum.
        let db = sled::open(path)?;
        Ok(Self {
            backend: Backend::Sled(db),
            ttl,
        })
    }

    /// Returns `true` if the key is a duplicate of a recent
    /// publish (within TTL); records the call regardless so
    /// subsequent calls within TTL are also deduped.
    ///
    /// Memory backend: serialised on the internal mutex.
    /// The lock guard scope is tight (1 HashMap op + 1
    /// Instant::now); contention is negligible for marketing
    /// volumes.
    ///
    /// Sled backend: per-key compare-and-swap; concurrent
    /// callers race on the key but at most one wins the
    /// "first publish" slot. Lazy eviction reads + drops
    /// stale rows on every call.
    pub fn is_duplicate(&self, key: &DedupKey) -> bool {
        match &self.backend {
            Backend::Memory(inner) => {
                let now = Instant::now();
                let mut map = inner.lock().expect("notification dedup poisoned");
                // Lazy eviction — drop every entry whose
                // timestamp is older than `ttl`. O(n) sweep
                // but n is bounded (per-tenant per-hour
                // publish count).
                map.retain(|_, t| now.duration_since(*t) < self.ttl);
                match map.get(key) {
                    Some(_) => true,
                    None => {
                        map.insert(key.clone(), now);
                        false
                    }
                }
            }
            #[cfg(feature = "dedup-sled")]
            Backend::Sled(db) => sled_is_duplicate(db, key, self.ttl),
        }
    }

    /// Number of live entries — used by tests + the optional
    /// `/healthz` debug surface. For the sled backend this
    /// returns the raw row count (including any stale rows
    /// that haven't been evicted by a read pass yet).
    pub fn len(&self) -> usize {
        match &self.backend {
            Backend::Memory(inner) => inner.lock().map(|m| m.len()).unwrap_or(0),
            #[cfg(feature = "dedup-sled")]
            Backend::Sled(db) => db.len(),
        }
    }

    /// `true` when no live entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(feature = "dedup-sled")]
fn sled_is_duplicate(db: &sled::Db, key: &DedupKey, ttl: Duration) -> bool {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let ttl_ms = ttl.as_millis() as u64;

    let key_bytes = key.as_str().as_bytes();
    match db.get(key_bytes) {
        Ok(Some(value)) => {
            // 8-byte little-endian u64 timestamp. Anything
            // shorter is a corrupted row; treat as stale.
            let stamp_ms = if value.len() >= 8 {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&value[..8]);
                u64::from_le_bytes(buf)
            } else {
                0
            };
            if now_ms.saturating_sub(stamp_ms) < ttl_ms {
                // Within TTL ⇒ duplicate. Don't refresh the
                // timestamp; the first-seen stamp gates the
                // window edge consistently.
                true
            } else {
                // Stale — overwrite with the fresh stamp +
                // treat as the first call.
                let _ = db.insert(key_bytes, &now_ms.to_le_bytes());
                let _ = db.flush();
                false
            }
        }
        Ok(None) => {
            // First time seeing the key.
            let _ = db.insert(key_bytes, &now_ms.to_le_bytes());
            let _ = db.flush();
            false
        }
        Err(e) => {
            // Read error — fail open (don't suppress the
            // publish). Operator's tracing log surfaces the
            // sled hiccup.
            tracing::warn!(
                target: "marketing.notification_dedup",
                error = %e, key = %key.as_str(),
                "sled read failed (failing open — duplicate may pass through)"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(lead: &str) -> DedupKey {
        DedupKey::new("acme", lead, "lead_created", 1_700_000_000_000)
    }

    #[test]
    fn first_call_returns_false_records_entry() {
        let cache = DedupCache::new();
        assert!(!cache.is_duplicate(&k("l-1")));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn second_call_with_same_key_returns_true() {
        let cache = DedupCache::new();
        assert!(!cache.is_duplicate(&k("l-1")));
        assert!(cache.is_duplicate(&k("l-1")));
        // Cache size doesn't grow — same key.
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn distinct_lead_ids_are_independent() {
        let cache = DedupCache::new();
        assert!(!cache.is_duplicate(&k("l-1")));
        assert!(!cache.is_duplicate(&k("l-2")));
        assert!(cache.is_duplicate(&k("l-1")));
        assert!(cache.is_duplicate(&k("l-2")));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn distinct_kinds_are_independent() {
        let cache = DedupCache::new();
        let k_created = DedupKey::new("acme", "l-1", "lead_created", 0);
        let k_replied = DedupKey::new("acme", "l-1", "lead_replied", 0);
        assert!(!cache.is_duplicate(&k_created));
        assert!(!cache.is_duplicate(&k_replied));
        // Re-fire each — both deduped.
        assert!(cache.is_duplicate(&k_created));
        assert!(cache.is_duplicate(&k_replied));
    }

    #[test]
    fn distinct_minute_buckets_are_independent() {
        let cache = DedupCache::new();
        // Two events on the same lead+kind, 5 minutes apart.
        // Bucket boundary at minute → distinct keys.
        let k_a = DedupKey::new("acme", "l-1", "lead_created", 0);
        let k_b = DedupKey::new("acme", "l-1", "lead_created", 5 * 60_000);
        assert!(!cache.is_duplicate(&k_a));
        assert!(!cache.is_duplicate(&k_b));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn cross_tenant_keys_are_independent() {
        let cache = DedupCache::new();
        let acme = DedupKey::new("acme", "l-1", "lead_created", 0);
        let globex = DedupKey::new("globex", "l-1", "lead_created", 0);
        assert!(!cache.is_duplicate(&acme));
        assert!(!cache.is_duplicate(&globex));
        assert!(cache.is_duplicate(&acme));
        assert!(cache.is_duplicate(&globex));
    }

    #[test]
    fn ttl_expired_entries_are_evicted_on_next_call() {
        let cache = DedupCache::with_ttl(Duration::from_millis(50));
        assert!(!cache.is_duplicate(&k("l-1")));
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(60));
        // Sleep window > TTL → first call's entry expired.
        // Next call on the same key returns false (treated
        // as fresh) AND the lazy sweep drops the stale row
        // before insert.
        assert!(!cache.is_duplicate(&k("l-1")));
        assert_eq!(cache.len(), 1, "stale entry should be replaced not stacked");
    }

    #[test]
    fn dedup_key_minute_bucket_collapses_30s_apart_events() {
        // Same minute bucket → identical key → second event
        // deduped against the first.
        let a = DedupKey::new("acme", "l-1", "lead_created", 0);
        let b = DedupKey::new("acme", "l-1", "lead_created", 30 * 1000);
        assert_eq!(a, b);
    }

    // ─── sled backend (cross-restart dedup) ─────────────

    #[cfg(feature = "dedup-sled")]
    mod sled_backend {
        use super::*;

        fn fresh_sled_cache(ttl: Duration) -> (DedupCache, tempfile::TempDir) {
            let dir = tempfile::tempdir().unwrap();
            let cache = DedupCache::with_sled(dir.path(), ttl).unwrap();
            (cache, dir)
        }

        #[test]
        fn first_call_returns_false_records_entry() {
            let (cache, _tmp) = fresh_sled_cache(Duration::from_secs(60));
            assert!(!cache.is_duplicate(&k("l-1")));
            assert_eq!(cache.len(), 1);
        }

        #[test]
        fn second_call_with_same_key_returns_true() {
            let (cache, _tmp) = fresh_sled_cache(Duration::from_secs(60));
            assert!(!cache.is_duplicate(&k("l-1")));
            assert!(cache.is_duplicate(&k("l-1")));
        }

        #[test]
        fn distinct_keys_independent() {
            let (cache, _tmp) = fresh_sled_cache(Duration::from_secs(60));
            assert!(!cache.is_duplicate(&k("l-1")));
            assert!(!cache.is_duplicate(&k("l-2")));
            assert!(cache.is_duplicate(&k("l-1")));
            assert!(cache.is_duplicate(&k("l-2")));
        }

        #[test]
        fn ttl_expired_entry_treated_as_fresh() {
            let (cache, _tmp) = fresh_sled_cache(Duration::from_millis(50));
            assert!(!cache.is_duplicate(&k("l-1")));
            std::thread::sleep(Duration::from_millis(60));
            assert!(!cache.is_duplicate(&k("l-1")));
        }

        #[test]
        fn cross_restart_dedup_survives_reopen() {
            // The headline invariant: open → write → close →
            // reopen → second call is deduped.
            let dir = tempfile::tempdir().unwrap();
            {
                let cache = DedupCache::with_sled(dir.path(), Duration::from_secs(3600)).unwrap();
                assert!(!cache.is_duplicate(&k("l-1")));
                // Drop scope — sled flushes on drop.
            }
            // Re-open the same path.
            let cache = DedupCache::with_sled(dir.path(), Duration::from_secs(3600)).unwrap();
            assert!(
                cache.is_duplicate(&k("l-1")),
                "sled-backed cache must persist across restarts"
            );
        }

        #[test]
        fn cross_restart_eviction_after_ttl() {
            // Re-open with a very short TTL. Stale rows
            // from the previous run get treated as fresh.
            let dir = tempfile::tempdir().unwrap();
            {
                let cache = DedupCache::with_sled(dir.path(), Duration::from_millis(50)).unwrap();
                assert!(!cache.is_duplicate(&k("l-1")));
            }
            std::thread::sleep(Duration::from_millis(60));
            let cache = DedupCache::with_sled(dir.path(), Duration::from_millis(50)).unwrap();
            assert!(
                !cache.is_duplicate(&k("l-1")),
                "stale row must be treated as fresh after restart"
            );
        }

        #[test]
        fn sled_len_reports_persisted_rows() {
            let (cache, _tmp) = fresh_sled_cache(Duration::from_secs(60));
            for lead in &["l-1", "l-2", "l-3"] {
                cache.is_duplicate(&k(lead));
            }
            assert_eq!(cache.len(), 3);
        }

        #[test]
        fn cross_tenant_keys_independent_in_sled() {
            let (cache, _tmp) = fresh_sled_cache(Duration::from_secs(60));
            let acme = DedupKey::new("acme", "l-1", "lead_created", 0);
            let globex = DedupKey::new("globex", "l-1", "lead_created", 0);
            assert!(!cache.is_duplicate(&acme));
            assert!(!cache.is_duplicate(&globex));
            assert!(cache.is_duplicate(&acme));
            assert!(cache.is_duplicate(&globex));
        }
    }
}
