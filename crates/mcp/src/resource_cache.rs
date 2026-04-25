//! Phase 12.5 follow-up — LRU+TTL cache for `resources/read` results.
//!
//! Immutable resources (manifests, READMEs, configs) pay a roundtrip
//! per read. This cache memoizes the raw `Vec<McpResourceContent>`
//! returned by `McpClient::read_resource*` keyed by `(server, uri)` with
//! a short TTL to bound staleness. Invalidation on
//! `notifications/resources/list_changed` is handled externally by
//! `invalidate_server(name)`.

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lru::LruCache;

use crate::types::McpResourceContent;

#[derive(Debug, Clone, Copy)]
pub struct ResourceCacheConfig {
    pub enabled: bool,
    pub ttl: Duration,
    pub max_entries: usize,
}

impl Default for ResourceCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl: Duration::from_secs(30),
            max_entries: 256,
        }
    }
}

struct Entry {
    inserted_at: Instant,
    contents: Vec<McpResourceContent>,
}

pub struct ResourceCache {
    ttl: Duration,
    enabled: bool,
    inner: Mutex<LruCache<(String, String), Entry>>,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

impl ResourceCache {
    pub fn new(cfg: ResourceCacheConfig) -> Self {
        let cap = NonZeroUsize::new(cfg.max_entries.max(1)).unwrap();
        Self {
            ttl: cfg.ttl,
            enabled: cfg.enabled,
            inner: Mutex::new(LruCache::new(cap)),
            hits: std::sync::atomic::AtomicU64::new(0),
            misses: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn disabled() -> Self {
        Self::new(ResourceCacheConfig::default())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn get(&self, server: &str, uri: &str) -> Option<Vec<McpResourceContent>> {
        if !self.enabled {
            return None;
        }
        let mut guard = self.inner.lock().ok()?;
        let key = (server.to_string(), uri.to_string());
        let fresh = match guard.peek(&key) {
            Some(entry) => entry.inserted_at.elapsed() < self.ttl,
            None => false,
        };
        if !fresh {
            guard.pop(&key);
            self.misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        }
        let hit = guard.get(&key).map(|e| e.contents.clone());
        if hit.is_some() {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        hit
    }

    pub fn put(&self, server: &str, uri: &str, contents: Vec<McpResourceContent>) {
        if !self.enabled {
            return;
        }
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.put(
            (server.to_string(), uri.to_string()),
            Entry {
                inserted_at: Instant::now(),
                contents,
            },
        );
    }

    /// Drop every entry whose key matches `server`. Called when
    /// `notifications/resources/list_changed` fires for that server.
    pub fn invalidate_server(&self, server: &str) -> usize {
        let Ok(mut guard) = self.inner.lock() else {
            return 0;
        };
        let victims: Vec<(String, String)> = guard
            .iter()
            .filter(|(k, _)| k.0 == server)
            .map(|(k, _)| k.clone())
            .collect();
        let n = victims.len();
        for k in victims {
            guard.pop(&k);
        }
        n
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(uri: &str, text: &str) -> Vec<McpResourceContent> {
        vec![McpResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/plain".into()),
            text: Some(text.to_string()),
            blob: None,
        }]
    }

    #[test]
    fn disabled_cache_never_stores() {
        let c = ResourceCache::disabled();
        c.put("s", "u", sample("u", "x"));
        assert!(c.get("s", "u").is_none());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn enabled_put_then_get_hits() {
        let c = ResourceCache::new(ResourceCacheConfig {
            enabled: true,
            ttl: Duration::from_secs(60),
            max_entries: 8,
        });
        c.put("s", "u", sample("u", "hello"));
        let got = c.get("s", "u").expect("hit");
        assert_eq!(got[0].text.as_deref(), Some("hello"));
        assert_eq!(c.hits(), 1);
        assert_eq!(c.misses(), 0);
    }

    #[test]
    fn miss_increments_counter() {
        let c = ResourceCache::new(ResourceCacheConfig {
            enabled: true,
            ttl: Duration::from_secs(60),
            max_entries: 8,
        });
        assert!(c.get("s", "u").is_none());
        assert_eq!(c.misses(), 1);
    }

    #[test]
    fn expired_entry_returns_miss() {
        let c = ResourceCache::new(ResourceCacheConfig {
            enabled: true,
            ttl: Duration::from_millis(10),
            max_entries: 8,
        });
        c.put("s", "u", sample("u", "x"));
        std::thread::sleep(Duration::from_millis(30));
        assert!(c.get("s", "u").is_none());
        assert_eq!(c.misses(), 1);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn invalidate_server_drops_matching_entries() {
        let c = ResourceCache::new(ResourceCacheConfig {
            enabled: true,
            ttl: Duration::from_secs(60),
            max_entries: 8,
        });
        c.put("a", "u1", sample("u1", "x"));
        c.put("a", "u2", sample("u2", "y"));
        c.put("b", "u3", sample("u3", "z"));
        let n = c.invalidate_server("a");
        assert_eq!(n, 2);
        assert!(c.get("a", "u1").is_none());
        assert!(c.get("b", "u3").is_some());
    }

    #[test]
    fn lru_evicts_oldest_when_capacity_hit() {
        let c = ResourceCache::new(ResourceCacheConfig {
            enabled: true,
            ttl: Duration::from_secs(60),
            max_entries: 2,
        });
        c.put("s", "u1", sample("u1", "a"));
        c.put("s", "u2", sample("u2", "b"));
        c.put("s", "u3", sample("u3", "c"));
        assert!(c.get("s", "u1").is_none());
        assert!(c.get("s", "u2").is_some());
        assert!(c.get("s", "u3").is_some());
    }
}
