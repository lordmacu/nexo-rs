//! Phase 25 follow-up (W-1) — Prometheus telemetry for the web-search
//! router. Exposes counters, histogram, and a render block that
//! `nexo_core::telemetry::render_prometheus` stitches into the
//! exposition response.
//!
//! Layering note: this crate is a leaf (no nexo-core dep) so we ship
//! the metric primitives here and the host process just calls
//! [`render`] when assembling `/metrics`.

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

const LATENCY_BUCKET_LIMITS_MS: [u64; 8] = [50, 100, 250, 500, 1000, 2500, 5000, 10000];

struct Histogram {
    buckets: [AtomicU64; 8],
    count: AtomicU64,
    sum_ms: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum_ms: AtomicU64::new(0),
        }
    }
    fn observe(&self, duration_ms: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_ms.fetch_add(duration_ms, Ordering::Relaxed);
        for (idx, upper) in LATENCY_BUCKET_LIMITS_MS.iter().enumerate() {
            if duration_ms <= *upper {
                self.buckets[idx].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// `(provider, result)` keyed call counter. `result` is one of
/// `"ok" | "error" | "unavailable"`. `unavailable` distinguishes a
/// breaker-open short-circuit from a real provider error so dashboards
/// can alert on outages without false positives during a self-healing
/// cooldown window.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct CallKey {
    provider: String,
    result: String,
}

static CALLS: LazyLock<DashMap<CallKey, AtomicU64>> = LazyLock::new(DashMap::new);
static LATENCY: LazyLock<DashMap<String, Histogram>> = LazyLock::new(DashMap::new);
static CACHE_EVENTS: LazyLock<DashMap<CallKey, AtomicU64>> = LazyLock::new(DashMap::new);
static BREAKER_OPEN: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);

/// Bump the per-(provider, result) call counter.
pub fn inc_call(provider: &str, result: &str) {
    CALLS
        .entry(CallKey {
            provider: provider.to_string(),
            result: result.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Observe per-provider call latency. Only fired for attempts that
/// actually issued an HTTP request — cache hits and breaker-open
/// short-circuits skip this so percentiles reflect real provider
/// work.
pub fn observe_latency_ms(provider: &str, duration_ms: u64) {
    LATENCY
        .entry(provider.to_string())
        .or_insert_with(Histogram::new)
        .observe(duration_ms);
}

/// Bump the cache lookup counter. `hit=true` for a TTL-fresh return,
/// `hit=false` for a miss (about to dispatch to a provider).
pub fn inc_cache(provider: &str, hit: bool) {
    let label = if hit { "true" } else { "false" };
    CACHE_EVENTS
        .entry(CallKey {
            provider: provider.to_string(),
            result: label.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Bump the breaker-open counter (one increment per request the
/// breaker short-circuited).
pub fn inc_breaker_open(provider: &str) {
    BREAKER_OPEN
        .entry(provider.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Test helpers.
pub fn calls_total(provider: &str, result: &str) -> u64 {
    CALLS
        .get(&CallKey {
            provider: provider.to_string(),
            result: result.to_string(),
        })
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}
pub fn cache_total(provider: &str, hit: bool) -> u64 {
    let label = if hit { "true" } else { "false" };
    CACHE_EVENTS
        .get(&CallKey {
            provider: provider.to_string(),
            result: label.to_string(),
        })
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}
pub fn breaker_open_total(provider: &str) -> u64 {
    BREAKER_OPEN
        .get(provider)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// Reset all counters (test-only; no-op clear of histograms — they
/// accumulate process-wide and tests should compare deltas).
pub fn reset_for_test() {
    CALLS.clear();
    CACHE_EVENTS.clear();
    BREAKER_OPEN.clear();
    LATENCY.clear();
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

/// Append the web-search metrics block to a Prometheus exposition
/// buffer. Called by `nexo_core::telemetry::render_prometheus`.
pub fn render(out: &mut String) {
    out.push_str(
        "# HELP nexo_web_search_calls_total Web-search provider calls by provider and result.\n",
    );
    out.push_str("# TYPE nexo_web_search_calls_total counter\n");
    if CALLS.is_empty() {
        out.push_str("nexo_web_search_calls_total{provider=\"\",result=\"\"} 0\n");
    } else {
        let mut rows: Vec<(CallKey, u64)> = CALLS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.provider.clone(), a.0.result.clone())
                .cmp(&(b.0.provider.clone(), b.0.result.clone()))
        });
        for (k, v) in rows {
            out.push_str(&format!(
                "nexo_web_search_calls_total{{provider=\"{}\",result=\"{}\"}} {}\n",
                escape(&k.provider),
                escape(&k.result),
                v
            ));
        }
    }

    out.push_str(
        "# HELP nexo_web_search_cache_total Web-search cache lookups by provider and hit.\n",
    );
    out.push_str("# TYPE nexo_web_search_cache_total counter\n");
    if CACHE_EVENTS.is_empty() {
        out.push_str("nexo_web_search_cache_total{provider=\"\",hit=\"\"} 0\n");
    } else {
        let mut rows: Vec<(CallKey, u64)> = CACHE_EVENTS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.provider.clone(), a.0.result.clone())
                .cmp(&(b.0.provider.clone(), b.0.result.clone()))
        });
        for (k, v) in rows {
            out.push_str(&format!(
                "nexo_web_search_cache_total{{provider=\"{}\",hit=\"{}\"}} {}\n",
                escape(&k.provider),
                escape(&k.result),
                v
            ));
        }
    }

    out.push_str(
        "# HELP nexo_web_search_breaker_open_total Provider requests short-circuited by an open circuit breaker.\n",
    );
    out.push_str("# TYPE nexo_web_search_breaker_open_total counter\n");
    if BREAKER_OPEN.is_empty() {
        out.push_str("nexo_web_search_breaker_open_total{provider=\"\"} 0\n");
    } else {
        let mut rows: Vec<(String, u64)> = BREAKER_OPEN
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (provider, v) in rows {
            out.push_str(&format!(
                "nexo_web_search_breaker_open_total{{provider=\"{}\"}} {}\n",
                escape(&provider),
                v
            ));
        }
    }

    out.push_str("# HELP nexo_web_search_latency_ms Per-provider call latency in milliseconds.\n");
    out.push_str("# TYPE nexo_web_search_latency_ms histogram\n");
    if LATENCY.is_empty() {
        for upper in LATENCY_BUCKET_LIMITS_MS.iter() {
            out.push_str(&format!(
                "nexo_web_search_latency_ms_bucket{{provider=\"\",le=\"{upper}\"}} 0\n"
            ));
        }
        out.push_str("nexo_web_search_latency_ms_bucket{provider=\"\",le=\"+Inf\"} 0\n");
        out.push_str("nexo_web_search_latency_ms_sum{provider=\"\"} 0\n");
        out.push_str("nexo_web_search_latency_ms_count{provider=\"\"} 0\n");
    } else {
        let mut keys: Vec<String> = LATENCY.iter().map(|e| e.key().clone()).collect();
        keys.sort();
        for k in keys {
            let Some(h) = LATENCY.get(&k) else { continue };
            let provider = escape(&k);
            for (idx, upper) in LATENCY_BUCKET_LIMITS_MS.iter().enumerate() {
                out.push_str(&format!(
                    "nexo_web_search_latency_ms_bucket{{provider=\"{provider}\",le=\"{upper}\"}} {}\n",
                    h.buckets[idx].load(Ordering::Relaxed)
                ));
            }
            let count = h.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "nexo_web_search_latency_ms_bucket{{provider=\"{provider}\",le=\"+Inf\"}} {count}\n"
            ));
            out.push_str(&format!(
                "nexo_web_search_latency_ms_sum{{provider=\"{provider}\"}} {}\n",
                h.sum_ms.load(Ordering::Relaxed)
            ));
            out.push_str(&format!(
                "nexo_web_search_latency_ms_count{{provider=\"{provider}\"}} {count}\n"
            ));
        }
    }
}
