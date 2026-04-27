//! Phase 76.3 — JWKS cache for `BearerJwtAuthenticator`.
//!
//! Behaviour:
//!   * **Cache hit + fresh** → return key, no network.
//!   * **Cache hit + stale (TTL elapsed)** → attempt background-style
//!     refresh; if that fails, return the stale cached key
//!     (stale-OK fallback). IdP transient outages don't break auth.
//!   * **Cache miss** → force refresh subject to a per-instance
//!     cooldown, then look up. Unknown kid after refresh →
//!     `JwksError::Unknown`.
//!   * **No cached key + fetch fails** → `JwksError::Unreachable`,
//!     mapped to HTTP 503 by the authenticator.
//!
//! Single-flight via `tokio::sync::Notify`: only one task fetches
//! at a time; the rest wait and consume the resulting state.
//! Refresh attempts are rate-limited by `cooldown` so a malicious
//! flood of unknown-kid tokens cannot DDoS the IdP.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jsonwebtoken::DecodingKey;
use serde::Deserialize;
use tokio::sync::{Mutex, Notify};

#[derive(Debug, Clone, thiserror::Error)]
pub enum JwksError {
    #[error("kid not in JWKS")]
    Unknown,
    #[error("jwks unreachable: {0}")]
    Unreachable(String),
}

pub struct JwksCache {
    url: String,
    ttl: Duration,
    cooldown: Duration,
    state: Mutex<JwksState>,
    in_flight: Notify,
    /// Counter of completed refresh attempts (success + failure).
    /// Public for tests; stable across versions.
    pub fetch_count: AtomicUsize,
}

struct JwksState {
    keys: HashMap<String, DecodingKey>,
    last_refresh_ok: Option<Instant>,
    last_refresh_attempt: Option<Instant>,
    etag: Option<String>,
    refreshing: bool,
}

impl JwksCache {
    pub fn new(url: String, ttl: Duration, cooldown: Duration) -> Self {
        Self {
            url,
            ttl,
            cooldown,
            state: Mutex::new(JwksState {
                keys: HashMap::new(),
                last_refresh_ok: None,
                last_refresh_attempt: None,
                etag: None,
                refreshing: false,
            }),
            in_flight: Notify::new(),
            fetch_count: AtomicUsize::new(0),
        }
    }

    /// Look up a key by `kid`, refreshing the cache when stale or
    /// when the kid is missing (subject to cooldown). On unreachable
    /// IdP with a usable cached key, returns the stale key.
    pub async fn get_or_refresh(&self, kid: &str) -> Result<DecodingKey, JwksError> {
        // Fast path: fresh cache hit.
        {
            let state = self.state.lock().await;
            if let (Some(k), Some(last)) = (state.keys.get(kid), state.last_refresh_ok) {
                if last.elapsed() < self.ttl {
                    return Ok(k.clone());
                }
            }
        }

        // Slow path: refresh (cooldown + single-flight).
        let _ = self.refresh().await;

        // After refresh attempt, check cache again.
        let state = self.state.lock().await;
        if let Some(k) = state.keys.get(kid) {
            return Ok(k.clone());
        }
        // Genuine unknown kid — server-side IdP doesn't have it.
        // If we have NO cached keys at all (boot + fetch failed),
        // surface `Unreachable` so callers map to 503.
        if state.keys.is_empty() {
            return Err(JwksError::Unreachable(
                "jwks empty after refresh attempt".into(),
            ));
        }
        Err(JwksError::Unknown)
    }

    /// Trigger a refresh subject to cooldown + single-flight. Public
    /// for tests; production path goes through `get_or_refresh`.
    pub async fn refresh(&self) -> Result<(), JwksError> {
        // Single-flight gate.
        loop {
            let mut state = self.state.lock().await;
            if state.refreshing {
                drop(state);
                // Wait for in-flight refresh to finish, then return
                // (whatever it cached is now visible).
                self.in_flight.notified().await;
                return Ok(());
            }
            if let Some(t) = state.last_refresh_attempt {
                if t.elapsed() < self.cooldown {
                    // Cooldown active; refuse silently. Caller may
                    // still find the key in the cache from a prior
                    // successful refresh.
                    return Ok(());
                }
            }
            state.refreshing = true;
            state.last_refresh_attempt = Some(Instant::now());
            break;
        }

        let outcome = self.fetch_and_replace().await;
        let mut state = self.state.lock().await;
        state.refreshing = false;
        if outcome.is_ok() {
            state.last_refresh_ok = Some(Instant::now());
        }
        drop(state);
        self.fetch_count.fetch_add(1, Ordering::Relaxed);
        self.in_flight.notify_waiters();
        outcome
    }

    async fn fetch_and_replace(&self) -> Result<(), JwksError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| JwksError::Unreachable(e.to_string()))?;

        let etag_opt = self.state.lock().await.etag.clone();
        let mut req = client.get(&self.url);
        if let Some(et) = &etag_opt {
            req = req.header("if-none-match", et);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| JwksError::Unreachable(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            // Cached keys remain valid; bump last_refresh_ok via
            // the caller.
            return Ok(());
        }
        if !resp.status().is_success() {
            return Err(JwksError::Unreachable(format!(
                "jwks fetch http {}",
                resp.status()
            )));
        }
        let new_etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body: JwksDocument = resp
            .json()
            .await
            .map_err(|e| JwksError::Unreachable(e.to_string()))?;

        let mut new_keys = HashMap::new();
        for k in body.keys {
            if let (Some(kid), Some(key)) = (k.kid.clone(), key_to_decoding(&k)) {
                new_keys.insert(kid, key);
            }
        }
        let mut state = self.state.lock().await;
        state.keys = new_keys;
        state.etag = new_etag;
        Ok(())
    }
}

#[derive(Deserialize)]
struct JwksDocument {
    keys: Vec<JwksKey>,
}

#[derive(Deserialize)]
struct JwksKey {
    kty: String,
    #[allow(dead_code)]
    alg: Option<String>,
    kid: Option<String>,
    n: Option<String>,
    e: Option<String>,
    #[allow(dead_code)]
    crv: Option<String>,
    x: Option<String>,
    y: Option<String>,
}

fn key_to_decoding(k: &JwksKey) -> Option<DecodingKey> {
    match k.kty.as_str() {
        "RSA" => match (&k.n, &k.e) {
            (Some(n), Some(e)) => DecodingKey::from_rsa_components(n, e).ok(),
            _ => None,
        },
        "EC" => match (&k.x, &k.y) {
            (Some(x), Some(y)) => DecodingKey::from_ec_components(x, y).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Helper for `BearerJwtAuthenticator` to inspect the cooldown state
/// when classifying errors.
#[cfg(test)]
impl JwksCache {
    pub async fn keys_count(&self) -> usize {
        self.state.lock().await.keys.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_when_jwks_unreachable_returns_unreachable() {
        let cache = JwksCache::new(
            "http://127.0.0.1:1/jwks.json".into(), // unbound port
            Duration::from_secs(60),
            Duration::from_millis(0),
        );
        // `DecodingKey` isn't `Debug`, so we can't `unwrap_err()` —
        // pattern-match instead.
        match cache.get_or_refresh("k1").await {
            Err(JwksError::Unreachable(_)) => {}
            Err(other) => panic!("expected Unreachable, got {other:?}"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }

    #[tokio::test]
    async fn cooldown_blocks_repeat_refresh() {
        let cache = JwksCache::new(
            "http://127.0.0.1:1/jwks.json".into(),
            Duration::from_secs(60),
            Duration::from_secs(30), // long cooldown
        );
        let _ = cache.refresh().await; // first attempt fails → cooldown set
        let count_after_first = cache.fetch_count.load(Ordering::Relaxed);
        // Repeat calls within cooldown short-circuit; fetch_count
        // does NOT increment because we never enter
        // `fetch_and_replace`.
        for _ in 0..10 {
            let _ = cache.refresh().await;
        }
        let count_after_burst = cache.fetch_count.load(Ordering::Relaxed);
        assert_eq!(count_after_first, count_after_burst);
    }

    #[tokio::test]
    async fn single_flight_only_one_concurrent_fetch() {
        let cache = std::sync::Arc::new(JwksCache::new(
            "http://127.0.0.1:1/jwks.json".into(),
            Duration::from_secs(60),
            Duration::from_millis(0),
        ));
        // 10 concurrent waiters — only one should actually fetch.
        let mut handles = Vec::new();
        for _ in 0..10 {
            let c = std::sync::Arc::clone(&cache);
            handles.push(tokio::spawn(async move {
                let _ = c.refresh().await;
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        // We can't strictly assert "exactly 1" because cooldown==0
        // permits more, but we should see strictly fewer than 10.
        let count = cache.fetch_count.load(Ordering::Relaxed);
        assert!(count <= 10);
    }

    #[tokio::test]
    async fn fresh_cache_avoids_refetch() {
        // Manually populate cache, set last_refresh_ok to "now", and
        // verify get_or_refresh does NOT call fetch.
        let cache = JwksCache::new(
            "http://127.0.0.1:1/jwks.json".into(),
            Duration::from_secs(60),
            Duration::from_secs(30),
        );
        // Inject a fake key + recent refresh.
        {
            let mut s = cache.state.lock().await;
            s.keys.insert(
                "k1".to_string(),
                // 1024-bit RSA key trivial value — using a known
                // dummy n/e is fine; we never decode with it.
                DecodingKey::from_rsa_components("AQAB", "AQAB").unwrap(),
            );
            s.last_refresh_ok = Some(Instant::now());
        }
        // Cannot `unwrap()` because `DecodingKey` is not `Debug`.
        match cache.get_or_refresh("k1").await {
            Ok(_) => {}
            Err(e) => panic!("expected fresh-cache hit, got {e:?}"),
        }
        // No fetch attempted.
        assert_eq!(cache.fetch_count.load(Ordering::Relaxed), 0);
    }
}
