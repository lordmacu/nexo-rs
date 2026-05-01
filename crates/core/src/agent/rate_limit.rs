//! Phase 9.2 follow-up — non-blocking per-tool rate limiter.
//!
//! Token bucket keyed by `(agent, tool)`. The LLM loop calls
//! `try_acquire` before invoking a handler; denial surfaces as an
//! `outcome="rate_limited"` turn result so the model can pick a
//! different tool. No sleep-based backpressure here — the agent loop
//! must stay live.
//!
//! Phase 82.7 — config types unified with the yaml-side `nexo-config`
//! (`ToolRateLimitsConfig` + `ToolRateLimitSpec`) so a single shape
//! crosses operator → runtime without translation. Re-exported as
//! `ToolRateLimitConfig` for back-compat with pre-82.7 callers.
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::Mutex;

pub use nexo_config::types::agents::{ToolRateLimitSpec, ToolRateLimitsConfig};

/// Phase 9.2 alias — `ToolRateLimitSpec` is the canonical name in
/// `nexo-config` (yaml shape); this alias preserves the older
/// `ToolRateLimitConfig` symbol for downstream re-exports without
/// duplicating the definition.
pub type ToolRateLimitConfig = ToolRateLimitSpec;

/// Default cap on concurrent buckets. With a triple key
/// `(agent, binding_id, tool)` cardinality stays low (operators
/// rarely declare more than ~10 patterns × ~5 bindings × ~50
/// tools per agent), so 10k is generous.
pub const DEFAULT_MAX_BUCKETS: usize = 10_000;

/// Bucket key triple: `(agent, Option<binding_id>, tool)`.
/// `None` binding_id encodes the legacy single-tenant path so
/// pre-Phase-82.7 callers see identical semantics.
type BucketKey = (String, Option<String>, String);

pub struct ToolRateLimiter {
    /// Sorted (pattern, config) pairs; `_default` always last.
    patterns: Vec<(String, ToolRateLimitConfig)>,
    default: Option<ToolRateLimitConfig>,
    buckets: DashMap<BucketKey, TokenBucket>,
    /// Phase 82.7 — keys evicted by LRU whose config had
    /// `essential_deny_on_miss=true`. The next allocation for
    /// such a key consumes the entry and returns false ONCE,
    /// then proceeds normally. Mirrors claude-code-leak's
    /// `policyLimits::isPolicyAllowed` ESSENTIAL deny-on-miss
    /// pattern adapted to LRU eviction.
    evicted_essential: DashMap<BucketKey, ()>,
    max_buckets: usize,
    /// Monotonic counter stamped on every `try_acquire` so we can
    /// pick the stalest bucket without taking any lock. Mirror
    /// `SenderRateLimiter::clock`.
    clock: AtomicU64,
}
struct TokenBucket {
    capacity: u64,
    rate_per_sec: f64,
    /// Phase 82.7 — `true` when config opts in to fail-closed on
    /// LRU eviction. Pinned in the bucket so `enforce_cap` can
    /// flag the key for `evicted_essential` insertion before
    /// removing.
    essential_deny_on_miss: bool,
    tokens: AtomicU64,
    last_refill: Mutex<Instant>,
    last_touch: AtomicU64,
}
impl TokenBucket {
    fn new(cfg: &ToolRateLimitConfig, touch: u64) -> Self {
        let capacity = cfg.effective_burst();
        Self {
            capacity,
            rate_per_sec: cfg.rps.max(0.0),
            essential_deny_on_miss: cfg.essential_deny_on_miss,
            tokens: AtomicU64::new(capacity),
            last_refill: Mutex::new(Instant::now()),
            last_touch: AtomicU64::new(touch),
        }
    }
    /// Atomically try to subtract one token. Refills lazily from elapsed
    /// time since `last_refill`. Returns true if allowed.
    async fn try_acquire(&self, touch: u64) -> bool {
        self.last_touch.store(touch, Ordering::Relaxed);
        let now = Instant::now();
        // Refill step under lock; keep critical section small.
        {
            let mut last = self.last_refill.lock().await;
            let elapsed = now.duration_since(*last).as_secs_f64();
            if elapsed > 0.0 && self.rate_per_sec > 0.0 {
                let add = (elapsed * self.rate_per_sec).floor() as u64;
                if add > 0 {
                    let current = self.tokens.load(Ordering::Relaxed);
                    let new = current.saturating_add(add).min(self.capacity);
                    self.tokens.store(new, Ordering::Relaxed);
                    // Advance last_refill proportionally to tokens added
                    // to avoid starvation from rounding loss.
                    let consumed_secs = add as f64 / self.rate_per_sec;
                    *last += std::time::Duration::from_secs_f64(consumed_secs);
                }
            }
        }
        let mut current = self.tokens.load(Ordering::Acquire);
        loop {
            if current == 0 {
                return false;
            }
            match self.tokens.compare_exchange(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }
}
impl ToolRateLimiter {
    pub fn new(cfg: ToolRateLimitsConfig) -> Self {
        Self::with_cap(cfg, DEFAULT_MAX_BUCKETS)
    }

    /// Build with a custom bucket cap. Production callers use
    /// `new`; tests cover eviction behaviour with smaller caps.
    pub fn with_cap(cfg: ToolRateLimitsConfig, max_buckets: usize) -> Self {
        let mut sorted: Vec<(String, ToolRateLimitConfig)> = cfg
            .patterns
            .iter()
            .filter(|(k, _)| k.as_str() != "_default")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Deterministic order (alpha) so tests are stable.
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let default = cfg.patterns.get("_default").cloned();
        Self {
            patterns: sorted,
            default,
            buckets: DashMap::new(),
            evicted_essential: DashMap::new(),
            max_buckets,
            clock: AtomicU64::new(0),
        }
    }

    /// Resolve the effective config for a given tool name from the
    /// global pattern set. Per-binding overrides bypass this via
    /// `resolve_in_override`.
    fn resolve_global(&self, tool: &str) -> Option<&ToolRateLimitConfig> {
        for (pat, cfg) in &self.patterns {
            if glob_matches(pat, tool) {
                return Some(cfg);
            }
        }
        self.default.as_ref()
    }

    /// Resolve from a per-binding override map (Phase 82.7). First
    /// glob match wins, then `_default`, else `None` (= unlimited
    /// for this tool on this binding — does NOT fall back to
    /// global, per spec semantic "per-binding fully replaces").
    fn resolve_in_override<'a>(
        &self,
        tool: &str,
        over: &'a ToolRateLimitsConfig,
    ) -> Option<ToolRateLimitConfig> {
        // Iterate in deterministic alpha order to mirror global
        // behaviour. Skip `_default` until last.
        let mut explicit: Vec<(&String, &ToolRateLimitSpec)> = over
            .patterns
            .iter()
            .filter(|(k, _)| k.as_str() != "_default")
            .collect();
        explicit.sort_by(|a, b| a.0.cmp(b.0));
        for (pat, cfg) in explicit {
            if glob_matches(pat, tool) {
                return Some(cfg.clone());
            }
        }
        over.patterns.get("_default").cloned()
    }

    /// Phase 9.2 back-compat — single-tenant entry point with no
    /// binding scope. Equivalent to
    /// `try_acquire_with_binding(agent, None, tool, None)`.
    pub async fn try_acquire(&self, agent: &str, tool: &str) -> bool {
        self.try_acquire_with_binding(agent, None, tool, None).await
    }

    /// Phase 82.7 — canonical entry point.
    ///
    /// Resolution priority:
    /// 1. `per_binding_override` is `Some` → look up tool in that
    ///    map only (no fall-through to global). `binding_id` is
    ///    stamped into the bucket key so each binding gets its
    ///    own bucket cardinality.
    /// 2. Else → look up in global `self.patterns` /
    ///    `self.default`. Bucket key uses `None` binding scope
    ///    (legacy single-tenant cell).
    /// 3. No match → unlimited (return `true` without bucket
    ///    allocation).
    pub async fn try_acquire_with_binding(
        &self,
        agent: &str,
        binding_id: Option<&str>,
        tool: &str,
        per_binding_override: Option<&ToolRateLimitsConfig>,
    ) -> bool {
        let (cfg, scoped_binding) = match per_binding_override {
            Some(over) => match self.resolve_in_override(tool, over) {
                Some(cfg) => (cfg, binding_id.map(String::from)),
                None => return true, // override + no match → unlimited (no global fallthrough)
            },
            None => match self.resolve_global(tool) {
                Some(cfg) => (cfg.clone(), None),
                None => return true,
            },
        };

        let key: BucketKey = (agent.to_string(), scoped_binding, tool.to_string());
        let touch = self.clock.fetch_add(1, Ordering::Relaxed);

        // Phase 82.7 — fail-closed deny-on-miss for keys that
        // were LRU-evicted while their config had
        // essential_deny_on_miss. Consume the entry and deny
        // ONCE; the next call allocates a fresh bucket.
        if self.evicted_essential.remove(&key).is_some() {
            return false;
        }

        if !self.buckets.contains_key(&key) {
            self.enforce_cap();
        }
        let entry = self
            .buckets
            .entry(key)
            .or_insert_with(|| TokenBucket::new(&cfg, touch));
        entry.value().try_acquire(touch).await
    }

    /// LRU eviction — drops the stalest bucket(s) until under
    /// `max_buckets`. Stalest is determined by `last_touch`. If
    /// the evicted bucket's config had `essential_deny_on_miss`,
    /// stamps the key into `evicted_essential` so the next
    /// allocation denies once before reallocating.
    fn enforce_cap(&self) {
        if self.max_buckets == 0 {
            return;
        }
        while self.buckets.len() >= self.max_buckets {
            let oldest = self
                .buckets
                .iter()
                .min_by_key(|e| e.value().last_touch.load(Ordering::Relaxed))
                .map(|e| (e.key().clone(), e.value().essential_deny_on_miss));
            match oldest {
                Some((k, was_essential)) => {
                    if self.buckets.remove(&k).is_some() {
                        if was_essential {
                            self.evicted_essential.insert(k.clone(), ());
                        }
                        tracing::debug!(
                            evicted_agent = %k.0,
                            evicted_binding = ?k.1,
                            evicted_tool = %k.2,
                            essential = was_essential,
                            cap = self.max_buckets,
                            "tool rate-limit bucket cap reached; evicted stalest"
                        );
                    }
                }
                None => break,
            }
        }
    }

    /// Active bucket count — useful for telemetry / metrics. Not
    /// part of the hot path.
    pub fn active_buckets(&self) -> usize {
        self.buckets.len()
    }

    /// Phase 82.7 — drop every bucket belonging to `agent`. Used
    /// by admin RPC delete-agent path (Phase 82.10) so
    /// `(agent, *, *)` cells don't leak after an operator removes
    /// the agent. Iteration is O(N) — only fires on admin ops, not
    /// in the hot path.
    pub fn drop_buckets_for_agent(&self, agent: &str) {
        let to_remove: Vec<BucketKey> = self
            .buckets
            .iter()
            .filter_map(|e| {
                if e.key().0 == agent {
                    Some(e.key().clone())
                } else {
                    None
                }
            })
            .collect();
        for k in to_remove {
            self.buckets.remove(&k);
            self.evicted_essential.remove(&k);
        }
    }
}
/// Minimal `*` glob: `foo*bar` matches any string starting `foo` and
/// ending `bar`. `*` alone matches anything.
pub fn glob_matches(pattern: &str, s: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == s;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0usize;
    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if idx == 0 {
            if !s[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if idx == parts.len() - 1 && !pattern.ends_with('*') {
            return s[pos..].ends_with(part);
        } else {
            match s[pos..].find(part) {
                Some(i) => pos += i + part.len(),
                None => return false,
            }
        }
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    #[test]
    fn glob_matches_exact_and_wildcards() {
        assert!(glob_matches("foo", "foo"));
        assert!(!glob_matches("foo", "bar"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("foo*", "foobar"));
        assert!(!glob_matches("foo*", "barfoo"));
        assert!(glob_matches("*bar", "foobar"));
        assert!(glob_matches("foo*bar", "foobazbar"));
        assert!(glob_matches("*mid*", "leftmidright"));
        assert!(!glob_matches("foo*bar", "foobaz"));
    }
    #[tokio::test]
    async fn token_bucket_burst_then_refill() {
        let cfg = ToolRateLimitConfig {
            rps: 10.0,
            burst: 3,
            essential_deny_on_miss: false,
        };
        let b = TokenBucket::new(&cfg, 0);
        assert!(b.try_acquire(1).await);
        assert!(b.try_acquire(2).await);
        assert!(b.try_acquire(3).await);
        assert!(!b.try_acquire(4).await);
        // Wait ~150ms; rate 10/s → ~1 token refilled.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(b.try_acquire(5).await);
    }
    #[tokio::test]
    async fn limiter_no_config_always_allows() {
        let rl = ToolRateLimiter::new(ToolRateLimitsConfig::default());
        for _ in 0..100 {
            assert!(rl.try_acquire("kate", "any_tool").await);
        }
    }
    #[tokio::test]
    async fn limiter_selects_first_matching_pattern() {
        let mut patterns = HashMap::new();
        patterns.insert(
            "memory_*".to_string(),
            ToolRateLimitConfig { rps: 1.0, burst: 1, essential_deny_on_miss: false },
        );
        patterns.insert(
            "_default".to_string(),
            ToolRateLimitConfig {
                rps: 100.0,
                burst: 100,
                essential_deny_on_miss: false,
            },
        );
        let rl = ToolRateLimiter::new(ToolRateLimitsConfig { patterns });
        // memory_recall uses memory_* → burst=1
        assert!(rl.try_acquire("kate", "memory_recall").await);
        assert!(!rl.try_acquire("kate", "memory_recall").await);
        // mcp_fs_read hits _default → burst=100
        for _ in 0..50 {
            assert!(rl.try_acquire("kate", "mcp_fs_read").await);
        }
    }
    #[tokio::test]
    async fn limiter_keys_per_agent_tool_independently() {
        let mut patterns = HashMap::new();
        patterns.insert(
            "mcp_*".to_string(),
            ToolRateLimitConfig { rps: 1.0, burst: 1, essential_deny_on_miss: false },
        );
        let rl = ToolRateLimiter::new(ToolRateLimitsConfig { patterns });
        assert!(rl.try_acquire("a", "mcp_fs_read").await);
        assert!(rl.try_acquire("b", "mcp_fs_read").await);
        assert!(rl.try_acquire("a", "mcp_fs_write").await);
        assert!(!rl.try_acquire("a", "mcp_fs_read").await);
    }

    /// Phase 82.7 — per-binding override allocates an independent
    /// bucket so binding A's tight cap does not affect binding B
    /// (which falls back to "no override → unlimited" because its
    /// `per_binding_override` is `None`).
    #[tokio::test]
    async fn per_binding_override_uses_independent_bucket() {
        let rl = ToolRateLimiter::new(ToolRateLimitsConfig::default());

        let mut over_a_patterns = HashMap::new();
        over_a_patterns.insert(
            "marketing_*".into(),
            ToolRateLimitSpec {
                rps: 1.0,
                burst: 1,
                essential_deny_on_miss: false,
            },
        );
        let over_a = ToolRateLimitsConfig { patterns: over_a_patterns };

        // Binding A: cap 1, second call denied.
        assert!(
            rl.try_acquire_with_binding("ana", Some("wa:free"), "marketing_send", Some(&over_a))
                .await
        );
        assert!(
            !rl.try_acquire_with_binding("ana", Some("wa:free"), "marketing_send", Some(&over_a))
                .await
        );

        // Binding B: no override → unlimited (separate bucket).
        for _ in 0..50 {
            assert!(
                rl.try_acquire_with_binding("ana", Some("wa:enterprise"), "marketing_send", None)
                    .await
            );
        }
    }

    /// Phase 82.7 — per-binding override fully replaces global
    /// (no fall-through). Tool not in override map → unlimited
    /// even if global pattern would catch it.
    #[tokio::test]
    async fn per_binding_no_match_falls_to_unlimited_no_global_fallthrough() {
        let mut global_patterns = HashMap::new();
        global_patterns.insert(
            "_default".into(),
            ToolRateLimitSpec {
                rps: 1.0,
                burst: 1,
                essential_deny_on_miss: false,
            },
        );
        let rl = ToolRateLimiter::new(ToolRateLimitsConfig { patterns: global_patterns });

        let mut over_patterns = HashMap::new();
        over_patterns.insert(
            "marketing_*".into(),
            ToolRateLimitSpec {
                rps: 1.0,
                burst: 1,
                essential_deny_on_miss: false,
            },
        );
        let over = ToolRateLimitsConfig { patterns: over_patterns };

        // `read_state` does NOT match override pattern; per-binding
        // semantic = unlimited (NOT fall back to global `_default`).
        for _ in 0..50 {
            assert!(
                rl.try_acquire_with_binding("ana", Some("wa:free"), "read_state", Some(&over))
                    .await
            );
        }
    }

    /// Phase 82.7 — `drop_buckets_for_agent` removes only the
    /// target agent's entries, leaving other agents intact.
    #[tokio::test]
    async fn drop_buckets_for_agent_clears_only_target() {
        let mut patterns = HashMap::new();
        patterns.insert(
            "*".into(),
            ToolRateLimitSpec {
                rps: 1.0,
                burst: 1,
                essential_deny_on_miss: false,
            },
        );
        let rl = ToolRateLimiter::new(ToolRateLimitsConfig { patterns });
        rl.try_acquire("ana", "tool_a").await;
        rl.try_acquire("bob", "tool_a").await;
        assert_eq!(rl.active_buckets(), 2);

        rl.drop_buckets_for_agent("ana");
        assert_eq!(rl.active_buckets(), 1);
        // bob's bucket persists — second acquire still denied (cap 1).
        assert!(!rl.try_acquire("bob", "tool_a").await);
        // ana's bucket cleared — fresh allocation, single token allowed.
        assert!(rl.try_acquire("ana", "tool_a").await);
    }

    /// Phase 82.7 — LRU eviction drops the stalest bucket once
    /// `max_buckets` is reached.
    #[tokio::test]
    async fn lru_eviction_drops_stalest_bucket_at_cap() {
        let mut patterns = HashMap::new();
        patterns.insert(
            "*".into(),
            ToolRateLimitSpec {
                rps: 100.0,
                burst: 10,
                essential_deny_on_miss: false,
            },
        );
        // Cap 2 — third unique key triggers eviction.
        let rl = ToolRateLimiter::with_cap(ToolRateLimitsConfig { patterns }, 2);
        rl.try_acquire("a1", "tool_a").await;
        rl.try_acquire("a2", "tool_a").await;
        assert_eq!(rl.active_buckets(), 2);

        // Touch a2 again to make a1 the stalest.
        rl.try_acquire("a2", "tool_a").await;
        // Allocate third key → a1 evicted.
        rl.try_acquire("a3", "tool_a").await;
        assert_eq!(rl.active_buckets(), 2);
        // Re-acquiring a1 allocates fresh bucket (full burst).
        assert!(rl.try_acquire("a1", "tool_a").await);
    }

    /// Phase 82.7 — `essential_deny_on_miss` makes the next
    /// allocation fail-closed when the bucket was evicted under
    /// LRU pressure. After consuming the deny token, subsequent
    /// allocations proceed normally.
    #[tokio::test]
    async fn essential_deny_on_miss_blocks_after_eviction() {
        let mut patterns = HashMap::new();
        patterns.insert(
            "*".into(),
            ToolRateLimitSpec {
                rps: 100.0,
                burst: 10,
                essential_deny_on_miss: true,
            },
        );
        let rl = ToolRateLimiter::with_cap(ToolRateLimitsConfig { patterns }, 2);
        // Allocate 2 buckets (cap full).
        rl.try_acquire("a1", "marketing_send").await;
        rl.try_acquire("a2", "marketing_send").await;
        // Touch a2 so a1 is stalest.
        rl.try_acquire("a2", "marketing_send").await;
        // Allocate third → a1 evicted with essential flag.
        rl.try_acquire("a3", "marketing_send").await;

        // a1 was essential → next allocation MUST deny once.
        assert!(!rl.try_acquire("a1", "marketing_send").await);
        // Subsequent calls allocate fresh bucket and proceed.
        assert!(rl.try_acquire("a1", "marketing_send").await);
    }

    /// Phase 82.7 — back-compat wrapper preserves pre-82.7
    /// `try_acquire(agent, tool)` semantics: bucket key uses
    /// `None` binding scope (legacy single-tenant cell).
    #[tokio::test]
    async fn legacy_try_acquire_keeps_pre_82_7_behavior() {
        let mut patterns = HashMap::new();
        patterns.insert(
            "tool_a".into(),
            ToolRateLimitSpec {
                rps: 1.0,
                burst: 1,
                essential_deny_on_miss: false,
            },
        );
        let rl = ToolRateLimiter::new(ToolRateLimitsConfig { patterns });
        assert!(rl.try_acquire("ana", "tool_a").await);
        // Wrapper hits same bucket as binding-aware None/None.
        assert!(
            !rl.try_acquire_with_binding("ana", None, "tool_a", None)
                .await
        );
    }
}
