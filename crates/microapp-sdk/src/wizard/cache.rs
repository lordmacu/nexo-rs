//! Generic TTL snapshot cache.
//!
//! The first-run wizard's bootstrap endpoint hits the same
//! `nexo/admin/*` RPCs on every page-load, so a 5 s in-memory
//! cache turns "open three tabs" into one fan-out. The shape is
//! generic — any microapp surface that wants `compute once, share
//! across N concurrent reads, drop on operator action` reuses
//! this directly.
//!
//! Lifted from `agent-creator-microapp::onboarding::bootstrap`
//! with the value type abstracted: `BootstrapState` becomes
//! `T: Clone + Send + Sync`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

/// Cache cell. Wrap in `Arc` and clone freely — the inner
/// `RwLock` allows concurrent readers + an exclusive writer for
/// invalidation.
#[derive(Debug)]
pub struct CachedSnapshot<T> {
    inner: RwLock<Option<(Instant, T)>>,
    ttl: Duration,
}

impl<T> CachedSnapshot<T>
where
    T: Clone + Send + Sync,
{
    /// Build a snapshot cache with the given time-to-live.
    pub fn new(ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(None),
            ttl,
        })
    }

    /// Drop the cached value. Subsequent [`Self::get_or_compute`]
    /// calls will recompute.
    pub async fn invalidate(&self) {
        *self.inner.write().await = None;
    }

    /// Borrow the cached value when fresh, otherwise compute via
    /// `f` and store the result.
    ///
    /// `f` is invoked at most once per TTL window per cache;
    /// concurrent callers within the window all read the same
    /// stored snapshot.
    ///
    /// Errors from `f` propagate without poisoning the cache —
    /// the next call will retry. This matches the
    /// agent-creator-microapp's `bootstrap::snapshot` semantics.
    pub async fn get_or_compute<F, Fut, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        if let Some(hit) = self.get().await {
            return Ok(hit);
        }
        let value = f().await?;
        self.put(value.clone()).await;
        Ok(value)
    }

    async fn get(&self) -> Option<T> {
        let g = self.inner.read().await;
        g.as_ref()
            .and_then(|(at, s)| (at.elapsed() < self.ttl).then(|| s.clone()))
    }

    async fn put(&self, s: T) {
        *self.inner.write().await = Some((Instant::now(), s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn computes_once_within_ttl() {
        let cache = CachedSnapshot::<u64>::new(Duration::from_secs(60));
        let calls = Arc::new(AtomicUsize::new(0));

        let c = Arc::clone(&calls);
        let v1 = cache
            .get_or_compute(|| async {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(42)
            })
            .await
            .unwrap();
        assert_eq!(v1, 42);

        let c = Arc::clone(&calls);
        let v2 = cache
            .get_or_compute(|| async {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(99)
            })
            .await
            .unwrap();
        // Second call short-circuits to the cached `42`.
        assert_eq!(v2, 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn invalidate_forces_recompute() {
        let cache = CachedSnapshot::<u64>::new(Duration::from_secs(60));
        let calls = Arc::new(AtomicUsize::new(0));

        for expected in [1usize, 2usize] {
            let c = Arc::clone(&calls);
            let _ = cache
                .get_or_compute(|| async {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(0)
                })
                .await
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), expected);
            cache.invalidate().await;
        }
    }

    #[tokio::test]
    async fn ttl_expiry_triggers_recompute() {
        let cache = CachedSnapshot::<u64>::new(Duration::from_millis(20));
        let calls = Arc::new(AtomicUsize::new(0));

        let c = Arc::clone(&calls);
        let _ = cache
            .get_or_compute(|| async {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(7)
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let c = Arc::clone(&calls);
        let _ = cache
            .get_or_compute(|| async {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(7)
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn errors_do_not_poison_cache() {
        let cache = CachedSnapshot::<u64>::new(Duration::from_secs(60));
        let calls = Arc::new(AtomicUsize::new(0));

        // First call errors — nothing cached.
        let c = Arc::clone(&calls);
        let r: Result<u64, &str> = cache
            .get_or_compute(|| async {
                c.fetch_add(1, Ordering::SeqCst);
                Err("nope")
            })
            .await;
        assert!(r.is_err());

        // Second call succeeds — the previous error didn't stick.
        let c = Arc::clone(&calls);
        let r: Result<u64, &str> = cache
            .get_or_compute(|| async {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(11)
            })
            .await;
        assert_eq!(r.unwrap(), 11);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
