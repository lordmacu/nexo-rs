//! Gate for `sampling/createMessage` before it reaches the LLM.
//!
//! Policy checks:
//! 1. Global `enabled` flag. When false, every request is denied.
//! 2. `deny_servers` list — hard reject per server id.
//! 3. Rate limit per server (token-bucket, refills one token per
//!    `60_000 / rate_limit_per_minute` ms).
//! 4. Token cap: `request.max_tokens` is truncated to
//!    `min(per_server_cap, global_cap)` (not rejected).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use dashmap::DashMap;

use super::types::SamplingRequest;
use super::SamplingError;

#[derive(Debug, Clone)]
pub struct PerServerPolicy {
    pub enabled: bool,
    pub rate_limit_per_minute: u32,
    pub max_tokens_cap: u32,
}

impl Default for PerServerPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            rate_limit_per_minute: 10,
            max_tokens_cap: 4096,
        }
    }
}

pub struct SamplingPolicy {
    enabled: bool,
    deny_servers: HashSet<String>,
    global_cap: u32,
    per_server: HashMap<String, PerServerPolicy>,
    buckets: DashMap<String, Mutex<TokenBucket>>,
}

impl SamplingPolicy {
    pub fn new(
        enabled: bool,
        deny_servers: Vec<String>,
        global_cap: u32,
        per_server: HashMap<String, PerServerPolicy>,
    ) -> Self {
        Self {
            enabled,
            deny_servers: deny_servers.into_iter().collect(),
            global_cap,
            per_server,
            buckets: DashMap::new(),
        }
    }

    pub fn permissive_for_tests() -> Self {
        Self::new(true, vec![], 4096, HashMap::new())
    }

    pub fn check(&self, req: &SamplingRequest) -> Result<(), SamplingError> {
        if !self.enabled {
            return Err(SamplingError::Disabled(req.server_id.clone()));
        }
        if self.deny_servers.contains(&req.server_id) {
            return Err(SamplingError::Disabled(req.server_id.clone()));
        }
        let server_cfg = self
            .per_server
            .get(&req.server_id)
            .cloned()
            .unwrap_or_default();
        if !server_cfg.enabled {
            return Err(SamplingError::Disabled(req.server_id.clone()));
        }
        self.check_rate_limit(&req.server_id, server_cfg.rate_limit_per_minute)?;
        Ok(())
    }

    pub fn cap(&self, req: &SamplingRequest) -> SamplingRequest {
        let server_cap = self
            .per_server
            .get(&req.server_id)
            .map(|p| p.max_tokens_cap)
            .unwrap_or(self.global_cap);
        let effective_cap = server_cap.min(self.global_cap);
        let mut out = req.clone();
        if out.max_tokens > effective_cap {
            tracing::info!(
                server = %req.server_id,
                requested = req.max_tokens,
                cap = effective_cap,
                "sampling maxTokens capped"
            );
            out.max_tokens = effective_cap;
        }
        out
    }

    fn check_rate_limit(&self, server_id: &str, per_minute: u32) -> Result<(), SamplingError> {
        if per_minute == 0 {
            return Err(SamplingError::Disabled(server_id.to_string()));
        }
        let bucket_mu = self
            .buckets
            .entry(server_id.to_string())
            .or_insert_with(|| Mutex::new(TokenBucket::new(per_minute)));
        // Unwrap the poison rather than propagating it: the bucket is
        // pure arithmetic over `Instant`/`f64`, so a panic elsewhere
        // can't have left it in an inconsistent shape. Without this,
        // one unrelated panic holding the lock would kill sampling
        // forever for this peer.
        let mut bucket = bucket_mu.lock().unwrap_or_else(|p| p.into_inner());
        bucket.refresh_capacity(per_minute);
        bucket.try_take().map_err(SamplingError::RateLimited)
    }
}

/// Token bucket with float tokens; `capacity` = `per_minute`, refill
/// rate = `capacity/60` tokens/s. Good enough for the sampling rate
/// limits we care about (tens/minute).
#[derive(Debug)]
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(per_minute: u32) -> Self {
        Self {
            capacity: per_minute as f64,
            tokens: per_minute as f64,
            last: Instant::now(),
        }
    }

    fn refresh_capacity(&mut self, per_minute: u32) {
        let cap = per_minute as f64;
        if (self.capacity - cap).abs() > f64::EPSILON {
            // Re-cap; clamp tokens into new capacity.
            self.capacity = cap;
            if self.tokens > cap {
                self.tokens = cap;
            }
        }
    }

    /// Take one token. On failure returns the ms until next token.
    fn try_take(&mut self) -> Result<(), u64> {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        let rate_per_sec = self.capacity / 60.0;
        self.tokens = (self.tokens + elapsed * rate_per_sec).min(self.capacity);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let missing = 1.0 - self.tokens;
            let ms = (missing / rate_per_sec * 1000.0).ceil() as u64;
            Err(ms.max(1))
        }
    }

    #[cfg(test)]
    fn advance_for_test(&mut self, d: Duration) {
        self.last -= d;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampling::types::{IncludeContext, SamplingRequest};

    fn req(server: &str) -> SamplingRequest {
        SamplingRequest {
            server_id: server.into(),
            messages: vec![],
            model_preferences: None,
            system_prompt: None,
            include_context: IncludeContext::None,
            temperature: None,
            max_tokens: 100,
            stop_sequences: vec![],
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn rejects_when_disabled() {
        let p = SamplingPolicy::new(false, vec![], 4096, HashMap::new());
        let r = req("x");
        let err = p.check(&r).unwrap_err();
        assert!(matches!(err, SamplingError::Disabled(_)));
    }

    #[test]
    fn rejects_denied_server() {
        let p = SamplingPolicy::new(true, vec!["bad".into()], 4096, HashMap::new());
        assert!(p.check(&req("bad")).is_err());
        assert!(p.check(&req("good")).is_ok());
    }

    #[test]
    fn allows_under_rate_limit() {
        let p = SamplingPolicy::permissive_for_tests();
        for _ in 0..5 {
            p.check(&req("x")).unwrap();
        }
    }

    #[test]
    fn rate_limit_trips_after_threshold() {
        let mut per = HashMap::new();
        per.insert(
            "x".into(),
            PerServerPolicy {
                enabled: true,
                rate_limit_per_minute: 3,
                max_tokens_cap: 4096,
            },
        );
        let p = SamplingPolicy::new(true, vec![], 4096, per);
        for _ in 0..3 {
            p.check(&req("x")).unwrap();
        }
        let err = p.check(&req("x")).unwrap_err();
        assert!(matches!(err, SamplingError::RateLimited(_)));
    }

    #[test]
    fn rate_limit_refills_over_time() {
        let mut per = HashMap::new();
        per.insert(
            "x".into(),
            PerServerPolicy {
                enabled: true,
                rate_limit_per_minute: 60, // 1 token/sec
                max_tokens_cap: 4096,
            },
        );
        let p = SamplingPolicy::new(true, vec![], 4096, per);
        // Drain.
        for _ in 0..60 {
            let _ = p.check(&req("x"));
        }
        // Advance the bucket's last-refill instant by 5 seconds.
        {
            let bucket = p.buckets.get("x").unwrap();
            bucket
                .lock()
                .unwrap()
                .advance_for_test(Duration::from_secs(5));
        }
        // Should allow at least 5 more.
        for _ in 0..5 {
            p.check(&req("x")).unwrap();
        }
    }

    #[test]
    fn caps_max_tokens_to_server_cap() {
        let mut per = HashMap::new();
        per.insert(
            "x".into(),
            PerServerPolicy {
                enabled: true,
                rate_limit_per_minute: 100,
                max_tokens_cap: 256,
            },
        );
        let p = SamplingPolicy::new(true, vec![], 4096, per);
        let mut r = req("x");
        r.max_tokens = 10_000;
        let capped = p.cap(&r);
        assert_eq!(capped.max_tokens, 256);
    }

    #[test]
    fn caps_max_tokens_to_global_when_server_cap_higher() {
        let mut per = HashMap::new();
        per.insert(
            "x".into(),
            PerServerPolicy {
                enabled: true,
                rate_limit_per_minute: 100,
                max_tokens_cap: 100_000,
            },
        );
        let p = SamplingPolicy::new(true, vec![], 2048, per);
        let mut r = req("x");
        r.max_tokens = 10_000;
        let capped = p.cap(&r);
        assert_eq!(capped.max_tokens, 2048);
    }
}
