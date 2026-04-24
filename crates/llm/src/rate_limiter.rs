use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::quota_tracker::QuotaTracker;

pub struct RateLimiter {
    interval: Duration,
    next_allowed: Arc<Mutex<Instant>>,
    quota_tracker: Option<QuotaTracker>,
}

impl RateLimiter {
    pub fn new(requests_per_second: f32) -> Self {
        Self::with_quota(requests_per_second, None)
    }

    pub fn with_quota(requests_per_second: f32, quota_alert_threshold: Option<u64>) -> Self {
        let rps = requests_per_second.max(0.001);
        let interval = Duration::from_secs_f32(1.0 / rps);
        let quota_tracker = quota_alert_threshold.map(QuotaTracker::new);
        Self {
            interval,
            next_allowed: Arc::new(Mutex::new(Instant::now())),
            quota_tracker,
        }
    }

    /// Waits until the next request slot is available, then claims it.
    pub async fn acquire(&self) {
        let mut next = self.next_allowed.lock().await;
        let now = Instant::now();
        if now < *next {
            tokio::time::sleep(*next - now).await;
        }
        *next = Instant::now() + self.interval;
    }

    pub fn quota_tracker(&self) -> Option<&QuotaTracker> {
        self.quota_tracker.as_ref()
    }
}
