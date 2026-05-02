//! Per-user rate limiter (token bucket).
//!
//! Each user-key gets a bucket that refills at
//! `tokens_per_window / window` rate up to a max of `max`.
//! Microapps call `try_acquire(user_key)` before processing an
//! inbound; on `Verdict::Denied`, vote `Block` from the Phase
//! 83.3 hook.
//!
//! Pure data + arithmetic — no async runtime, no persistence.
//! Restart drops the buckets (best-effort throttling, not a
//! billing meter).

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitVerdict {
    /// Token consumed; caller may proceed.
    Allowed { tokens_remaining: u32 },
    /// Bucket empty; caller MUST refuse / queue.
    Denied {
        /// Earliest time the bucket will have at least one token.
        retry_after: Duration,
    },
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

#[derive(Debug)]
pub struct RateLimitPerUser {
    /// Tokens added per `window`.
    tokens_per_window: u32,
    window: Duration,
    /// Cap on the bucket. `max < tokens_per_window` would
    /// throttle below the long-run rate; the constructor
    /// guards against that.
    max: u32,
    buckets: HashMap<String, Bucket>,
}

impl RateLimitPerUser {
    /// `tokens_per_window` tokens replenish every `window`,
    /// capped at `max`. `max` is clamped to at least
    /// `tokens_per_window` so the long-run rate is honoured.
    pub fn new(tokens_per_window: u32, window: Duration, max: u32) -> Self {
        let max = max.max(tokens_per_window);
        Self {
            tokens_per_window,
            window,
            max,
            buckets: HashMap::new(),
        }
    }

    /// Convenience: same `tokens_per_window` for `max`.
    pub fn flat(tokens_per_window: u32, window: Duration) -> Self {
        Self::new(tokens_per_window, window, tokens_per_window)
    }

    pub fn try_acquire(&mut self, user_key: &str) -> RateLimitVerdict {
        self.try_acquire_at(user_key, Instant::now())
    }

    /// Test-only / time-injectable variant.
    #[doc(hidden)]
    pub fn try_acquire_at(&mut self, user_key: &str, now: Instant) -> RateLimitVerdict {
        let max = self.max as f64;
        let rate = self.tokens_per_window as f64 / self.window.as_secs_f64();

        let bucket = self.buckets.entry(user_key.to_string()).or_insert(Bucket {
            tokens: max,
            last_refill: now,
        });

        // Refill since last touch.
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(max);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            RateLimitVerdict::Allowed {
                tokens_remaining: bucket.tokens.floor() as u32,
            }
        } else {
            // Time to next whole token.
            let needed = 1.0 - bucket.tokens;
            let secs = needed / rate;
            RateLimitVerdict::Denied {
                retry_after: Duration::from_secs_f64(secs),
            }
        }
    }

    /// Test/admin helper: drop one user's bucket (e.g. after an
    /// operator unblock).
    pub fn reset_user(&mut self, user_key: &str) {
        self.buckets.remove(user_key);
    }

    pub fn user_count(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_user_can_acquire_up_to_cap() {
        let mut r = RateLimitPerUser::flat(5, Duration::from_secs(60));
        for _ in 0..5 {
            let v = r.try_acquire("alice");
            assert!(matches!(v, RateLimitVerdict::Allowed { .. }));
        }
        let v = r.try_acquire("alice");
        assert!(matches!(v, RateLimitVerdict::Denied { .. }));
    }

    #[test]
    fn buckets_are_isolated_per_user() {
        let mut r = RateLimitPerUser::flat(2, Duration::from_secs(60));
        // alice exhausts.
        r.try_acquire("alice");
        r.try_acquire("alice");
        assert!(matches!(
            r.try_acquire("alice"),
            RateLimitVerdict::Denied { .. }
        ));
        // bob unaffected.
        assert!(matches!(
            r.try_acquire("bob"),
            RateLimitVerdict::Allowed { .. }
        ));
    }

    #[test]
    fn refill_after_window_replenishes() {
        let mut r = RateLimitPerUser::flat(2, Duration::from_secs(10));
        let t0 = Instant::now();
        r.try_acquire_at("alice", t0);
        r.try_acquire_at("alice", t0);
        assert!(matches!(
            r.try_acquire_at("alice", t0),
            RateLimitVerdict::Denied { .. }
        ));
        // Full window later — bucket back to max.
        let t1 = t0 + Duration::from_secs(10);
        let v = r.try_acquire_at("alice", t1);
        assert!(matches!(v, RateLimitVerdict::Allowed { .. }));
    }

    #[test]
    fn partial_refill_still_denies_when_under_one_token() {
        let mut r = RateLimitPerUser::flat(2, Duration::from_secs(10));
        let t0 = Instant::now();
        r.try_acquire_at("alice", t0);
        r.try_acquire_at("alice", t0);
        // 1 second later — refilled 0.2 tokens, still < 1.
        let t1 = t0 + Duration::from_secs(1);
        let v = r.try_acquire_at("alice", t1);
        match v {
            RateLimitVerdict::Denied { retry_after } => {
                // Need 0.8 more tokens; rate 0.2/s → 4 s.
                assert!(retry_after >= Duration::from_secs(3));
                assert!(retry_after <= Duration::from_secs(5));
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn max_cap_clamps_to_at_least_tokens_per_window() {
        // max=1 with tokens_per_window=5 would let users burst
        // ABOVE the long-run rate. Constructor must clamp max.
        let r = RateLimitPerUser::new(5, Duration::from_secs(60), 1);
        // max should have been clamped to 5.
        // Indirect verification via burst capacity.
        let mut r = r;
        for _ in 0..5 {
            assert!(matches!(
                r.try_acquire("a"),
                RateLimitVerdict::Allowed { .. }
            ));
        }
    }

    #[test]
    fn reset_user_drops_bucket_state() {
        let mut r = RateLimitPerUser::flat(1, Duration::from_secs(60));
        r.try_acquire("alice");
        assert!(matches!(
            r.try_acquire("alice"),
            RateLimitVerdict::Denied { .. }
        ));
        r.reset_user("alice");
        // Fresh bucket — full again.
        assert!(matches!(
            r.try_acquire("alice"),
            RateLimitVerdict::Allowed { .. }
        ));
    }

    #[test]
    fn user_count_tracks_distinct_keys() {
        let mut r = RateLimitPerUser::flat(2, Duration::from_secs(60));
        r.try_acquire("a");
        r.try_acquire("b");
        r.try_acquire("a");
        assert_eq!(r.user_count(), 2);
    }
}
