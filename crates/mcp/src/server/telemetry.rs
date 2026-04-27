//! Phase 76.10 — server-side observability for the MCP HTTP/stdio
//! dispatcher. Hand-rolled Prometheus text emitter following the
//! in-tree pattern (`crates/web-search/src/telemetry.rs`,
//! `crates/llm/src/telemetry.rs`, `crates/poller/src/telemetry.rs`)
//! — `LazyLock<DashMap<Key, AtomicU64>>` modules globals,
//! render-on-scrape, no `prometheus` crate dependency.
//!
//! ## Metric inventory
//!
//! * `mcp_requests_total{tenant,tool,outcome}` — counter
//! * `mcp_request_duration_seconds{tenant,tool}` — histogram (8 buckets)
//! * `mcp_in_flight{tenant,tool}` — gauge (signed; managed by RAII guard)
//! * `mcp_rate_limit_hits_total{tenant,tool}` — counter
//! * `mcp_timeouts_total{tenant,tool}` — counter
//! * `mcp_concurrency_rejections_total{tenant,tool}` — counter
//! * `mcp_progress_notifications_total{outcome}` — counter (ok|drop)
//! * `mcp_sessions_active` — gauge (sampled at render time)
//! * `mcp_sse_subscribers` — gauge (sampled at render time)
//!
//! ## Cardinality discipline
//!
//! Tool labels are bounded by `MAX_DISTINCT_TOOLS = 256`; once the
//! threshold is reached, every new tool name is collapsed to
//! `"other"`. Pattern ported from
//! `claude-code-leak/src/services/analytics/datadog.ts:195-217`
//! (`mcp__*` tools collapsed to `'mcp'`). Tenant labels are bounded
//! by config (the `TenantId` parser already restricts the alphabet
//! to `[a-z0-9_-]{1,64}`); operators with thousands of tenants
//! should aggregate at the dashboard layer rather than at scrape
//! time, but the metric won't blow up Prometheus.
//!
//! ## Reset for tests
//!
//! `reset_for_tests()` empties every map. Tests that exercise the
//! counters from multiple methods within a single binary should call
//! it at the top of each `#[tokio::test]` to avoid cross-test
//! pollution.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use dashmap::DashMap;

// --- Outcome -----------------------------------------------------

/// Bounded set of outcome labels. Reused by Phase 76.11 audit
/// — `serde` derives keep the wire shape identical to
/// `as_label()` (snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Cancelled,
    Timeout,
    RateLimited,
    Denied,
    Panicked,
}

impl Outcome {
    pub fn as_label(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Error => "error",
            Outcome::Cancelled => "cancelled",
            Outcome::Timeout => "timeout",
            Outcome::RateLimited => "rate_limited",
            Outcome::Denied => "denied",
            Outcome::Panicked => "panicked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressEmitOutcome {
    Ok,
    Drop,
}

impl ProgressEmitOutcome {
    pub fn as_label(self) -> &'static str {
        match self {
            ProgressEmitOutcome::Ok => "ok",
            ProgressEmitOutcome::Drop => "drop",
        }
    }
}

// --- Cardinality bounding ---------------------------------------

const MAX_DISTINCT_TOOLS: usize = 256;
const OTHER_LABEL: &str = "other";

static KNOWN_TOOLS: LazyLock<DashMap<String, ()>> = LazyLock::new(DashMap::new);

/// Returns the tool name to use as a Prometheus label. First
/// `MAX_DISTINCT_TOOLS` distinct names are kept; further names
/// collapse to `"other"`. The DashMap is module-global so the
/// allowlist is shared across the process — a deliberately-leaky
/// "first 256 distinct tool names ever seen win" policy.
fn label_tool(name: &str) -> String {
    if KNOWN_TOOLS.contains_key(name) {
        return name.to_string();
    }
    if KNOWN_TOOLS.len() >= MAX_DISTINCT_TOOLS {
        return OTHER_LABEL.to_string();
    }
    KNOWN_TOOLS.insert(name.to_string(), ());
    name.to_string()
}

// --- Storage primitives -----------------------------------------

const HIST_BUCKETS_MS: [u64; 8] = [50, 100, 250, 500, 1000, 2500, 5000, 10_000];

/// One histogram. 8 buckets per the in-tree convention plus a
/// `+Inf` slot rendered separately. `sum_ms` and `count` follow
/// Prometheus `histogram` semantics.
struct HistogramSlot {
    buckets: [AtomicU64; 8],
    sum_ms: AtomicU64,
    count: AtomicU64,
}

impl HistogramSlot {
    fn new() -> Self {
        Self {
            buckets: Default::default(),
            sum_ms: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn observe(&self, duration: Duration) {
        let ms = duration.as_millis() as u64;
        for (i, threshold) in HIST_BUCKETS_MS.iter().enumerate() {
            if ms <= *threshold {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sum_ms.fetch_add(ms, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

// --- Module-global maps -----------------------------------------

type LabeledCounter = DashMap<(String, String, &'static str), AtomicU64>;
type PairCounter = DashMap<(String, String), AtomicU64>;
type Gauge = DashMap<(String, String), AtomicI64>;
type Histogram = DashMap<(String, String), HistogramSlot>;

static REQUESTS_TOTAL: LazyLock<LabeledCounter> = LazyLock::new(DashMap::new);
static REQUEST_DURATION: LazyLock<Histogram> = LazyLock::new(DashMap::new);
static IN_FLIGHT: LazyLock<Gauge> = LazyLock::new(DashMap::new);
static RATE_LIMIT_HITS: LazyLock<PairCounter> = LazyLock::new(DashMap::new);
static TIMEOUTS: LazyLock<PairCounter> = LazyLock::new(DashMap::new);
static CONCURRENCY_REJECTIONS: LazyLock<PairCounter> = LazyLock::new(DashMap::new);
static PROGRESS_NOTIFICATIONS: LazyLock<DashMap<&'static str, AtomicU64>> =
    LazyLock::new(DashMap::new);

/// Sampled gauges (set by the host at scrape time or via
/// dedicated setters). Stored as plain `AtomicI64` so render is
/// lock-free.
static SESSIONS_ACTIVE: AtomicI64 = AtomicI64::new(0);
static SSE_SUBSCRIBERS: AtomicI64 = AtomicI64::new(0);

// --- Public API --------------------------------------------------

pub fn bump_request(tenant: &str, tool: &str, outcome: Outcome) {
    let key = (tenant.to_string(), label_tool(tool), outcome.as_label());
    REQUESTS_TOTAL
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn observe_request_duration(tenant: &str, tool: &str, duration: Duration) {
    let key = (tenant.to_string(), label_tool(tool));
    REQUEST_DURATION
        .entry(key)
        .or_insert_with(HistogramSlot::new)
        .observe(duration);
}

pub fn inc_in_flight(tenant: &str, tool: &str) {
    let key = (tenant.to_string(), label_tool(tool));
    IN_FLIGHT
        .entry(key)
        .or_insert_with(|| AtomicI64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn dec_in_flight(tenant: &str, tool: &str) {
    let key = (tenant.to_string(), label_tool(tool));
    if let Some(g) = IN_FLIGHT.get(&key) {
        g.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn bump_rate_limit_hit(tenant: &str, tool: &str) {
    let key = (tenant.to_string(), label_tool(tool));
    RATE_LIMIT_HITS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn bump_timeout(tenant: &str, tool: &str) {
    let key = (tenant.to_string(), label_tool(tool));
    TIMEOUTS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn bump_concurrency_rejection(tenant: &str, tool: &str) {
    let key = (tenant.to_string(), label_tool(tool));
    CONCURRENCY_REJECTIONS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn bump_progress_notification(outcome: ProgressEmitOutcome) {
    PROGRESS_NOTIFICATIONS
        .entry(outcome.as_label())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn set_sessions_active(n: usize) {
    SESSIONS_ACTIVE.store(n as i64, Ordering::Relaxed);
}

pub fn set_sse_subscribers(n: usize) {
    SSE_SUBSCRIBERS.store(n as i64, Ordering::Relaxed);
}

/// RAII guard for the `mcp_in_flight` gauge. Constructed at the
/// top of `tools/call`; the `Drop` impl decrements regardless of
/// the exit path (success / error / panic / cancellation), so the
/// gauge can never leak. Rust's drop runs on unwind, so panics in
/// the wrapped handler still release the gauge slot.
pub struct InFlightGuard {
    tenant: String,
    tool: String,
}

impl InFlightGuard {
    pub fn new(tenant: &str, tool: &str) -> Self {
        inc_in_flight(tenant, tool);
        Self {
            tenant: tenant.to_string(),
            tool: tool.to_string(),
        }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        dec_in_flight(&self.tenant, &self.tool);
    }
}

// --- Prometheus rendering ---------------------------------------

/// Render every `mcp_*` metric as Prometheus 0.0.4 text. Called
/// from `nexo_core::telemetry::render_prometheus` at scrape time.
pub fn render_prometheus() -> String {
    let mut out = String::new();
    render_requests_total(&mut out);
    render_request_duration(&mut out);
    render_in_flight(&mut out);
    render_pair_counter(
        &mut out,
        &RATE_LIMIT_HITS,
        "mcp_rate_limit_hits_total",
        "Calls rejected by per-(tenant,tool) rate-limiter",
    );
    render_pair_counter(
        &mut out,
        &TIMEOUTS,
        "mcp_timeouts_total",
        "Calls rejected with -32001 timeout",
    );
    render_pair_counter(
        &mut out,
        &CONCURRENCY_REJECTIONS,
        "mcp_concurrency_rejections_total",
        "Calls rejected with -32002 concurrency cap",
    );
    render_progress_notifications(&mut out);
    // `mcp_sessions_active` and `mcp_sse_subscribers` are emitted
    // by `crates/mcp/src/telemetry.rs::render_prometheus()`
    // (Phase 12.4 session-lifecycle metrics) — no duplication
    // here.
    out
}

fn render_requests_total(out: &mut String) {
    out.push_str("# HELP mcp_requests_total Number of MCP tools/call dispatches by outcome\n");
    out.push_str("# TYPE mcp_requests_total counter\n");
    for entry in REQUESTS_TOTAL.iter() {
        let ((tenant, tool, outcome), v) = (entry.key(), entry.value());
        out.push_str(&format!(
            "mcp_requests_total{{tenant=\"{}\",tool=\"{}\",outcome=\"{}\"}} {}\n",
            tenant,
            tool,
            outcome,
            v.load(Ordering::Relaxed),
        ));
    }
}

fn render_request_duration(out: &mut String) {
    out.push_str("# HELP mcp_request_duration_seconds Latency of MCP tools/call dispatches\n");
    out.push_str("# TYPE mcp_request_duration_seconds histogram\n");
    for entry in REQUEST_DURATION.iter() {
        let ((tenant, tool), slot) = (entry.key(), entry.value());
        for (i, threshold_ms) in HIST_BUCKETS_MS.iter().enumerate() {
            let le = (*threshold_ms as f64) / 1000.0;
            out.push_str(&format!(
                "mcp_request_duration_seconds_bucket{{tenant=\"{}\",tool=\"{}\",le=\"{}\"}} {}\n",
                tenant,
                tool,
                le,
                slot.buckets[i].load(Ordering::Relaxed),
            ));
        }
        out.push_str(&format!(
            "mcp_request_duration_seconds_bucket{{tenant=\"{}\",tool=\"{}\",le=\"+Inf\"}} {}\n",
            tenant,
            tool,
            slot.count.load(Ordering::Relaxed),
        ));
        out.push_str(&format!(
            "mcp_request_duration_seconds_sum{{tenant=\"{}\",tool=\"{}\"}} {}\n",
            tenant,
            tool,
            (slot.sum_ms.load(Ordering::Relaxed) as f64) / 1000.0,
        ));
        out.push_str(&format!(
            "mcp_request_duration_seconds_count{{tenant=\"{}\",tool=\"{}\"}} {}\n",
            tenant,
            tool,
            slot.count.load(Ordering::Relaxed),
        ));
    }
}

fn render_in_flight(out: &mut String) {
    out.push_str("# HELP mcp_in_flight Currently-executing tools/call dispatches\n");
    out.push_str("# TYPE mcp_in_flight gauge\n");
    for entry in IN_FLIGHT.iter() {
        let ((tenant, tool), v) = (entry.key(), entry.value());
        out.push_str(&format!(
            "mcp_in_flight{{tenant=\"{}\",tool=\"{}\"}} {}\n",
            tenant,
            tool,
            v.load(Ordering::Relaxed),
        ));
    }
}

fn render_pair_counter(out: &mut String, map: &PairCounter, name: &str, help: &str) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} counter\n"));
    for entry in map.iter() {
        let ((tenant, tool), v) = (entry.key(), entry.value());
        out.push_str(&format!(
            "{name}{{tenant=\"{}\",tool=\"{}\"}} {}\n",
            tenant,
            tool,
            v.load(Ordering::Relaxed),
        ));
    }
}

fn render_progress_notifications(out: &mut String) {
    out.push_str("# HELP mcp_progress_notifications_total Progress events emitted vs dropped\n");
    out.push_str("# TYPE mcp_progress_notifications_total counter\n");
    for entry in PROGRESS_NOTIFICATIONS.iter() {
        let (k, v) = (entry.key(), entry.value());
        out.push_str(&format!(
            "mcp_progress_notifications_total{{outcome=\"{}\"}} {}\n",
            k,
            v.load(Ordering::Relaxed),
        ));
    }
}

// `mcp_sessions_active` and `mcp_sse_subscribers` are intentionally
// not rendered here — see comment in `render_prometheus`. The
// SESSIONS_ACTIVE / SSE_SUBSCRIBERS atomics stay for future use
// (a follow-up may consolidate session-lifecycle + dispatch
// telemetry into one module).
#[allow(dead_code)]
fn _render_global_gauges_reserved() {}

/// Reset every map. Used by integration tests so cross-test
/// pollution doesn't leak. NEVER call from production code —
/// it'll erase live counters.
pub fn reset_for_tests() {
    REQUESTS_TOTAL.clear();
    REQUEST_DURATION.clear();
    IN_FLIGHT.clear();
    RATE_LIMIT_HITS.clear();
    TIMEOUTS.clear();
    CONCURRENCY_REJECTIONS.clear();
    PROGRESS_NOTIFICATIONS.clear();
    KNOWN_TOOLS.clear();
    SESSIONS_ACTIVE.store(0, Ordering::Relaxed);
    SSE_SUBSCRIBERS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate() {
        reset_for_tests();
    }

    #[test]
    #[serial_test::serial]
    fn outcome_round_trip() {
        for o in [
            Outcome::Ok,
            Outcome::Error,
            Outcome::Cancelled,
            Outcome::Timeout,
            Outcome::RateLimited,
            Outcome::Denied,
            Outcome::Panicked,
        ] {
            // Just exercise the label mapping — the test asserts
            // there is no panic and every variant has a label.
            let l = o.as_label();
            assert!(!l.is_empty());
        }
    }

    #[test]
    #[serial_test::serial]
    fn bump_request_increments_and_renders() {
        isolate();
        bump_request("tenant-a", "echo", Outcome::Ok);
        bump_request("tenant-a", "echo", Outcome::Ok);
        bump_request("tenant-a", "echo", Outcome::Error);
        let body = render_prometheus();
        assert!(body.contains("# TYPE mcp_requests_total counter"));
        assert!(
            body.contains("mcp_requests_total{tenant=\"tenant-a\",tool=\"echo\",outcome=\"ok\"} 2")
        );
        assert!(body
            .contains("mcp_requests_total{tenant=\"tenant-a\",tool=\"echo\",outcome=\"error\"} 1"));
    }

    #[test]
    #[serial_test::serial]
    fn observe_duration_buckets_correctly() {
        isolate();
        observe_request_duration("t", "echo", Duration::from_millis(30));
        observe_request_duration("t", "echo", Duration::from_millis(200));
        let body = render_prometheus();
        // 30ms hits every bucket le >= 50ms (cumulative); 200ms
        // hits every bucket le >= 250ms.
        assert!(
            body.contains("le=\"0.05\"} 1"),
            "le=0.05 should have 1 (just the 30ms)"
        );
        assert!(body.contains("le=\"0.1\"} 1"), "le=0.1 should have 1");
        assert!(body.contains("le=\"0.25\"} 2"), "le=0.25 should have both");
        assert!(body.contains("le=\"+Inf\"} 2"));
        assert!(body.contains("count{tenant=\"t\",tool=\"echo\"} 2"));
    }

    #[test]
    #[serial_test::serial]
    fn inc_dec_in_flight_zero_at_rest() {
        isolate();
        inc_in_flight("t", "echo");
        inc_in_flight("t", "echo");
        dec_in_flight("t", "echo");
        dec_in_flight("t", "echo");
        let body = render_prometheus();
        assert!(body.contains("mcp_in_flight{tenant=\"t\",tool=\"echo\"} 0"));
    }

    #[test]
    #[serial_test::serial]
    fn inflight_guard_decrements_on_drop() {
        isolate();
        {
            let _g = InFlightGuard::new("t", "echo");
            let body = render_prometheus();
            assert!(body.contains("mcp_in_flight{tenant=\"t\",tool=\"echo\"} 1"));
        }
        let body = render_prometheus();
        assert!(body.contains("mcp_in_flight{tenant=\"t\",tool=\"echo\"} 0"));
    }

    #[test]
    #[serial_test::serial]
    fn inflight_guard_decrements_on_panic_unwind() {
        isolate();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = InFlightGuard::new("t", "boom");
            panic!("intentional");
        }));
        assert!(r.is_err());
        // Drop runs on unwind — gauge must be back to 0.
        let body = render_prometheus();
        assert!(
            body.contains("mcp_in_flight{tenant=\"t\",tool=\"boom\"} 0"),
            "expected 0, got body: {body}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn unknown_tool_collapses_to_other_above_cap() {
        isolate();
        // Fill the allowlist with 256 distinct tool names.
        for i in 0..MAX_DISTINCT_TOOLS {
            bump_request("t", &format!("tool_{i}"), Outcome::Ok);
        }
        // 257th name spills to "other".
        bump_request("t", "tool_256", Outcome::Ok);
        let body = render_prometheus();
        assert!(
            body.contains("tool=\"other\""),
            "expected other label, body did not contain it"
        );
    }

    #[test]
    #[serial_test::serial]
    fn progress_notification_labels_only_ok_or_drop() {
        isolate();
        bump_progress_notification(ProgressEmitOutcome::Ok);
        bump_progress_notification(ProgressEmitOutcome::Ok);
        bump_progress_notification(ProgressEmitOutcome::Drop);
        let body = render_prometheus();
        assert!(body.contains("mcp_progress_notifications_total{outcome=\"ok\"} 2"));
        assert!(body.contains("mcp_progress_notifications_total{outcome=\"drop\"} 1"));
    }

    // `global_gauges_render` removed — gauges live in
    // `crates/mcp/src/telemetry.rs` (top-level session-lifecycle
    // module), not in this server-side dispatch module.

    #[test]
    #[serial_test::serial]
    fn rate_limit_timeout_rejection_counters() {
        isolate();
        bump_rate_limit_hit("t", "echo");
        bump_timeout("t", "echo");
        bump_concurrency_rejection("t", "echo");
        let body = render_prometheus();
        assert!(body.contains("mcp_rate_limit_hits_total{tenant=\"t\",tool=\"echo\"} 1"));
        assert!(body.contains("mcp_timeouts_total{tenant=\"t\",tool=\"echo\"} 1"));
        assert!(body.contains("mcp_concurrency_rejections_total{tenant=\"t\",tool=\"echo\"} 1"));
    }

    #[test]
    #[serial_test::serial]
    fn render_emits_help_and_type_lines_for_every_family() {
        isolate();
        bump_request("t", "echo", Outcome::Ok);
        let body = render_prometheus();
        for family in [
            "mcp_requests_total",
            "mcp_request_duration_seconds",
            "mcp_in_flight",
            "mcp_rate_limit_hits_total",
            "mcp_timeouts_total",
            "mcp_concurrency_rejections_total",
            "mcp_progress_notifications_total",
        ] {
            assert!(
                body.contains(&format!("# HELP {family} ")),
                "missing HELP for {family}"
            );
            assert!(
                body.contains(&format!("# TYPE {family} ")),
                "missing TYPE for {family}"
            );
        }
    }
}
