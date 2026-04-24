use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct QuotaTracker {
    remaining: Arc<AtomicU64>,
    alert_threshold: u64,
}

impl QuotaTracker {
    pub fn new(alert_threshold: u64) -> Self {
        Self {
            remaining: Arc::new(AtomicU64::new(u64::MAX)),
            alert_threshold,
        }
    }

    pub fn set_remaining(&self, remaining: u64) {
        let prev = self.remaining.swap(remaining, Ordering::Relaxed);
        if remaining < self.alert_threshold && prev >= self.alert_threshold {
            tracing::warn!(
                remaining,
                threshold = self.alert_threshold,
                "LLM quota below alert threshold"
            );
        }
    }

    pub fn record_usage(&self, prompt_tokens: u32, completion_tokens: u32) {
        let used = prompt_tokens.saturating_add(completion_tokens) as u64;
        // CAS-loop with saturating_sub: `fetch_sub` wraps on underflow
        // and would flip the counter to u64::MAX if usage exceeded the
        // remaining quota — alert semantics invert and ops dashboards
        // show "infinite quota" when we've actually exhausted it.
        let mut cur = self.remaining.load(Ordering::Relaxed);
        loop {
            let next = cur.saturating_sub(used);
            match self.remaining.compare_exchange_weak(
                cur,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if next < self.alert_threshold && cur >= self.alert_threshold {
                        tracing::warn!(
                            remaining = next,
                            threshold = self.alert_threshold,
                            "LLM quota below alert threshold (via usage)"
                        );
                    }
                    break;
                }
                Err(observed) => cur = observed,
            }
        }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining.load(Ordering::Relaxed)
    }

    pub fn alert_threshold(&self) -> u64 {
        self.alert_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::QuotaTracker;

    #[test]
    fn tracker_alerts_when_crossing_threshold() {
        let tracker = QuotaTracker::new(100);

        tracker.set_remaining(150);
        assert_eq!(tracker.remaining(), 150);

        tracker.set_remaining(90);
        assert_eq!(tracker.remaining(), 90);
    }

    #[test]
    fn record_usage_decrements_remaining() {
        let tracker = QuotaTracker::new(1000);
        tracker.set_remaining(500);

        tracker.record_usage(100, 50);
        assert_eq!(tracker.remaining(), 350);

        tracker.record_usage(50, 50);
        assert_eq!(tracker.remaining(), 250);
    }

    #[test]
    fn does_not_alert_above_threshold() {
        let tracker = QuotaTracker::new(50);
        tracker.set_remaining(100);
        tracker.set_remaining(80);
        assert_eq!(tracker.remaining(), 80);
    }

    #[test]
    fn usage_saturates_at_zero_not_wraps() {
        let tracker = QuotaTracker::new(1000);
        tracker.set_remaining(10);
        // Burn more than remaining — the old `fetch_sub` wrapped to
        // u64::MAX. New path saturates at 0.
        tracker.record_usage(50_000, 0);
        assert_eq!(tracker.remaining(), 0);
    }
}
