//! Phase 76.6 — per-principal in-flight concurrency cap +
//! per-call timeout, keyed on `(TenantId, ToolName)`.
//!
//! Sits inside `Dispatcher::dispatch` for `tools/call`, AFTER the
//! 76.5 rate-limit gate. Stdio principals bypass entirely.
//!
//! ## Why this is distinct from 76.5 (rate-limit)
//!
//! * 76.5 measures **calls per second** (token bucket). Protects
//!   against floods of fast requests.
//! * 76.6 measures **calls in flight at once** (semaphore).
//!   Protects against slow handlers piling up — a tenant that
//!   sends 1 req/s but each takes 60 s would pass 76.5 cleanly
//!   but accumulate 60 simultaneous calls without a cap here.
//!
//! The two layers compose: a request must (a) get a token from
//! 76.5 AND (b) acquire a permit from 76.6 before reaching the
//! handler.
//!
//! ## Wire shape
//!
//! Two distinct outcomes are surfaced to the caller as JSON-RPC
//! error codes:
//!
//! * `-32002` — concurrency cap exceeded (queue wait expired).
//!   Body `data` carries `max_in_flight` and `queue_wait_ms_exceeded`.
//! * `-32001` — per-call timeout exceeded.
//!   Body `data` carries `timeout_ms`.
//!
//! ## Reference patterns
//!
//! * **RAII permit + cancel-aware acquire** — in-tree
//!   `crates/mcp/src/client.rs:873-899` (76.1 client side).
//! * **DashMap + sweeper + hard-cap eviction** — Phase 76.5
//!   `per_principal_rate_limit.rs`. We mirror the same shape with
//!   `Semaphore` in place of `TokenBucket`.
//! * **`tokio::select!` cancellation pattern** — Phase 76.2
//!   `dispatch.rs:201-205` (`biased; cancel; do_dispatch`).
//! * **AbortSignal/AbortController equivalent in TS** —
//!   `claude-code-leak/src/Task.ts:39` and
//!   `claude-code-leak/src/services/tools/toolExecution.ts:415-416`.
//!   The leak does not implement server-side concurrency caps;
//!   only the cancellation propagation pattern is portable.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::auth::TenantId;

// --- Internal entry ----------------------------------------------

/// One semaphore + bookkeeping per (tenant, tool) pair. Kept
/// internal so callers always reach for the public API
/// (`PerPrincipalConcurrencyCap::acquire`) instead of the raw
/// `Semaphore`, which forces the LRU bookkeeping to stay
/// consistent.
pub(crate) struct SemEntry {
    pub(crate) sem: Arc<Semaphore>,
    pub(crate) last_seen: Mutex<Instant>,
    /// Cached so the sweeper can decide "is this entry idle?"
    /// without re-reading the config map. Mirrors the
    /// permit count the semaphore was constructed with.
    pub(crate) max_in_flight: u32,
}

impl SemEntry {
    pub(crate) fn new(max_in_flight: u32) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(max_in_flight as usize)),
            last_seen: Mutex::new(Instant::now()),
            max_in_flight,
        }
    }

    pub(crate) fn touch(&self, now: Instant) {
        *self.last_seen.lock() = now;
    }
}

// --- Public errors -----------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireRejection {
    /// Caller's `CancellationToken` fired while waiting in queue.
    /// Caller surfaces as `-32800 request cancelled`.
    Cancelled,
    /// `queue_wait_ms` expired before a permit became available.
    /// Caller surfaces as `-32002 concurrent calls exceeded`.
    QueueTimeout {
        max_in_flight: u32,
        queue_wait_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ConcurrencyCapStats {
    pub semaphores: usize,
    pub timeouts_total: u64,
    pub rejections_total: u64,
}

// --- PerPrincipalConcurrencyCap ----------------------------------

pub struct PerPrincipalConcurrencyCap {
    cfg: Arc<PerPrincipalConcurrencyConfig>,
    semaphores: DashMap<(TenantId, String), Arc<SemEntry>>,
    timeouts_total: AtomicU64,
    rejections_total: AtomicU64,
    /// Background sweeper handle. Aborted on drop.
    sweeper: Mutex<Option<JoinHandle<()>>>,
    /// Used by the disabled-mode fast-path: we hand out permits
    /// from a single "always available" semaphore so the caller
    /// code path is uniform whether the cap is on or off.
    disabled_sentinel: Arc<Semaphore>,
}

impl PerPrincipalConcurrencyCap {
    /// Build the cap and spawn the background sweeper. Must be
    /// called from inside a tokio runtime.
    pub fn new(cfg: PerPrincipalConcurrencyConfig) -> Result<Arc<Self>, String> {
        let me = Self::new_without_sweeper(cfg)?;
        let weak = Arc::downgrade(&me);
        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            tick.tick().await; // skip immediate first tick
            loop {
                tick.tick().await;
                let Some(strong) = weak.upgrade() else { return };
                strong.prune_stale(Instant::now());
            }
        });
        *me.sweeper.lock() = Some(handle);
        Ok(me)
    }

    /// Build the cap WITHOUT the background sweeper. Tests use
    /// this so they can drive `prune_stale` directly under a
    /// fake clock.
    pub fn new_without_sweeper(cfg: PerPrincipalConcurrencyConfig) -> Result<Arc<Self>, String> {
        cfg.validate()?;
        Ok(Arc::new(Self {
            cfg: Arc::new(cfg),
            semaphores: DashMap::new(),
            timeouts_total: AtomicU64::new(0),
            rejections_total: AtomicU64::new(0),
            sweeper: Mutex::new(None),
            disabled_sentinel: Arc::new(Semaphore::new(usize::MAX >> 4)),
        }))
    }

    pub fn cap_for(&self, tool: &str) -> u32 {
        self.cfg.cap_for(tool)
    }

    pub fn timeout_for(&self, tool: &str) -> Duration {
        self.cfg.timeout_for(tool)
    }

    pub fn config(&self) -> &PerPrincipalConcurrencyConfig {
        &self.cfg
    }

    pub fn semaphore_count(&self) -> usize {
        self.semaphores.len()
    }

    pub fn stats(&self) -> ConcurrencyCapStats {
        ConcurrencyCapStats {
            semaphores: self.semaphores.len(),
            timeouts_total: self.timeouts_total.load(Ordering::Relaxed),
            rejections_total: self.rejections_total.load(Ordering::Relaxed),
        }
    }

    /// Bump the timeouts counter. Called by the dispatcher when
    /// the per-call `tokio::time::timeout` elapses; the dispatcher
    /// owns the timing because the timeout depends on the
    /// handler future, not the cap struct.
    pub fn note_timeout(&self) {
        self.timeouts_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Try to acquire a permit for `(tenant, tool)`. The returned
    /// `OwnedSemaphorePermit` is RAII — drop it to release.
    ///
    /// When `cfg.enabled == false`, returns a permit from the
    /// disabled-mode sentinel semaphore (always available). Code
    /// path is uniform either way.
    pub async fn acquire(
        &self,
        tenant: &TenantId,
        tool: &str,
        cancel: &CancellationToken,
    ) -> Result<OwnedSemaphorePermit, AcquireRejection> {
        if !self.cfg.enabled {
            // Fast path: hand out a sentinel permit. Cancel-aware
            // so a shutting-down session doesn't block on the
            // sentinel acquire (it never blocks in practice but
            // cheap to keep symmetric).
            return Arc::clone(&self.disabled_sentinel)
                .acquire_owned()
                .await
                .map_err(|_| AcquireRejection::Cancelled);
        }

        let cap = self.cfg.cap_for(tool);
        let key = (tenant.clone(), tool.to_string());

        // Hard-cap eviction BEFORE the lookup-then-insert. Same
        // pattern as 76.5 `per_principal_rate_limit.rs`.
        if !self.semaphores.contains_key(&key) && self.semaphores.len() >= self.cfg.max_buckets {
            self.evict_oldest(self.cfg.max_buckets / 100 + 1);
        }

        let entry = self
            .semaphores
            .entry(key)
            .or_insert_with(|| Arc::new(SemEntry::new(cap)));
        entry.touch(Instant::now());
        let sem = Arc::clone(&entry.sem);
        drop(entry); // release the DashMap shard lock before await

        let wait = Duration::from_millis(self.cfg.queue_wait_ms);

        // queue_wait_ms == 0: try-acquire only, no wait.
        if wait.is_zero() {
            return match sem.try_acquire_owned() {
                Ok(p) => Ok(p),
                Err(_) => {
                    self.rejections_total.fetch_add(1, Ordering::Relaxed);
                    Err(AcquireRejection::QueueTimeout {
                        max_in_flight: cap,
                        queue_wait_ms: 0,
                    })
                }
            };
        }

        // Otherwise wait up to queue_wait_ms — but also honour
        // the caller's CancellationToken (HTTP client disconnect,
        // session shutdown, etc.).
        let acquire_fut = sem.acquire_owned();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AcquireRejection::Cancelled),
            res = tokio::time::timeout(wait, acquire_fut) => match res {
                Ok(Ok(p)) => Ok(p),
                Ok(Err(_closed)) => unreachable!(
                    "Semaphore is never closed in this codebase"
                ),
                Err(_elapsed) => {
                    self.rejections_total.fetch_add(1, Ordering::Relaxed);
                    Err(AcquireRejection::QueueTimeout {
                        max_in_flight: cap,
                        queue_wait_ms: self.cfg.queue_wait_ms,
                    })
                }
            }
        }
    }

    /// Drop `n` entries with smallest `last_seen`. Only entries
    /// whose semaphore is fully released (no calls in flight)
    /// are eligible — we never strand an in-flight permit.
    fn evict_oldest(&self, n: usize) {
        if n == 0 || self.semaphores.is_empty() {
            return;
        }
        let mut victims: Vec<((TenantId, String), Instant)> = self
            .semaphores
            .iter()
            .filter(|e| {
                let cap_count = e.value().max_in_flight as usize;
                e.value().sem.available_permits() == cap_count
            })
            .map(|e| {
                let key = e.key().clone();
                let last_seen = *e.value().last_seen.lock();
                (key, last_seen)
            })
            .collect();
        victims.sort_by_key(|(_, ls)| *ls);
        for (key, _) in victims.into_iter().take(n) {
            self.semaphores.remove(&key);
        }
    }

    /// Drop entries whose `last_seen` is older than `stale_ttl`,
    /// but only when the semaphore has no in-flight permits.
    /// Called on a 60-s tick by the background sweeper.
    pub(crate) fn prune_stale(&self, now: Instant) {
        let ttl = Duration::from_secs(self.cfg.stale_ttl_secs);
        let mut victims: Vec<(TenantId, String)> = Vec::new();
        for entry in self.semaphores.iter() {
            let cap_count = entry.value().max_in_flight as usize;
            if entry.value().sem.available_permits() != cap_count {
                continue; // calls in flight; do not strand them
            }
            let last_seen = *entry.value().last_seen.lock();
            if now.saturating_duration_since(last_seen) > ttl {
                victims.push(entry.key().clone());
            }
        }
        for key in victims {
            self.semaphores.remove(&key);
        }
    }
}

impl Drop for PerPrincipalConcurrencyCap {
    fn drop(&mut self) {
        if let Some(h) = self.sweeper.lock().take() {
            h.abort();
        }
    }
}

// --- Limiter unit tests ------------------------------------------

#[cfg(test)]
mod limiter_tests {
    use super::*;

    fn tid(s: &str) -> TenantId {
        TenantId::parse(s).unwrap()
    }

    fn cfg(max_in_flight: u32) -> PerPrincipalConcurrencyConfig {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.default = PerToolConcurrency {
            max_in_flight,
            timeout_secs: None,
        };
        c
    }

    #[tokio::test]
    async fn acquire_admits_up_to_cap() {
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(cfg(2)).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        let p1 = cap.acquire(&a, "x", &cancel).await.unwrap();
        let p2 = cap.acquire(&a, "x", &cancel).await.unwrap();
        // Third acquire must time out — drive queue_wait short.
        // Default 5000ms is too slow for a unit test, so override.
        let mut cfg2 = cfg(2);
        cfg2.queue_wait_ms = 50;
        let cap2 = PerPrincipalConcurrencyCap::new_without_sweeper(cfg2).unwrap();
        let _p1 = cap2.acquire(&a, "x", &cancel).await.unwrap();
        let _p2 = cap2.acquire(&a, "x", &cancel).await.unwrap();
        let err = cap2.acquire(&a, "x", &cancel).await.unwrap_err();
        match err {
            AcquireRejection::QueueTimeout {
                max_in_flight,
                queue_wait_ms,
            } => {
                assert_eq!(max_in_flight, 2);
                assert_eq!(queue_wait_ms, 50);
            }
            other => panic!("expected QueueTimeout, got {other:?}"),
        }
        drop(p1);
        drop(p2);
    }

    #[tokio::test]
    async fn acquire_with_zero_wait_rejects_immediately() {
        let mut c = cfg(1);
        c.queue_wait_ms = 0;
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(c).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        let _p1 = cap.acquire(&a, "x", &cancel).await.unwrap();
        let err = cap.acquire(&a, "x", &cancel).await.unwrap_err();
        assert!(matches!(
            err,
            AcquireRejection::QueueTimeout {
                queue_wait_ms: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn permit_dropped_releases_back_to_pool() {
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(cfg(1)).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        let p1 = cap.acquire(&a, "x", &cancel).await.unwrap();
        // Sanity: the bucket exists.
        assert_eq!(cap.semaphore_count(), 1);
        drop(p1);
        // Next acquire is immediate — no queue wait needed.
        let _p2 = cap.acquire(&a, "x", &cancel).await.unwrap();
    }

    #[tokio::test]
    async fn acquire_cancelled_returns_cancelled() {
        let mut c = cfg(1);
        c.queue_wait_ms = 5_000; // long enough we cancel before it fires
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(c).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        let _p1 = cap.acquire(&a, "x", &cancel).await.unwrap();
        // Spawn a task that will be queued waiting; cancel fires
        // before the queue_wait elapses.
        let cap2 = Arc::clone(&cap);
        let cancel2 = cancel.clone();
        let a2 = a.clone();
        let t = tokio::spawn(async move { cap2.acquire(&a2, "x", &cancel2).await });
        // Give the spawned task a moment to enter the wait, then
        // cancel.
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        let res = t.await.unwrap();
        assert_eq!(res.unwrap_err(), AcquireRejection::Cancelled);
    }

    #[tokio::test]
    async fn cross_tenant_caps_independent() {
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(cfg(1)).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        let b = tid("tenant-b");
        let _p_a = cap.acquire(&a, "x", &cancel).await.unwrap();
        // Tenant B is unaffected.
        let _p_b = cap.acquire(&b, "x", &cancel).await.unwrap();
        assert_eq!(cap.semaphore_count(), 2);
    }

    #[tokio::test]
    async fn cross_tool_caps_independent_for_same_tenant() {
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(cfg(1)).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        let _p1 = cap.acquire(&a, "tool_a", &cancel).await.unwrap();
        // Same tenant, different tool — different bucket.
        let _p2 = cap.acquire(&a, "tool_b", &cancel).await.unwrap();
        assert_eq!(cap.semaphore_count(), 2);
    }

    #[tokio::test]
    async fn disabled_returns_no_op_permit() {
        let mut c = cfg(1);
        c.enabled = false;
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(c).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        // 100 acquires, all admit, no buckets created.
        for _ in 0..100 {
            let _p = cap.acquire(&a, "x", &cancel).await.unwrap();
        }
        assert_eq!(cap.semaphore_count(), 0);
    }

    #[tokio::test]
    async fn stats_reports_buckets_timeouts_rejections() {
        let mut c = cfg(1);
        c.queue_wait_ms = 0;
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(c).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        let _p = cap.acquire(&a, "x", &cancel).await.unwrap();
        // Forced rejection: queue_wait_ms=0 + bucket full.
        let _ = cap.acquire(&a, "x", &cancel).await.unwrap_err();
        cap.note_timeout();
        let s = cap.stats();
        assert_eq!(s.semaphores, 1);
        assert_eq!(s.rejections_total, 1);
        assert_eq!(s.timeouts_total, 1);
    }

    #[tokio::test]
    async fn prune_stale_skips_in_flight_entries() {
        let mut c = cfg(1);
        c.stale_ttl_secs = 60;
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(c).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        let _held = cap.acquire(&a, "x", &cancel).await.unwrap();
        // Run prune far in the future. The bucket has an
        // in-flight permit, so prune must skip it.
        let far_future = Instant::now() + Duration::from_secs(10_000);
        cap.prune_stale(far_future);
        assert_eq!(cap.semaphore_count(), 1);
    }

    #[tokio::test]
    async fn prune_stale_drops_idle_entries() {
        let mut c = cfg(1);
        c.stale_ttl_secs = 60;
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(c).unwrap();
        let cancel = CancellationToken::new();
        let a = tid("tenant-a");
        {
            let _p = cap.acquire(&a, "x", &cancel).await.unwrap();
        } // permit dropped here, bucket is fully released
        assert_eq!(cap.semaphore_count(), 1);
        // Walk the clock past the TTL.
        cap.prune_stale(Instant::now() + Duration::from_secs(120));
        assert_eq!(cap.semaphore_count(), 0);
    }

    #[tokio::test]
    async fn evict_oldest_drops_lru_first_on_hard_cap() {
        let mut c = cfg(1);
        c.max_buckets = 5;
        let cap = PerPrincipalConcurrencyCap::new_without_sweeper(c).unwrap();
        let cancel = CancellationToken::new();
        // Insert 5 idle entries, then a 6th — eviction kicks in.
        for i in 0..5 {
            let t = tid(&format!("t-{i:02}"));
            let _p = cap.acquire(&t, "x", &cancel).await.unwrap();
        }
        assert_eq!(cap.semaphore_count(), 5);
        let new = tid("t-zz");
        let _p = cap.acquire(&new, "x", &cancel).await.unwrap();
        // 1 victim evicted on insert, then the new one added.
        assert_eq!(cap.semaphore_count(), 5);
    }
}

// --- Config ------------------------------------------------------

/// Per-tool concurrency override. `timeout_secs` is optional —
/// when None, the top-level `default_timeout_secs` applies.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerToolConcurrency {
    pub max_in_flight: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerPrincipalConcurrencyConfig {
    /// Defaults to `true` so an operator who reaches Phase 76.6
    /// is concurrency-capped by default. Opt out with
    /// `enabled: false`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_default_limit")]
    pub default: PerToolConcurrency,
    #[serde(default)]
    pub per_tool: BTreeMap<String, PerToolConcurrency>,
    /// Top-level fallback timeout. Used when the tool has no
    /// per-tool override AND `default.timeout_secs` is None.
    #[serde(default = "default_default_timeout_secs")]
    pub default_timeout_secs: u64,
    /// Max time a request will wait for a permit before the
    /// concurrency cap rejects it (`-32002`). 0 means "reject
    /// immediately" (no queue).
    #[serde(default = "default_queue_wait_ms")]
    pub queue_wait_ms: u64,
    #[serde(default = "default_max_buckets")]
    pub max_buckets: usize,
    #[serde(default = "default_stale_ttl_secs")]
    pub stale_ttl_secs: u64,
}

/// Hard upper bound on `max_in_flight`. Beyond this, an operator
/// is almost certainly mis-configuring (50_000 outstanding handler
/// futures per tool would blow the process). Mirrors the spirit
/// of `MAX_REQUEST_TIMEOUT_SECS` in `http_config.rs`.
pub const MAX_IN_FLIGHT_HARD_CAP: u32 = 10_000;

/// Hard upper bound on `*_timeout_secs`. Mirrors
/// `http_config::MAX_REQUEST_TIMEOUT_SECS` (10 min) — any handler
/// past this is almost certainly stuck.
pub const MAX_TIMEOUT_SECS: u64 = 600;

fn default_enabled() -> bool {
    true
}
fn default_default_limit() -> PerToolConcurrency {
    PerToolConcurrency {
        max_in_flight: 10,
        timeout_secs: None,
    }
}
fn default_default_timeout_secs() -> u64 {
    30
}
fn default_queue_wait_ms() -> u64 {
    5_000
}
fn default_max_buckets() -> usize {
    50_000
}
fn default_stale_ttl_secs() -> u64 {
    300
}

impl Default for PerPrincipalConcurrencyConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            default: default_default_limit(),
            per_tool: BTreeMap::new(),
            default_timeout_secs: default_default_timeout_secs(),
            queue_wait_ms: default_queue_wait_ms(),
            max_buckets: default_max_buckets(),
            stale_ttl_secs: default_stale_ttl_secs(),
        }
    }
}

impl PerPrincipalConcurrencyConfig {
    /// Boot validation. All errors fail-fast at server start so
    /// the operator never runs with a silently-broken cap (e.g.
    /// `max_in_flight: 0` would block every request forever).
    pub fn validate(&self) -> Result<(), String> {
        check_in_flight("default", self.default.max_in_flight)?;
        if let Some(ts) = self.default.timeout_secs {
            check_timeout("default", ts)?;
        }
        for (name, pt) in &self.per_tool {
            let label = format!("per_tool[{name}]");
            check_in_flight(&label, pt.max_in_flight)?;
            if let Some(ts) = pt.timeout_secs {
                check_timeout(&label, ts)?;
            }
        }
        if self.default_timeout_secs == 0 {
            return Err("default_timeout_secs must be > 0".into());
        }
        if self.default_timeout_secs > MAX_TIMEOUT_SECS {
            return Err(format!(
                "default_timeout_secs must be ≤ {MAX_TIMEOUT_SECS} (got {})",
                self.default_timeout_secs
            ));
        }
        if self.max_buckets == 0 {
            return Err("max_buckets must be > 0".into());
        }
        if self.stale_ttl_secs == 0 {
            return Err("stale_ttl_secs must be > 0".into());
        }
        // Waiting in queue longer than your own deadline is
        // dead code — the call would time out before the permit
        // ever arrives.
        let max_wait_ms = self.default_timeout_secs.saturating_mul(1000);
        if self.queue_wait_ms > max_wait_ms {
            return Err(format!(
                "queue_wait_ms ({}) must be ≤ default_timeout_secs * 1000 ({})",
                self.queue_wait_ms, max_wait_ms
            ));
        }
        Ok(())
    }

    /// Effective in-flight cap for `tool`. Per-tool override wins
    /// over `default`.
    pub fn cap_for(&self, tool: &str) -> u32 {
        self.per_tool
            .get(tool)
            .map(|pt| pt.max_in_flight)
            .unwrap_or(self.default.max_in_flight)
    }

    /// Effective per-call timeout for `tool`. Lookup priority:
    /// per_tool override → default override → top-level fallback.
    pub fn timeout_for(&self, tool: &str) -> Duration {
        let secs = self
            .per_tool
            .get(tool)
            .and_then(|pt| pt.timeout_secs)
            .or(self.default.timeout_secs)
            .unwrap_or(self.default_timeout_secs);
        Duration::from_secs(secs)
    }
}

fn check_in_flight(label: &str, n: u32) -> Result<(), String> {
    if n == 0 {
        return Err(format!("{label}.max_in_flight must be > 0"));
    }
    if n > MAX_IN_FLIGHT_HARD_CAP {
        return Err(format!(
            "{label}.max_in_flight ({n}) exceeds hard cap {MAX_IN_FLIGHT_HARD_CAP}"
        ));
    }
    Ok(())
}

fn check_timeout(label: &str, secs: u64) -> Result<(), String> {
    if secs == 0 {
        return Err(format!("{label}.timeout_secs must be > 0"));
    }
    if secs > MAX_TIMEOUT_SECS {
        return Err(format!(
            "{label}.timeout_secs ({secs}) exceeds hard cap {MAX_TIMEOUT_SECS}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn default_validates() {
        assert!(PerPrincipalConcurrencyConfig::default().validate().is_ok());
    }

    #[test]
    fn rejects_zero_max_in_flight() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.default.max_in_flight = 0;
        let err = c.validate().unwrap_err();
        assert!(err.contains("max_in_flight"), "{err}");
    }

    #[test]
    fn rejects_per_tool_zero_max() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.per_tool.insert(
            "agent_turn".into(),
            PerToolConcurrency {
                max_in_flight: 0,
                timeout_secs: None,
            },
        );
        let err = c.validate().unwrap_err();
        assert!(err.contains("per_tool[agent_turn]"), "{err}");
    }

    #[test]
    fn rejects_zero_default_timeout() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.default_timeout_secs = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_excessive_default_timeout() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.default_timeout_secs = MAX_TIMEOUT_SECS + 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_per_tool_excessive_timeout() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.per_tool.insert(
            "slow".into(),
            PerToolConcurrency {
                max_in_flight: 5,
                timeout_secs: Some(MAX_TIMEOUT_SECS + 1),
            },
        );
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_queue_wait_above_default_timeout() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.default_timeout_secs = 5;
        c.queue_wait_ms = 6_000; // 6s > 5s
        let err = c.validate().unwrap_err();
        assert!(err.contains("queue_wait_ms"), "{err}");
    }

    #[test]
    fn rejects_excessive_max_in_flight() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.default.max_in_flight = MAX_IN_FLIGHT_HARD_CAP + 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_zero_max_buckets() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.max_buckets = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn cap_for_falls_back_to_default() {
        let c = PerPrincipalConcurrencyConfig::default();
        assert_eq!(c.cap_for("anything"), 10);
    }

    #[test]
    fn cap_for_uses_per_tool_override() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.per_tool.insert(
            "heavy".into(),
            PerToolConcurrency {
                max_in_flight: 2,
                timeout_secs: None,
            },
        );
        assert_eq!(c.cap_for("heavy"), 2);
        assert_eq!(c.cap_for("light"), 10);
    }

    #[test]
    fn timeout_for_uses_default_when_per_tool_omits() {
        let c = PerPrincipalConcurrencyConfig::default();
        assert_eq!(c.timeout_for("any"), Duration::from_secs(30));
    }

    #[test]
    fn timeout_for_uses_per_tool_override_when_present() {
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.per_tool.insert(
            "long".into(),
            PerToolConcurrency {
                max_in_flight: 5,
                timeout_secs: Some(120),
            },
        );
        assert_eq!(c.timeout_for("long"), Duration::from_secs(120));
        assert_eq!(c.timeout_for("short"), Duration::from_secs(30));
    }

    #[test]
    fn timeout_for_default_block_override_beats_top_level() {
        // When `default.timeout_secs` is set it wins over the
        // top-level `default_timeout_secs` for tools without
        // a per-tool override.
        let mut c = PerPrincipalConcurrencyConfig::default();
        c.default.timeout_secs = Some(7);
        c.default_timeout_secs = 30;
        assert_eq!(c.timeout_for("any"), Duration::from_secs(7));
    }
}

#[cfg(test)]
mod entry_tests {
    use super::*;

    #[test]
    fn sem_entry_starts_with_full_permits() {
        let e = SemEntry::new(5);
        assert_eq!(e.sem.available_permits(), 5);
        assert_eq!(e.max_in_flight, 5);
    }

    #[tokio::test]
    async fn sem_entry_permits_round_trip() {
        let e = SemEntry::new(2);
        let p1 = Arc::clone(&e.sem).acquire_owned().await.unwrap();
        let p2 = Arc::clone(&e.sem).acquire_owned().await.unwrap();
        assert_eq!(e.sem.available_permits(), 0);
        drop(p1);
        assert_eq!(e.sem.available_permits(), 1);
        drop(p2);
        assert_eq!(e.sem.available_permits(), 2);
    }

    #[test]
    fn touch_advances_last_seen() {
        let e = SemEntry::new(1);
        let t0 = *e.last_seen.lock();
        let t1 = t0 + Duration::from_secs(5);
        e.touch(t1);
        assert_eq!(*e.last_seen.lock(), t1);
    }
}
