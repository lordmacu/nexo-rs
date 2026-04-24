//! Phase 9.2 follow-up — non-blocking per-tool rate limiter.
//!
//! Token bucket keyed by `(agent, tool)`. The LLM loop calls
//! `try_acquire` before invoking a handler; denial surfaces as an
//! `outcome="rate_limited"` turn result so the model can pick a
//! different tool. No sleep-based backpressure here — the agent loop
//! must stay live.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use dashmap::DashMap;
use serde::Deserialize;
use tokio::sync::Mutex;
/// Per-tool config: requests-per-second refill rate and burst capacity.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRateLimitConfig {
    pub rps: f64,
    #[serde(default)]
    pub burst: u64,
}
impl ToolRateLimitConfig {
    fn effective_burst(&self) -> u64 {
        if self.burst == 0 {
            self.rps.ceil().max(1.0) as u64
        } else {
            self.burst
        }
    }
}
/// Map of glob patterns to limits. `_default` is a reserved pattern that
/// applies when nothing else matches.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRateLimitsConfig {
    #[serde(default)]
    pub patterns: HashMap<String, ToolRateLimitConfig>,
}
pub struct ToolRateLimiter {
    /// Sorted (pattern, config) pairs; `_default` always last.
    patterns: Vec<(String, ToolRateLimitConfig)>,
    default: Option<ToolRateLimitConfig>,
    buckets: DashMap<(String, String), TokenBucket>,
}
struct TokenBucket {
    capacity: u64,
    rate_per_sec: f64,
    tokens: AtomicU64,
    last_refill: Mutex<Instant>,
}
impl TokenBucket {
    fn new(cfg: &ToolRateLimitConfig) -> Self {
        let capacity = cfg.effective_burst();
        Self {
            capacity,
            rate_per_sec: cfg.rps.max(0.0),
            tokens: AtomicU64::new(capacity),
            last_refill: Mutex::new(Instant::now()),
        }
    }
    /// Atomically try to subtract one token. Refills lazily from elapsed
    /// time since `last_refill`. Returns true if allowed.
    async fn try_acquire(&self) -> bool {
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
                    *last = *last + std::time::Duration::from_secs_f64(consumed_secs);
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
        }
    }
    /// Resolve the effective config for a given tool name — first glob
    /// match wins, else `_default`, else None (unlimited).
    fn resolve(&self, tool: &str) -> Option<&ToolRateLimitConfig> {
        for (pat, cfg) in &self.patterns {
            if glob_matches(pat, tool) {
                return Some(cfg);
            }
        }
        self.default.as_ref()
    }
    /// Non-blocking bucket acquisition. `true` = allowed; `false` =
    /// rate-limited (caller should skip the handler).
    pub async fn try_acquire(&self, agent: &str, tool: &str) -> bool {
        let Some(cfg) = self.resolve(tool) else {
            return true;
        };
        let key = (agent.to_string(), tool.to_string());
        let entry = self
            .buckets
            .entry(key)
            .or_insert_with(|| TokenBucket::new(cfg));
        entry.value().try_acquire().await
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
        let cfg = ToolRateLimitConfig { rps: 10.0, burst: 3 };
        let b = TokenBucket::new(&cfg);
        assert!(b.try_acquire().await);
        assert!(b.try_acquire().await);
        assert!(b.try_acquire().await);
        assert!(!b.try_acquire().await);
        // Wait ~150ms; rate 10/s → ~1 token refilled.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(b.try_acquire().await);
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
            ToolRateLimitConfig { rps: 1.0, burst: 1 },
        );
        patterns.insert(
            "_default".to_string(),
            ToolRateLimitConfig { rps: 100.0, burst: 100 },
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
            ToolRateLimitConfig { rps: 1.0, burst: 1 },
        );
        let rl = ToolRateLimiter::new(ToolRateLimitsConfig { patterns });
        assert!(rl.try_acquire("a", "mcp_fs_read").await);
        assert!(rl.try_acquire("b", "mcp_fs_read").await);
        assert!(rl.try_acquire("a", "mcp_fs_write").await);
        assert!(!rl.try_acquire("a", "mcp_fs_read").await);
    }
}
