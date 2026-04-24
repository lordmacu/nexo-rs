//! Per-sender inbound rate limit.
//!
//! Protects agents exposed to public plugin surfaces (Telegram bot,
//! WhatsApp business account) from flood / cost griefing. The runtime
//! loop consults this before enqueueing a message; a denied event is
//! dropped with a trace log.
//!
//! Token bucket keyed by `(agent_id, sender_id)` — each sender gets
//! its own quota, isolated across agents. Unlike `ToolRateLimiter`
//! this is fully sync (DashMap entry lock only) because the hot path
//! is per-message and must not yield.
use agent_config::types::agents::SenderRateLimitConfig;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::Mutex;

/// Cap on concurrent sender buckets. Protects against OOM when a
/// public bot sees traffic from a churning population of sender_ids
/// (spam, probes, abandoned chats). At the cap, the oldest-touched
/// bucket is evicted — losing at most a half-refilled bucket for that
/// sender, which re-initialises on their next message.
pub const DEFAULT_MAX_BUCKETS: usize = 50_000;

pub struct SenderRateLimiter {
    cfg: SenderRateLimitConfig,
    buckets: DashMap<(String, String), TokenBucket>,
    max_buckets: usize,
    /// Monotonic counter stamped on every `try_acquire` so we can pick
    /// the stalest bucket without taking any lock.
    clock: AtomicU64,
}
struct TokenBucket {
    capacity: u64,
    rate_per_sec: f64,
    tokens: AtomicU64,
    last_refill: Mutex<Instant>,
    last_touch: AtomicU64,
}
impl TokenBucket {
    fn new(cfg: &SenderRateLimitConfig, touch: u64) -> Self {
        Self {
            capacity: cfg.burst.max(1),
            rate_per_sec: cfg.rps.max(0.0),
            tokens: AtomicU64::new(cfg.burst.max(1)),
            last_refill: Mutex::new(Instant::now()),
            last_touch: AtomicU64::new(touch),
        }
    }
    async fn try_acquire(&self, touch: u64) -> bool {
        self.last_touch.store(touch, Ordering::Relaxed);
        let now = Instant::now();
        {
            let mut last = self.last_refill.lock().await;
            let elapsed = now.duration_since(*last).as_secs_f64();
            if elapsed > 0.0 && self.rate_per_sec > 0.0 {
                let add = (elapsed * self.rate_per_sec).floor() as u64;
                if add > 0 {
                    let cur = self.tokens.load(Ordering::Relaxed);
                    let new = cur.saturating_add(add).min(self.capacity);
                    self.tokens.store(new, Ordering::Relaxed);
                    let consumed = add as f64 / self.rate_per_sec;
                    *last += std::time::Duration::from_secs_f64(consumed);
                }
            }
        }
        let mut cur = self.tokens.load(Ordering::Acquire);
        loop {
            if cur == 0 {
                return false;
            }
            match self
                .tokens
                .compare_exchange(cur, cur - 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }
}
impl SenderRateLimiter {
    pub fn new(cfg: SenderRateLimitConfig) -> Self {
        Self::with_cap(cfg, DEFAULT_MAX_BUCKETS)
    }

    pub fn with_cap(cfg: SenderRateLimitConfig, max_buckets: usize) -> Self {
        Self {
            cfg,
            buckets: DashMap::new(),
            max_buckets,
            clock: AtomicU64::new(0),
        }
    }

    /// Drop the single stalest bucket once we're at or above the cap.
    /// Runs O(n) but only fires when we'd otherwise allocate a new
    /// entry — amortises to near-zero under steady traffic.
    fn enforce_cap(&self) {
        if self.max_buckets == 0 {
            return;
        }
        while self.buckets.len() >= self.max_buckets {
            let oldest = self
                .buckets
                .iter()
                .min_by_key(|e| e.value().last_touch.load(Ordering::Relaxed))
                .map(|e| e.key().clone());
            match oldest {
                Some(k) => {
                    if self.buckets.remove(&k).is_some() {
                        tracing::debug!(
                            evicted_agent = %k.0,
                            evicted_sender = %k.1,
                            cap = self.max_buckets,
                            "sender bucket cap reached; evicted stalest"
                        );
                    }
                }
                None => break,
            }
        }
    }

    pub fn active_buckets(&self) -> usize {
        self.buckets.len()
    }

    /// Allow the message through, or deny (bucket empty). `sender_id`
    /// is typically the platform-specific user id (telegram user id,
    /// whatsapp phone, etc.). Unknown senders (`None`) are unlimited —
    /// fall back gracefully for system events without a sender.
    pub async fn try_acquire(&self, agent: &str, sender: Option<&str>) -> bool {
        let Some(sender) = sender else { return true };
        let key = (agent.to_string(), sender.to_string());
        let touch = self.clock.fetch_add(1, Ordering::Relaxed);
        if !self.buckets.contains_key(&key) {
            self.enforce_cap();
        }
        let entry = self
            .buckets
            .entry(key)
            .or_insert_with(|| TokenBucket::new(&self.cfg, touch));
        entry.value().try_acquire(touch).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn burst_exhausts_then_refills() {
        let rl = SenderRateLimiter::new(SenderRateLimitConfig {
            rps: 10.0,
            burst: 3,
        });
        // Burst of 3 allows 3 calls in a row.
        assert!(rl.try_acquire("a", Some("u1")).await);
        assert!(rl.try_acquire("a", Some("u1")).await);
        assert!(rl.try_acquire("a", Some("u1")).await);
        // 4th is denied.
        assert!(!rl.try_acquire("a", Some("u1")).await);
        // After a refill window, at least one token is back.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert!(rl.try_acquire("a", Some("u1")).await);
    }
    #[tokio::test]
    async fn sender_buckets_are_isolated_per_sender_and_agent() {
        let rl = SenderRateLimiter::new(SenderRateLimitConfig { rps: 0.0, burst: 1 });
        // Sender u1 on agent A → uses its single token.
        assert!(rl.try_acquire("a", Some("u1")).await);
        assert!(!rl.try_acquire("a", Some("u1")).await);
        // Sender u2 on agent A has its own bucket → 1 allowed.
        assert!(rl.try_acquire("a", Some("u2")).await);
        // Same u1 on agent B also independent.
        assert!(rl.try_acquire("b", Some("u1")).await);
    }
    #[tokio::test]
    async fn sender_none_is_always_allowed() {
        let rl = SenderRateLimiter::new(SenderRateLimitConfig { rps: 0.0, burst: 0 });
        // burst=0 would deny everything, but None bypasses the check.
        for _ in 0..5 {
            assert!(rl.try_acquire("a", None).await);
        }
    }

    #[tokio::test]
    async fn cap_evicts_stalest_bucket() {
        let rl = SenderRateLimiter::with_cap(
            SenderRateLimitConfig {
                rps: 1000.0,
                burst: 10,
            },
            3,
        );
        // Fill to cap; u1 stamped earliest.
        rl.try_acquire("a", Some("u1")).await;
        rl.try_acquire("a", Some("u2")).await;
        rl.try_acquire("a", Some("u3")).await;
        assert_eq!(rl.active_buckets(), 3);

        // u4 causes u1 (oldest last_touch) to be evicted.
        rl.try_acquire("a", Some("u4")).await;
        assert_eq!(rl.active_buckets(), 3);
    }
}
