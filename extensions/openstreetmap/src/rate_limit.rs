use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Token-less rate limiter: enforces a minimum interval between requests.
/// Nominatim's usage policy requires <= 1 req/sec from a single IP/UA.
#[derive(Debug)]
pub struct RateLimiter {
    min_interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: Mutex::new(None),
        }
    }

    /// Block the calling thread until the next slot is available, then mark
    /// the slot as consumed.
    pub fn acquire(&self) {
        let wait;
        {
            let mut guard = self.last.lock().expect("rate limiter poisoned");
            let now = Instant::now();
            wait = match *guard {
                Some(t) => {
                    let elapsed = now.duration_since(t);
                    if elapsed >= self.min_interval {
                        Duration::ZERO
                    } else {
                        self.min_interval - elapsed
                    }
                }
                None => Duration::ZERO,
            };
            // Reserve the slot at `now + wait` to avoid concurrent threads stacking.
            *guard = Some(now + wait);
        }
        if !wait.is_zero() {
            sleep(wait);
        }
    }

    pub fn reset(&self) {
        *self.last.lock().expect("rate limiter poisoned") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_call_does_not_wait() {
        let r = RateLimiter::new(Duration::from_millis(100));
        let t0 = Instant::now();
        r.acquire();
        assert!(t0.elapsed() < Duration::from_millis(20));
    }

    #[test]
    fn second_call_waits_min_interval() {
        let r = RateLimiter::new(Duration::from_millis(80));
        r.acquire();
        let t0 = Instant::now();
        r.acquire();
        assert!(t0.elapsed() >= Duration::from_millis(70));
    }
}
