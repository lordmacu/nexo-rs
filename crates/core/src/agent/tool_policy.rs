//! Tool-execution policy: caching + parallel-safety.
//!
//! Attached to `LlmAgentBehavior` as an `Arc<ToolPolicy>`; the behavior
//! consults it on every tool call to decide whether to dedupe via
//! cache and whether a batch of calls may run concurrently.
//!
//! Names are matched with a tiny glob syntax (`*` at end only — we
//! don't need full PCRE here and avoiding a regex dep keeps this lean).
//!
//! Zero impact on `ToolDef` or the public tool registry — this is a
//! sidecar lookup keyed by `name`.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use dashmap::DashMap;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
/// YAML shape loaded from `config/tool_policy.yaml`. Optional file —
/// when absent the policy is "cache nothing, nothing parallel" (i.e.
/// current behavior, fully back-compat).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolPolicyConfig {
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub parallel_safe: Vec<String>,
    /// Bounds for the `join_all`-style parallel executor — protects
    /// both the local process and downstream HTTP endpoints from being
    /// hammered by a 20-tool tool-call batch.
    #[serde(default)]
    pub parallel: ParallelConfig,
    /// Phase 3 — TF-cosine relevance filter. When `enabled`, the agent
    /// only sends top-K ranked tools to the LLM per turn instead of
    /// the full catalog.
    #[serde(default)]
    pub relevance: super::tool_filter::RelevanceConfig,
    /// Per-agent overrides. When an agent_id matches a key, that
    /// override **fully replaces** the global config for the agent
    /// (no deep merge — explicit and predictable). Missing agents fall
    /// through to the top-level settings.
    #[serde(default)]
    pub per_agent: HashMap<String, AgentPolicyOverride>,
}
/// Same shape as `ToolPolicyConfig` minus `per_agent` (no recursion).
/// Declared separately so YAML can't nest overrides inside overrides.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicyOverride {
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub parallel_safe: Vec<String>,
    #[serde(default)]
    pub parallel: ParallelConfig,
    #[serde(default)]
    pub relevance: super::tool_filter::RelevanceConfig,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParallelConfig {
    /// Max number of parallel_safe calls in flight at once. `None`/`0`
    /// means unbounded (original `join_all` behavior).
    #[serde(default = "default_parallel_limit")]
    pub max_in_flight: usize,
    /// Per-call timeout in seconds. A slow call cancels and returns
    /// an error result instead of blocking the batch.
    #[serde(default = "default_parallel_timeout_secs")]
    pub call_timeout_secs: u64,
}
impl Default for ParallelConfig {
    fn default() -> Self {
        Self {
            max_in_flight: default_parallel_limit(),
            call_timeout_secs: default_parallel_timeout_secs(),
        }
    }
}
fn default_parallel_limit() -> usize { 4 }
fn default_parallel_timeout_secs() -> u64 { 30 }
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,
    #[serde(default)]
    pub tools: Vec<String>,
    /// Soft cap — drop oldest entries when exceeded. Prevents unbounded
    /// growth for pathological call patterns.
    #[serde(default = "default_max_entries")]
    pub max_entries: usize,
    /// Per-entry bytesize cap. A tool result whose JSON serialization
    /// exceeds this limit skips the cache entirely — protects against
    /// OOM when a cacheable tool returns a pathological payload (file
    /// bytes, scraped HTML, megabyte LLM summaries). `0` disables the
    /// check.
    #[serde(default = "default_max_value_bytes")]
    pub max_value_bytes: usize,
}
impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl_secs: default_ttl_secs(),
            tools: Vec::new(),
            max_entries: default_max_entries(),
            max_value_bytes: default_max_value_bytes(),
        }
    }
}
fn default_ttl_secs() -> u64 {
    60
}
fn default_max_entries() -> usize {
    1024
}
fn default_max_value_bytes() -> usize {
    256 * 1024
}
pub struct ToolPolicy {
    cache_patterns: Vec<GlobPattern>,
    parallel_patterns: Vec<GlobPattern>,
    ttl: Duration,
    max_entries: usize,
    max_value_bytes: usize,
    cache: DashMap<CacheKey, CacheEntry>,
    relevance: super::tool_filter::RelevanceConfig,
    parallel: ParallelConfig,
}
/// Cache key. The `agent_id` prefix partitions cache by agent so two
/// agents with different scopes (different auth, different workspace)
/// don't serve each other's results. The empty string `""` is a valid
/// agent_id for tests / bootstrap paths that don't have one.
type CacheKey = (String, String, u64); // (agent_id, tool_name, args_hash)
struct CacheEntry {
    value: Value,
    stored_at: Instant,
}
impl ToolPolicy {
    pub fn from_config(cfg: &ToolPolicyConfig) -> Arc<Self> {
        Arc::new(Self {
            cache_patterns: cfg.cache.tools.iter().map(|p| GlobPattern::new(p)).collect(),
            parallel_patterns: cfg.parallel_safe.iter().map(|p| GlobPattern::new(p)).collect(),
            ttl: Duration::from_secs(cfg.cache.ttl_secs),
            max_entries: cfg.cache.max_entries,
            max_value_bytes: cfg.cache.max_value_bytes,
            cache: DashMap::new(),
            relevance: cfg.relevance.clone(),
            parallel: cfg.parallel.clone(),
        })
    }
    pub fn parallel_config(&self) -> &ParallelConfig {
        &self.parallel
    }
    /// Empty policy — nothing cached, nothing parallel. Use when no
    /// `tool_policy.yaml` file is present.
    pub fn disabled() -> Arc<Self> {
        Self::from_config(&ToolPolicyConfig::default())
    }
    pub fn is_cacheable(&self, tool_name: &str) -> bool {
        self.cache_patterns.iter().any(|p| p.matches(tool_name))
    }
    pub fn is_parallel_safe(&self, tool_name: &str) -> bool {
        self.parallel_patterns.iter().any(|p| p.matches(tool_name))
    }
    /// Relevance config held for `ToolFilter::build` at turn start.
    pub fn relevance_config(&self) -> &super::tool_filter::RelevanceConfig {
        &self.relevance
    }
    /// Return the cached result if still fresh. Consumes expired
    /// entries opportunistically — a miss on expiry also frees the
    /// slot so `max_entries` stays honest. Bumps the `hit` / `miss`
    /// counter via `telemetry::inc_tool_cache_event`.
    pub fn cache_get(&self, agent_id: &str, tool_name: &str, args: &Value) -> Option<Value> {
        if !self.is_cacheable(tool_name) {
            return None;
        }
        let key = (
            agent_id.to_string(),
            tool_name.to_string(),
            hash_args(args),
        );
        let entry = match self.cache.get(&key) {
            Some(e) => e,
            None => {
                crate::telemetry::inc_tool_cache_event(agent_id, tool_name, "miss");
                return None;
            }
        };
        if entry.stored_at.elapsed() <= self.ttl {
            let v = entry.value.clone();
            drop(entry);
            crate::telemetry::inc_tool_cache_event(agent_id, tool_name, "hit");
            Some(v)
        } else {
            drop(entry);
            self.cache.remove(&key);
            crate::telemetry::inc_tool_cache_event(agent_id, tool_name, "miss");
            None
        }
    }
    pub fn cache_put(&self, agent_id: &str, tool_name: &str, args: &Value, value: Value) {
        if !self.is_cacheable(tool_name) {
            return;
        }
        // Bytesize guard — a cacheable tool that returns a megabyte
        // payload (scraped page, file bytes, big LLM dump) shouldn't
        // be allowed to pin 256KB × 1024 = 256MB of RSS. Serialize once,
        // bail if over cap. `0` disables the check.
        if self.max_value_bytes > 0 {
            let approx = serde_json::to_vec(&value).map(|b| b.len()).unwrap_or(0);
            if approx > self.max_value_bytes {
                crate::telemetry::inc_tool_cache_event(agent_id, tool_name, "skip_size");
                return;
            }
        }
        let key = (
            agent_id.to_string(),
            tool_name.to_string(),
            hash_args(args),
        );
        // LRU eviction: when at capacity, scan for the oldest
        // `stored_at` and drop it. O(n) on a 1024-entry map is
        // negligible (~100μs) and guarantees the eviction order is
        // actually bounded, unlike the previous "first entry" policy
        // which could churn the same shard over and over.
        if self.cache.len() >= self.max_entries {
            let mut oldest_key: Option<CacheKey> = None;
            let mut oldest_t: Option<Instant> = None;
            for entry in self.cache.iter() {
                let t = entry.value().stored_at;
                if oldest_t.map(|ot| t < ot).unwrap_or(true) {
                    oldest_t = Some(t);
                    oldest_key = Some(entry.key().clone());
                }
            }
            if let Some(k) = oldest_key {
                let evicted_tool = k.1.clone();
                let evicted_agent = k.0.clone();
                self.cache.remove(&k);
                crate::telemetry::inc_tool_cache_event(
                    &evicted_agent,
                    &evicted_tool,
                    "evict",
                );
            }
        }
        self.cache.insert(
            key,
            CacheEntry {
                value,
                stored_at: Instant::now(),
            },
        );
        crate::telemetry::inc_tool_cache_event(agent_id, tool_name, "put");
    }
    /// Partition a list of tool calls into two groups: parallel-safe
    /// (can all run concurrently) and sequential (must run in order
    /// one after another). Order is preserved so the LLM's original
    /// call order stays consistent for logs and tool_use ids.
    pub fn partition<'a, T>(&self, calls: &'a [T], name_of: impl Fn(&T) -> &str) -> (Vec<&'a T>, Vec<&'a T>) {
        let mut par = Vec::new();
        let mut seq = Vec::new();
        for c in calls {
            if self.is_parallel_safe(name_of(c)) {
                par.push(c);
            } else {
                seq.push(c);
            }
        }
        (par, seq)
    }
    /// Drain cached entries older than `ttl`. Good to call from a
    /// background heartbeat so the map doesn't bloat when traffic
    /// drops but `max_entries` hasn't kicked in.
    pub fn sweep_expired(&self) {
        let ttl = self.ttl;
        self.cache.retain(|_, entry| entry.stored_at.elapsed() <= ttl);
    }
    /// Drop every entry matching `(agent_id, tool_name)`. Ops hook —
    /// when an underlying plugin changes behavior (new API version,
    /// prompt tweak, config reload) we want the next call to re-hit
    /// the source of truth instead of serving a stale answer. Pattern
    /// match is exact on the tuple — callers do wildcard expansion
    /// themselves if they need it.
    pub fn cache_invalidate(&self, agent_id: &str, tool_name: &str) -> usize {
        let mut keys = Vec::new();
        for entry in self.cache.iter() {
            if entry.key().0 == agent_id && entry.key().1 == tool_name {
                keys.push(entry.key().clone());
            }
        }
        let n = keys.len();
        for k in keys {
            self.cache.remove(&k);
        }
        if n > 0 {
            crate::telemetry::inc_tool_cache_event(agent_id, tool_name, "invalidate");
        }
        n
    }
    /// Drop every cached entry. Returns the count removed. Used by
    /// the admin HTTP path and on hot-reload of tool_policy.yaml.
    pub fn cache_clear(&self) -> usize {
        let n = self.cache.len();
        self.cache.clear();
        if n > 0 {
            crate::telemetry::inc_tool_cache_event("*", "*", "invalidate");
        }
        n
    }
    /// Current number of entries in the cache. Used by the admin
    /// endpoint to report utilisation and by tests for assertions.
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }
}
// ── Registry ─────────────────────────────────────────────────────────────
/// Owns the default `ToolPolicy` plus a per-agent map. Agents get their
/// own `Arc<ToolPolicy>` via `for_agent`, so cache stores, parallel
/// caps, and relevance filters are all isolated per agent when an
/// override exists. Agents without an override share the default `Arc`
/// (cheap — clone, no duplication).
pub struct ToolPolicyRegistry {
    default: Arc<ToolPolicy>,
    per_agent: HashMap<String, Arc<ToolPolicy>>,
}
impl ToolPolicyRegistry {
    pub fn from_config(cfg: &ToolPolicyConfig) -> Arc<Self> {
        let default = ToolPolicy::from_config(cfg);
        let mut per_agent = HashMap::new();
        for (agent_id, ovr) in &cfg.per_agent {
            let resolved = ToolPolicyConfig {
                cache: ovr.cache.clone(),
                parallel_safe: ovr.parallel_safe.clone(),
                parallel: ovr.parallel.clone(),
                relevance: ovr.relevance.clone(),
                per_agent: HashMap::new(),
            };
            per_agent.insert(agent_id.clone(), ToolPolicy::from_config(&resolved));
        }
        Arc::new(Self { default, per_agent })
    }
    pub fn disabled() -> Arc<Self> {
        Self::from_config(&ToolPolicyConfig::default())
    }
    /// Resolve the policy for an agent. Unknown agents get the default.
    pub fn for_agent(&self, agent_id: &str) -> Arc<ToolPolicy> {
        self.per_agent
            .get(agent_id)
            .cloned()
            .unwrap_or_else(|| Arc::clone(&self.default))
    }
    /// Sweep every policy's cache. Called by the background tick.
    pub fn sweep_expired(&self) {
        self.default.sweep_expired();
        for p in self.per_agent.values() {
            p.sweep_expired();
        }
    }
    /// Clear every policy's cache. Ops hook — on hot-reload of the
    /// yaml or manual `POST /admin/cache/clear`.
    pub fn cache_clear_all(&self) -> usize {
        let mut n = self.default.cache_clear();
        for p in self.per_agent.values() {
            n += p.cache_clear();
        }
        n
    }
    /// Summed cache entry count across default and overrides. Reported
    /// by `GET /admin/tool-cache/stats`.
    pub fn cache_len_total(&self) -> usize {
        let mut n = self.default.cache_len();
        for p in self.per_agent.values() {
            n += p.cache_len();
        }
        n
    }
    /// Number of per-agent overrides. Reported by stats.
    pub fn override_count(&self) -> usize {
        self.per_agent.len()
    }
}
// ── Admin dispatch (pure, testable) ──────────────────────────────────────
/// Outcome of an admin request: `(status, body, content_type)`. Pure
/// function so `main.rs`'s `TcpStream` plumbing stays a thin wrapper
/// and the routing rules are covered by unit tests.
pub fn admin_dispatch(
    method: &str,
    path: &str,
    query: &str,
    registry: &ToolPolicyRegistry,
) -> (u16, String, &'static str) {
    const JSON: &str = "application/json; charset=utf-8";
    const TEXT: &str = "text/plain; charset=utf-8";
    match (method, path) {
        ("GET", "/admin/tool-cache/stats") => {
            let body = format!(
                r#"{{"entries":{},"per_agent_overrides":{}}}"#,
                registry.cache_len_total(),
                registry.override_count()
            );
            (200, body, JSON)
        }
        ("POST", "/admin/tool-cache/clear") => {
            let n = registry.cache_clear_all();
            (200, format!(r#"{{"cleared":{n}}}"#), JSON)
        }
        ("POST", "/admin/tool-cache/invalidate") => {
            let agent = query_param(query, "agent").unwrap_or_default();
            let tool = query_param(query, "tool").unwrap_or_default();
            if agent.is_empty() || tool.is_empty() {
                return (
                    400,
                    r#"{"error":"missing agent or tool query param"}"#.to_string(),
                    JSON,
                );
            }
            let policy = registry.for_agent(agent);
            let n = policy.cache_invalidate(agent, tool);
            (200, format!(r#"{{"removed":{n}}}"#), JSON)
        }
        _ => (404, "not found".to_string(), TEXT),
    }
}
fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v);
            }
        }
    }
    None
}
// ── Glob ─────────────────────────────────────────────────────────────────
/// Minimal glob: `ext_weather_*` or exact `ext_weather_forecast`. Only
/// trailing `*` is supported — anything else we treat as exact match.
/// Deliberately small: avoids pulling `globset`/`regex` into core for
/// a feature this simple.
struct GlobPattern {
    prefix: String,
    wildcard: bool,
}
impl GlobPattern {
    fn new(pattern: &str) -> Self {
        if let Some(p) = pattern.strip_suffix('*') {
            Self {
                prefix: p.to_string(),
                wildcard: true,
            }
        } else {
            Self {
                prefix: pattern.to_string(),
                wildcard: false,
            }
        }
    }
    fn matches(&self, name: &str) -> bool {
        if self.wildcard {
            name.starts_with(&self.prefix)
        } else {
            name == self.prefix
        }
    }
}
// ── Hash ─────────────────────────────────────────────────────────────────
/// Stable u64 hash of a `serde_json::Value` via `sha2`. We serialize
/// with sorted keys so `{"a":1,"b":2}` and `{"b":2,"a":1}` collide on
/// the same cache slot — otherwise cache hit rate suffers for free.
fn hash_args(args: &Value) -> u64 {
    let normalized = normalize(args);
    let bytes = serde_json::to_vec(&normalized).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(out)
}
/// Recursively replace object maps with sorted BTreeMap so the
/// serialized bytes are deterministic regardless of the caller's
/// iteration order.
fn normalize(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut sorted: HashMap<&str, Value> = HashMap::new();
            for (k, val) in m {
                sorted.insert(k.as_str(), normalize(val));
            }
            let mut keys: Vec<&&str> = sorted.keys().collect();
            keys.sort();
            let ordered: serde_json::Map<String, Value> = keys
                .into_iter()
                .map(|k| ((*k).to_string(), sorted[k].clone()))
                .collect();
            Value::Object(ordered)
        }
        Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
        other => other.clone(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn glob_prefix_match() {
        let p = GlobPattern::new("ext_weather_*");
        assert!(p.matches("ext_weather_forecast"));
        assert!(p.matches("ext_weather_current"));
        assert!(!p.matches("ext_wikipedia_search"));
    }
    #[test]
    fn glob_exact_match() {
        let p = GlobPattern::new("ext_weather_forecast");
        assert!(p.matches("ext_weather_forecast"));
        assert!(!p.matches("ext_weather_current"));
    }
    #[test]
    fn hash_is_order_independent() {
        let a = json!({"a": 1, "b": 2, "nested": {"x": 1, "y": 2}});
        let b = json!({"b": 2, "a": 1, "nested": {"y": 2, "x": 1}});
        assert_eq!(hash_args(&a), hash_args(&b));
    }
    #[test]
    fn cache_get_put_hit_and_expiry_semantics() {
        let cfg = ToolPolicyConfig {
            cache: CacheConfig {
                ttl_secs: 60,
                tools: vec!["ext_weather_*".into()],
                max_entries: 100,
                max_value_bytes: 0,
            },
            parallel_safe: vec![],
            parallel: Default::default(),
            relevance: Default::default(),
            per_agent: HashMap::new(),
        };
        let policy = ToolPolicy::from_config(&cfg);
        let args = json!({"city": "Bogotá"});
        let agent = "kate";
        assert!(policy.cache_get(agent, "ext_weather_forecast", &args).is_none());
        policy.cache_put(agent, "ext_weather_forecast", &args, json!({"temp": 22}));
        let hit = policy.cache_get(agent, "ext_weather_forecast", &args).unwrap();
        assert_eq!(hit["temp"], 22);
        // Different agent — same tool, same args — MUST miss. Cross-
        // agent cache scopes are hard-partitioned.
        assert!(policy.cache_get("other", "ext_weather_forecast", &args).is_none());
        // Not-cacheable tool bypasses the store entirely.
        policy.cache_put(agent, "ext_github_comment", &args, json!({"ok": true}));
        assert!(policy.cache_get(agent, "ext_github_comment", &args).is_none());
    }
    #[test]
    fn cache_put_skips_when_value_exceeds_bytesize_cap() {
        let cfg = ToolPolicyConfig {
            cache: CacheConfig {
                ttl_secs: 60,
                tools: vec!["ext_big_*".into()],
                max_entries: 100,
                max_value_bytes: 64,
            },
            parallel_safe: vec![],
            parallel: Default::default(),
            relevance: Default::default(),
            per_agent: HashMap::new(),
        };
        let policy = ToolPolicy::from_config(&cfg);
        let args = json!({"q": "x"});
        // Small value under 64B → cached.
        policy.cache_put("a", "ext_big_small", &args, json!({"ok": 1}));
        assert!(policy.cache_get("a", "ext_big_small", &args).is_some());
        // Large value over 64B → silently skipped.
        let big = json!({"payload": "x".repeat(256)});
        policy.cache_put("a", "ext_big_huge", &args, big);
        assert!(policy.cache_get("a", "ext_big_huge", &args).is_none());
        // Zero cap disables the check entirely.
        let cfg0 = ToolPolicyConfig {
            cache: CacheConfig {
                ttl_secs: 60,
                tools: vec!["ext_big_*".into()],
                max_entries: 100,
                max_value_bytes: 0,
            },
            parallel_safe: vec![],
            parallel: Default::default(),
            relevance: Default::default(),
            per_agent: HashMap::new(),
        };
        let p2 = ToolPolicy::from_config(&cfg0);
        p2.cache_put("a", "ext_big_huge", &args, json!({"payload": "x".repeat(1024)}));
        assert!(p2.cache_get("a", "ext_big_huge", &args).is_some());
    }
    #[test]
    fn cache_invalidate_and_clear() {
        let cfg = ToolPolicyConfig {
            cache: CacheConfig {
                ttl_secs: 60,
                tools: vec!["ext_*".into()],
                max_entries: 100,
                max_value_bytes: 0,
            },
            parallel_safe: vec![],
            parallel: Default::default(),
            relevance: Default::default(),
            per_agent: HashMap::new(),
        };
        let policy = ToolPolicy::from_config(&cfg);
        let a1 = json!({"q": "a"});
        let a2 = json!({"q": "b"});
        policy.cache_put("agent", "ext_weather_forecast", &a1, json!({"t": 1}));
        policy.cache_put("agent", "ext_weather_forecast", &a2, json!({"t": 2}));
        policy.cache_put("agent", "ext_wikipedia_summary", &a1, json!({"s": "x"}));
        assert_eq!(policy.cache_len(), 3);
        // Invalidate one (agent, tool) pair — wipes both arg variants.
        let removed = policy.cache_invalidate("agent", "ext_weather_forecast");
        assert_eq!(removed, 2);
        assert!(policy.cache_get("agent", "ext_weather_forecast", &a1).is_none());
        assert!(policy.cache_get("agent", "ext_weather_forecast", &a2).is_none());
        // Untargeted tool still cached.
        assert!(policy.cache_get("agent", "ext_wikipedia_summary", &a1).is_some());
        // Invalidating a non-existent pair is a harmless zero.
        assert_eq!(policy.cache_invalidate("agent", "does_not_exist"), 0);
        // Clear drops the rest.
        let cleared = policy.cache_clear();
        assert_eq!(cleared, 1);
        assert_eq!(policy.cache_len(), 0);
    }
    #[test]
    fn admin_dispatch_routes_and_validates() {
        let cfg = ToolPolicyConfig {
            cache: CacheConfig {
                ttl_secs: 60,
                tools: vec!["ext_*".into()],
                max_entries: 100,
                max_value_bytes: 0,
            },
            parallel_safe: vec![],
            parallel: Default::default(),
            relevance: Default::default(),
            per_agent: HashMap::new(),
        };
        let reg = ToolPolicyRegistry::from_config(&cfg);
        let policy = reg.for_agent("kate");
        let args = json!({"q": "x"});
        policy.cache_put("kate", "ext_weather_forecast", &args, json!({"t": 1}));
        // Stats reports the one entry, zero overrides (empty per_agent).
        let (status, body, ctype) = admin_dispatch("GET", "/admin/tool-cache/stats", "", &reg);
        assert_eq!(status, 200);
        assert!(ctype.starts_with("application/json"));
        assert!(body.contains(r#""entries":1"#));
        assert!(body.contains(r#""per_agent_overrides":0"#));
        // Invalidate — valid payload returns `removed:1`.
        let (status, body, _) = admin_dispatch(
            "POST",
            "/admin/tool-cache/invalidate",
            "agent=kate&tool=ext_weather_forecast",
            &reg,
        );
        assert_eq!(status, 200);
        assert!(body.contains(r#""removed":1"#));
        // Cache should now be empty.
        let (_, body, _) = admin_dispatch("GET", "/admin/tool-cache/stats", "", &reg);
        assert!(body.contains(r#""entries":0"#));
        // Invalidate with missing params → 400.
        let (status, body, _) = admin_dispatch(
            "POST",
            "/admin/tool-cache/invalidate",
            "agent=kate",
            &reg,
        );
        assert_eq!(status, 400);
        assert!(body.contains("missing"));
        // Clear path returns 200 even on empty cache.
        let (status, body, _) = admin_dispatch("POST", "/admin/tool-cache/clear", "", &reg);
        assert_eq!(status, 200);
        assert!(body.contains(r#""cleared":0"#));
        // Unknown route → 404.
        let (status, _, _) = admin_dispatch("GET", "/admin/unknown", "", &reg);
        assert_eq!(status, 404);
        // Wrong method → 404 (we don't advertise Method Not Allowed).
        let (status, _, _) = admin_dispatch("DELETE", "/admin/tool-cache/clear", "", &reg);
        assert_eq!(status, 404);
    }
    #[test]
    fn registry_routes_per_agent_overrides() {
        let mut per_agent = HashMap::new();
        per_agent.insert(
            "kate".to_string(),
            AgentPolicyOverride {
                cache: CacheConfig {
                    ttl_secs: 10,
                    tools: vec!["ext_kate_only_*".into()],
                    max_entries: 8,
                    max_value_bytes: 0,
                },
                parallel_safe: vec!["ext_kate_only_*".into()],
                parallel: ParallelConfig { max_in_flight: 2, call_timeout_secs: 5 },
                relevance: Default::default(),
            },
        );
        let cfg = ToolPolicyConfig {
            cache: CacheConfig {
                ttl_secs: 60,
                tools: vec!["ext_shared_*".into()],
                max_entries: 100,
                max_value_bytes: 0,
            },
            parallel_safe: vec!["ext_shared_*".into()],
            parallel: Default::default(),
            relevance: Default::default(),
            per_agent,
        };
        let reg = ToolPolicyRegistry::from_config(&cfg);
        let kate = reg.for_agent("kate");
        let eli = reg.for_agent("eli");
        // Kate's override fully replaces cache tool list.
        assert!(kate.is_cacheable("ext_kate_only_foo"));
        assert!(!kate.is_cacheable("ext_shared_bar"));
        // Unknown agent falls through to the default.
        assert!(eli.is_cacheable("ext_shared_bar"));
        assert!(!eli.is_cacheable("ext_kate_only_foo"));
        // Parallel bound picked up per agent.
        assert_eq!(kate.parallel_config().max_in_flight, 2);
        // Cache store is separate Arc per agent.
        let args = json!({"x": 1});
        kate.cache_put("kate", "ext_kate_only_foo", &args, json!("kate-val"));
        // Eli shares the default ToolPolicy, so inserting on `eli` via
        // default-partitioned cache does NOT leak into kate's store.
        eli.cache_put("eli", "ext_shared_bar", &args, json!("eli-val"));
        assert!(kate.cache_get("eli", "ext_shared_bar", &args).is_none());
        assert!(eli.cache_get("kate", "ext_kate_only_foo", &args).is_none());
    }
    #[test]
    fn partition_splits_by_parallel_safe() {
        let cfg = ToolPolicyConfig {
            cache: CacheConfig::default(),
            parallel_safe: vec!["ext_weather_*".into(), "ext_wikipedia_*".into()],
            parallel: Default::default(),
            relevance: Default::default(),
            per_agent: HashMap::new(),
        };
        let policy = ToolPolicy::from_config(&cfg);
        let calls = vec![
            "ext_weather_forecast",
            "ext_github_comment",
            "ext_wikipedia_summary",
        ];
        let (par, seq) = policy.partition(&calls, |s: &&str| *s);
        assert_eq!(par.len(), 2);
        assert_eq!(seq.len(), 1);
        assert_eq!(*seq[0], "ext_github_comment");
    }
}
