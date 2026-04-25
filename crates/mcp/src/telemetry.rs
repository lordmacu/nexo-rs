//! Phase 9.2 follow-up — Prometheus metrics for MCP session lifecycle.
//!
//! Lives in `nexo-mcp` (not `nexo-core::telemetry`) because
//! `nexo-mcp` is deliberately kept free of an `nexo-core` dep to
//! avoid the cycle. `main.rs` concatenates `render_mcp_prometheus()` into
//! the `/metrics` response alongside core helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use dashmap::DashMap;

const LIFETIME_BUCKET_LIMITS_SECS: [u64; 8] = [5, 30, 60, 300, 900, 1800, 3600, 7200];
const SAMPLING_BUCKET_LIMITS_MS: [u64; 8] = [10, 25, 50, 100, 250, 500, 1000, 2500];

struct Histogram {
    buckets: [AtomicU64; 8],
    count: AtomicU64,
    sum_secs: AtomicU64,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct SamplingRequestKey {
    server: String,
    outcome: String,
}

struct SamplingHistogram {
    buckets: [AtomicU64; 8],
    count: AtomicU64,
    sum_ms: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum_secs: AtomicU64::new(0),
        }
    }

    fn observe(&self, secs: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_secs.fetch_add(secs, Ordering::Relaxed);
        for (i, upper) in LIFETIME_BUCKET_LIMITS_SECS.iter().enumerate() {
            if secs <= *upper {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl SamplingHistogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum_ms: AtomicU64::new(0),
        }
    }

    fn observe_ms(&self, duration_ms: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_ms.fetch_add(duration_ms, Ordering::Relaxed);
        for (idx, upper) in SAMPLING_BUCKET_LIMITS_MS.iter().enumerate() {
            if duration_ms <= *upper {
                self.buckets[idx].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

static SESSIONS_CREATED: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static SESSIONS_ACTIVE: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static SESSIONS_DISPOSED: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);
static SESSION_LIFETIME: LazyLock<Histogram> = LazyLock::new(Histogram::new);
/// Phase 12.5 follow-up — resource cache hit/miss counters, keyed by server.
static RESOURCE_CACHE_HITS: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);
static RESOURCE_CACHE_MISSES: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);
static RESOURCE_URI_ALLOWLIST_VIOLATIONS: LazyLock<DashMap<String, AtomicU64>> =
    LazyLock::new(DashMap::new);
static MCP_SAMPLING_REQUESTS: LazyLock<DashMap<SamplingRequestKey, AtomicU64>> =
    LazyLock::new(DashMap::new);
static MCP_SAMPLING_DURATION_MS: LazyLock<DashMap<String, SamplingHistogram>> =
    LazyLock::new(DashMap::new);

pub fn inc_sessions_created() {
    SESSIONS_CREATED.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_sessions_active() {
    SESSIONS_ACTIVE.fetch_add(1, Ordering::Relaxed);
}

pub fn dec_sessions_active() {
    // saturating: never go negative.
    let mut cur = SESSIONS_ACTIVE.load(Ordering::Relaxed);
    loop {
        if cur == 0 {
            return;
        }
        match SESSIONS_ACTIVE.compare_exchange(cur, cur - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(v) => cur = v,
        }
    }
}

pub fn inc_sessions_disposed(reason: &str) {
    let clean: String = reason
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    SESSIONS_DISPOSED
        .entry(clean)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn observe_session_lifetime_secs(secs: u64) {
    SESSION_LIFETIME.observe(secs);
}

pub fn inc_resource_cache_hit(server: &str) {
    RESOURCE_CACHE_HITS
        .entry(server.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_resource_cache_miss(server: &str) {
    RESOURCE_CACHE_MISSES
        .entry(server.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_resource_uri_allowlist_violation(server: &str) {
    RESOURCE_URI_ALLOWLIST_VIOLATIONS
        .entry(server.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_sampling_requests_total(server: &str, outcome: &str) {
    MCP_SAMPLING_REQUESTS
        .entry(SamplingRequestKey {
            server: server.to_string(),
            outcome: outcome.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn observe_sampling_duration_ms(server: &str, duration_ms: u64) {
    MCP_SAMPLING_DURATION_MS
        .entry(server.to_string())
        .or_insert_with(SamplingHistogram::new)
        .observe_ms(duration_ms);
}

#[cfg(test)]
pub fn reset_for_test() {
    SESSIONS_CREATED.store(0, Ordering::Relaxed);
    SESSIONS_ACTIVE.store(0, Ordering::Relaxed);
    SESSIONS_DISPOSED.clear();
    for b in SESSION_LIFETIME.buckets.iter() {
        b.store(0, Ordering::Relaxed);
    }
    SESSION_LIFETIME.count.store(0, Ordering::Relaxed);
    SESSION_LIFETIME.sum_secs.store(0, Ordering::Relaxed);
    RESOURCE_CACHE_HITS.clear();
    RESOURCE_CACHE_MISSES.clear();
    RESOURCE_URI_ALLOWLIST_VIOLATIONS.clear();
    MCP_SAMPLING_REQUESTS.clear();
    MCP_SAMPLING_DURATION_MS.clear();
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Render all MCP session-lifecycle metrics in Prometheus text format.
/// Concatenated by `main.rs` onto the core telemetry response.
pub fn render_prometheus() -> String {
    let mut out = String::new();

    out.push_str("# HELP mcp_sessions_active Number of live MCP session runtimes.\n");
    out.push_str("# TYPE mcp_sessions_active gauge\n");
    out.push_str(&format!(
        "mcp_sessions_active {}\n",
        SESSIONS_ACTIVE.load(Ordering::Relaxed)
    ));

    out.push_str("# HELP mcp_sessions_created_total Total MCP session runtimes created.\n");
    out.push_str("# TYPE mcp_sessions_created_total counter\n");
    out.push_str(&format!(
        "mcp_sessions_created_total {}\n",
        SESSIONS_CREATED.load(Ordering::Relaxed)
    ));

    out.push_str("# HELP mcp_sessions_disposed_total MCP session runtimes disposed by reason.\n");
    out.push_str("# TYPE mcp_sessions_disposed_total counter\n");
    if SESSIONS_DISPOSED.is_empty() {
        out.push_str("mcp_sessions_disposed_total{reason=\"\"} 0\n");
    } else {
        let mut rows: Vec<_> = SESSIONS_DISPOSED
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (reason, v) in rows {
            out.push_str(&format!(
                "mcp_sessions_disposed_total{{reason=\"{}\"}} {}\n",
                escape(&reason),
                v
            ));
        }
    }

    out.push_str("# HELP mcp_session_lifetime_secs MCP session lifetime in seconds.\n");
    out.push_str("# TYPE mcp_session_lifetime_secs histogram\n");
    let count = SESSION_LIFETIME.count.load(Ordering::Relaxed);
    for (i, upper) in LIFETIME_BUCKET_LIMITS_SECS.iter().enumerate() {
        out.push_str(&format!(
            "mcp_session_lifetime_secs_bucket{{le=\"{upper}\"}} {}\n",
            SESSION_LIFETIME.buckets[i].load(Ordering::Relaxed)
        ));
    }
    out.push_str(&format!(
        "mcp_session_lifetime_secs_bucket{{le=\"+Inf\"}} {count}\n"
    ));
    out.push_str(&format!(
        "mcp_session_lifetime_secs_sum {}\n",
        SESSION_LIFETIME.sum_secs.load(Ordering::Relaxed)
    ));
    out.push_str(&format!("mcp_session_lifetime_secs_count {count}\n"));

    out.push_str("# HELP mcp_resource_cache_hits_total Resource cache hits by server.\n");
    out.push_str("# TYPE mcp_resource_cache_hits_total counter\n");
    let mut hits: Vec<_> = RESOURCE_CACHE_HITS
        .iter()
        .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0));
    if hits.is_empty() {
        out.push_str("mcp_resource_cache_hits_total{server=\"\"} 0\n");
    } else {
        for (server, v) in hits {
            out.push_str(&format!(
                "mcp_resource_cache_hits_total{{server=\"{}\"}} {}\n",
                escape(&server),
                v
            ));
        }
    }
    out.push_str("# HELP mcp_resource_cache_misses_total Resource cache misses by server.\n");
    out.push_str("# TYPE mcp_resource_cache_misses_total counter\n");
    let mut misses: Vec<_> = RESOURCE_CACHE_MISSES
        .iter()
        .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
        .collect();
    misses.sort_by(|a, b| a.0.cmp(&b.0));
    if misses.is_empty() {
        out.push_str("mcp_resource_cache_misses_total{server=\"\"} 0\n");
    } else {
        for (server, v) in misses {
            out.push_str(&format!(
                "mcp_resource_cache_misses_total{{server=\"{}\"}} {}\n",
                escape(&server),
                v
            ));
        }
    }

    out.push_str(
        "# HELP mcp_resource_uri_allowlist_violations_total read_resource calls with URI scheme outside the configured allowlist.\n",
    );
    out.push_str("# TYPE mcp_resource_uri_allowlist_violations_total counter\n");
    let mut viol: Vec<_> = RESOURCE_URI_ALLOWLIST_VIOLATIONS
        .iter()
        .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
        .collect();
    viol.sort_by(|a, b| a.0.cmp(&b.0));
    if viol.is_empty() {
        out.push_str("mcp_resource_uri_allowlist_violations_total{server=\"\"} 0\n");
    } else {
        for (server, v) in viol {
            out.push_str(&format!(
                "mcp_resource_uri_allowlist_violations_total{{server=\"{}\"}} {}\n",
                escape(&server),
                v
            ));
        }
    }

    out.push_str(
        "# HELP nexo_mcp_sampling_requests_total sampling/createMessage requests by server and outcome.\n",
    );
    out.push_str("# TYPE nexo_mcp_sampling_requests_total counter\n");
    if MCP_SAMPLING_REQUESTS.is_empty() {
        out.push_str("nexo_mcp_sampling_requests_total{server=\"\",outcome=\"\"} 0\n");
    } else {
        let mut rows: Vec<_> = MCP_SAMPLING_REQUESTS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.server.clone(), a.0.outcome.clone())
                .cmp(&(b.0.server.clone(), b.0.outcome.clone()))
        });
        for (key, v) in rows {
            out.push_str(&format!(
                "nexo_mcp_sampling_requests_total{{server=\"{}\",outcome=\"{}\"}} {}\n",
                escape(&key.server),
                escape(&key.outcome),
                v
            ));
        }
    }

    out.push_str(
        "# HELP nexo_mcp_sampling_duration_ms sampling/createMessage duration in milliseconds.\n",
    );
    out.push_str("# TYPE nexo_mcp_sampling_duration_ms histogram\n");
    if MCP_SAMPLING_DURATION_MS.is_empty() {
        for upper in SAMPLING_BUCKET_LIMITS_MS.iter() {
            out.push_str(&format!(
                "nexo_mcp_sampling_duration_ms_bucket{{server=\"\",le=\"{upper}\"}} 0\n"
            ));
        }
        out.push_str("nexo_mcp_sampling_duration_ms_bucket{server=\"\",le=\"+Inf\"} 0\n");
        out.push_str("nexo_mcp_sampling_duration_ms_sum{server=\"\"} 0\n");
        out.push_str("nexo_mcp_sampling_duration_ms_count{server=\"\"} 0\n");
    } else {
        let mut servers: Vec<_> = MCP_SAMPLING_DURATION_MS
            .iter()
            .map(|e| e.key().clone())
            .collect();
        servers.sort();
        servers.dedup();
        for server in servers {
            let Some(series) = MCP_SAMPLING_DURATION_MS.get(&server) else {
                continue;
            };
            let server = escape(&server);
            for (idx, upper) in SAMPLING_BUCKET_LIMITS_MS.iter().enumerate() {
                out.push_str(&format!(
                    "nexo_mcp_sampling_duration_ms_bucket{{server=\"{server}\",le=\"{upper}\"}} {}\n",
                    series.buckets[idx].load(Ordering::Relaxed)
                ));
            }
            let count = series.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "nexo_mcp_sampling_duration_ms_bucket{{server=\"{server}\",le=\"+Inf\"}} {count}\n"
            ));
            out.push_str(&format!(
                "nexo_mcp_sampling_duration_ms_sum{{server=\"{server}\"}} {}\n",
                series.sum_ms.load(Ordering::Relaxed)
            ));
            out.push_str(&format!(
                "nexo_mcp_sampling_duration_ms_count{{server=\"{server}\"}} {count}\n"
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn render_includes_all_series() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        inc_sessions_created();
        inc_sessions_active();
        inc_sessions_disposed("reap");
        inc_sessions_disposed("client shutdown");
        observe_session_lifetime_secs(42);
        dec_sessions_active();

        let body = render_prometheus();
        assert!(body.contains("mcp_sessions_active 0"));
        assert!(body.contains("mcp_sessions_created_total 1"));
        assert!(body.contains("mcp_sessions_disposed_total{reason=\"reap\"} 1"));
        assert!(body.contains("mcp_sessions_disposed_total{reason=\"client shutdown\"} 1"));
        assert!(body.contains("mcp_session_lifetime_secs_count 1"));
        assert!(body.contains("mcp_session_lifetime_secs_sum 42"));
        assert!(body.contains("mcp_session_lifetime_secs_bucket{le=\"60\"} 1"));
    }

    #[test]
    fn disposed_reason_is_sanitized() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        inc_sessions_disposed("newline\nstripped");
        let body = render_prometheus();
        assert!(body.contains("mcp_sessions_disposed_total{reason=\"newlinestripped\"}"));
    }

    #[test]
    fn resource_cache_counters_render() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        inc_resource_cache_hit("fs");
        inc_resource_cache_hit("fs");
        inc_resource_cache_miss("fs");
        inc_resource_cache_miss("db");
        let body = render_prometheus();
        assert!(body.contains("mcp_resource_cache_hits_total{server=\"fs\"} 2"));
        assert!(body.contains("mcp_resource_cache_misses_total{server=\"fs\"} 1"));
        assert!(body.contains("mcp_resource_cache_misses_total{server=\"db\"} 1"));
    }

    #[test]
    fn resource_cache_counters_empty_default() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        let body = render_prometheus();
        assert!(body.contains("mcp_resource_cache_hits_total{server=\"\"} 0"));
        assert!(body.contains("mcp_resource_cache_misses_total{server=\"\"} 0"));
    }

    #[test]
    fn dec_never_goes_negative() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        dec_sessions_active();
        dec_sessions_active();
        let body = render_prometheus();
        assert!(body.contains("mcp_sessions_active 0"));
    }

    #[test]
    fn sampling_metrics_render() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        inc_sampling_requests_total("srv-a", "ok");
        inc_sampling_requests_total("srv-a", "ok");
        inc_sampling_requests_total("srv-a", "llm_error");
        observe_sampling_duration_ms("srv-a", 42);
        observe_sampling_duration_ms("srv-a", 120);

        let body = render_prometheus();
        assert!(
            body.contains("nexo_mcp_sampling_requests_total{server=\"srv-a\",outcome=\"ok\"} 2")
        );
        assert!(body.contains(
            "nexo_mcp_sampling_requests_total{server=\"srv-a\",outcome=\"llm_error\"} 1"
        ));
        assert!(body.contains("nexo_mcp_sampling_duration_ms_count{server=\"srv-a\"} 2"));
        assert!(body.contains("nexo_mcp_sampling_duration_ms_sum{server=\"srv-a\"} 162"));
    }

    #[test]
    fn sampling_metrics_empty_defaults() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        let body = render_prometheus();
        assert!(body.contains("nexo_mcp_sampling_requests_total{server=\"\",outcome=\"\"} 0"));
        assert!(body.contains("nexo_mcp_sampling_duration_ms_count{server=\"\"} 0"));
    }
}
