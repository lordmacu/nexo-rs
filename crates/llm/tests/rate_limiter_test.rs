use nexo_llm::RateLimiter;
use std::time::{Duration, Instant};

#[tokio::test]
async fn acquire_does_not_overlap_slots() {
    // 10 rps → 100ms between calls
    let rl = RateLimiter::new(10.0);

    let t0 = Instant::now();
    rl.acquire().await;
    rl.acquire().await;
    let elapsed = t0.elapsed();

    // Two acquires must be at least 90ms apart (allow 10ms slack for scheduling)
    assert!(elapsed >= Duration::from_millis(90), "elapsed: {elapsed:?}");
}

#[tokio::test]
async fn first_acquire_is_immediate() {
    let rl = RateLimiter::new(1.0);
    let t0 = Instant::now();
    rl.acquire().await;
    // First slot should fire almost immediately (< 50ms)
    assert!(t0.elapsed() < Duration::from_millis(50));
}
