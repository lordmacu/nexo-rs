//! Token-bucket rate limiter keyed on `(source_id, client_ip)`.
//!
//! Reuses the same algorithm as Phase 80.9.g `TokenBucket` in
//! `nexo-mcp::channel` — single source of truth across the
//! framework's rate-limit surface (channels + webhooks). Lazy
//! refill on each call keeps the data structure free of background
//! tickers.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use nexo_config::types::channels::ChannelRateLimit;

/// Shared key — pairs the source id with the resolved client IP
/// so a noisy client only burns its own bucket, not the whole
/// source's budget.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct ClientBucketKey {
    /// Webhook source id (`WebhookSourceConfig.id`).
    pub source_id: String,
    /// Resolved client IP (after trusted-proxy logic).
    pub client_ip: IpAddr,
}

/// One token-bucket — lazy-refill per-call, no background ticker.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    /// `Mutex` over the `(tokens, last_refill)` pair. Lazy refill
    /// happens inside `try_acquire`.
    state: Mutex<BucketState>,
}

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Build a fresh bucket pre-filled to `rl.burst`.
    pub fn new(rl: &ChannelRateLimit) -> Self {
        Self {
            capacity: rl.burst as f64,
            refill_per_sec: rl.rps,
            state: Mutex::new(BucketState {
                tokens: rl.burst as f64,
                last_refill: Instant::now(),
            }),
        }
    }

    /// `true` when one token was available and consumed; `false`
    /// when the bucket is empty (caller maps to 429).
    pub fn try_acquire(&self) -> bool {
        if self.refill_per_sec <= 0.0 || self.capacity <= 0.0 {
            // Disabled / inactive — `is_active() == false`. Treat
            // as unlimited (the caller is expected to skip
            // construction in that case).
            return true;
        }
        let mut s = self.state.lock().expect("bucket mutex");
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(s.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            s.tokens = (s.tokens + elapsed * self.refill_per_sec).min(self.capacity);
            s.last_refill = now;
        }
        if s.tokens >= 1.0 {
            s.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Per-key bucket map — buckets are constructed on-demand the
/// first time a `(source_id, client_ip)` combination is seen.
/// `MAX_KEYS` caps the map so a flood from random IPs can't
/// exhaust memory; once the cap is hit, the oldest entry is
/// evicted.
pub struct ClientBucketMap {
    rl: ChannelRateLimit,
    buckets: Mutex<HashMap<ClientBucketKey, TokenBucket>>,
    max_keys: usize,
}

const DEFAULT_MAX_KEYS: usize = 4096;

impl ClientBucketMap {
    /// Build a fresh map with the default 4096-key cap.
    pub fn new(rl: ChannelRateLimit) -> Self {
        Self {
            rl,
            buckets: Mutex::new(HashMap::new()),
            max_keys: DEFAULT_MAX_KEYS,
        }
    }

    /// Override the per-source key cap. Useful for tests.
    pub fn with_max_keys(mut self, n: usize) -> Self {
        self.max_keys = n.max(1);
        self
    }

    /// Borrow the rate-limit configuration the map was built with.
    pub fn rate_limit(&self) -> &ChannelRateLimit {
        &self.rl
    }

    /// Current number of distinct keys observed.
    pub fn len(&self) -> usize {
        self.buckets.lock().expect("buckets").len()
    }

    /// `true` when no keys have been seen.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Acquire one token for the given key. Returns `true` on
    /// success, `false` when the bucket is empty.
    pub fn try_acquire(&self, key: &ClientBucketKey) -> bool {
        if !self.rl.is_active() {
            return true;
        }
        let mut buckets = self.buckets.lock().expect("buckets");
        if !buckets.contains_key(key) && buckets.len() >= self.max_keys {
            // Defensive eviction — drop a single arbitrary entry.
            // HashMap iteration order is randomised; the eviction
            // is not LRU-strict but bounded memory wins. Documented
            // limit; flood resilience is the priority.
            if let Some(victim) = buckets.keys().next().cloned() {
                buckets.remove(&victim);
            }
        }
        let bucket = buckets
            .entry(key.clone())
            .or_insert_with(|| TokenBucket::new(&self.rl));
        bucket.try_acquire()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn inactive_rate_limit_always_allows() {
        let rl = ChannelRateLimit {
            rps: 0.0,
            burst: 0,
        };
        let map = ClientBucketMap::new(rl);
        let key = ClientBucketKey {
            source_id: "x".into(),
            client_ip: ip("1.1.1.1"),
        };
        for _ in 0..1000 {
            assert!(map.try_acquire(&key));
        }
    }

    #[test]
    fn burst_capacity_drains_then_refuses() {
        let rl = ChannelRateLimit {
            rps: 0.0001,
            burst: 3,
        };
        let map = ClientBucketMap::new(rl);
        let key = ClientBucketKey {
            source_id: "x".into(),
            client_ip: ip("1.1.1.1"),
        };
        assert!(map.try_acquire(&key));
        assert!(map.try_acquire(&key));
        assert!(map.try_acquire(&key));
        // Bucket empty.
        assert!(!map.try_acquire(&key));
    }

    #[test]
    fn separate_keys_get_separate_buckets() {
        let rl = ChannelRateLimit {
            rps: 0.0001,
            burst: 1,
        };
        let map = ClientBucketMap::new(rl);
        let a = ClientBucketKey {
            source_id: "x".into(),
            client_ip: ip("1.1.1.1"),
        };
        let b = ClientBucketKey {
            source_id: "x".into(),
            client_ip: ip("2.2.2.2"),
        };
        assert!(map.try_acquire(&a));
        assert!(map.try_acquire(&b));
        // Each key empty independently.
        assert!(!map.try_acquire(&a));
        assert!(!map.try_acquire(&b));
    }

    #[test]
    fn max_keys_cap_evicts_when_full() {
        let rl = ChannelRateLimit {
            rps: 0.0001,
            burst: 1,
        };
        let map = ClientBucketMap::new(rl).with_max_keys(2);
        let a = ClientBucketKey {
            source_id: "x".into(),
            client_ip: ip("1.1.1.1"),
        };
        let b = ClientBucketKey {
            source_id: "x".into(),
            client_ip: ip("2.2.2.2"),
        };
        let c = ClientBucketKey {
            source_id: "x".into(),
            client_ip: ip("3.3.3.3"),
        };
        assert!(map.try_acquire(&a));
        assert!(map.try_acquire(&b));
        assert_eq!(map.len(), 2);
        // c forces eviction.
        assert!(map.try_acquire(&c));
        assert_eq!(map.len(), 2);
    }
}
