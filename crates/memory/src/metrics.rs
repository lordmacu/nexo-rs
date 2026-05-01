//! Phase 86.1 — local memory-shape metrics.
//!
//! Process-wide counters + histograms covering recall and write
//! activity in `LongTermMemory`. Mirrors the lock-free pattern used
//! by `nexo-memory-snapshot::metrics`,
//! `nexo-llm::telemetry`, and `nexo-web-search::telemetry`:
//! `LazyLock<DashMap>` storage, hand-rolled Prometheus text
//! rendering, no `prometheus` crate dep, no metrics server here.
//! Operators stitch [`render_prometheus`] into the runtime's
//! `/metrics` aggregator.
//!
//! Cardinality discipline: `agent_id` is bounded to 256 distinct
//! values per process; beyond that the label collapses to
//! `"other"` so a misbehaving caller cannot blow up Prometheus
//! storage.
//!
//! Network-egress discipline (vs. the reference implementation):
//! every metric stays local. Zero outbound calls — the operator's
//! Grafana scrapes the same `/metrics` endpoint as the rest of the
//! daemon's signals. See PHASES.md §86 header for the rationale.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use dashmap::DashMap;

const AGENT_LABEL_CAP: usize = 256;

/// Memory taxonomy from Phase 77.5. Any string outside this set
/// collapses to `"other"` at emit time so the label space stays
/// small and well-known.
pub const KNOWN_TYPES: &[&str] = &["user", "feedback", "project", "reference"];

/// Recall-side scope tag. Mirrors [`MutationScope`] discriminants
/// from `nexo-memory::long_term::MutationScope` but kept here as
/// `&'static str` to dodge a circular import.
pub const KNOWN_SCOPES: &[&str] = &[
    "sqlite_long_term",
    "sqlite_vector",
    "sqlite_concepts",
    "git",
    "compaction",
];

/// Byte-size buckets for `memory_write_size_bytes` histogram.
/// Aligned with the spec (256 / 1k / 4k / 16k / 64k) plus an
/// implicit overflow row for anything bigger.
const SIZE_BUCKETS: &[u64] = &[256, 1_024, 4_096, 16_384, 65_536];

/// Age buckets (seconds) for `memory_age_at_recall_seconds`. Rough
/// log spacing covers fresh-recall (1 s) to long-tail
/// retrieval (1 yr).
const AGE_BUCKETS_SECS: &[u64] = &[
    1,
    60,                 // 1 min
    3_600,              // 1 hr
    86_400,             // 1 d
    7 * 86_400,         // 1 wk
    30 * 86_400,        // 1 mo
    365 * 86_400,       // 1 yr
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WriteKey {
    agent: String,
    kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RecallKey {
    agent: String,
    scope: &'static str,
}

#[derive(Debug, Default)]
struct RatioCell {
    selected: AtomicU64,
    available: AtomicU64,
}

struct Histogram<const N: usize> {
    buckets: [AtomicU64; N],
    overflow: AtomicU64,
    sum: AtomicU64,
    count: AtomicU64,
}

impl<const N: usize> Default for Histogram<N> {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            overflow: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }
}

impl<const N: usize> Histogram<N> {
    fn record(&self, value: u64, ceilings: &[u64]) {
        for (i, ceiling) in ceilings.iter().enumerate() {
            if value <= *ceiling {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                self.sum.fetch_add(value, Ordering::Relaxed);
                self.count.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.overflow.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(value, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn cumulative_at(&self, idx: usize) -> u64 {
        let mut sum = 0u64;
        for i in 0..=idx.min(N - 1) {
            sum = sum.saturating_add(self.buckets[i].load(Ordering::Relaxed));
        }
        sum
    }

    fn total(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    fn sum_total(&self) -> u64 {
        self.sum.load(Ordering::Relaxed)
    }
}

static WRITE_TOTAL: LazyLock<DashMap<WriteKey, AtomicU64>> = LazyLock::new(DashMap::new);
static RECALL_TOTAL: LazyLock<DashMap<RecallKey, AtomicU64>> = LazyLock::new(DashMap::new);
static RECALL_RATIO: LazyLock<DashMap<RecallKey, RatioCell>> = LazyLock::new(DashMap::new);
static WRITE_SIZE: LazyLock<Histogram<5>> = LazyLock::new(<Histogram<5>>::default);
static AGE_AT_RECALL: LazyLock<Histogram<7>> = LazyLock::new(<Histogram<7>>::default);

fn bound_agent(agent: &str) -> String {
    let len = WRITE_TOTAL.len() + RECALL_TOTAL.len();
    if len >= AGENT_LABEL_CAP * 2 {
        return "other".to_string();
    }
    agent.to_string()
}

fn known_type_label(kind: &str) -> &'static str {
    for &k in KNOWN_TYPES {
        if k.eq_ignore_ascii_case(kind) {
            return k;
        }
    }
    "other"
}

fn known_scope_label(scope: &str) -> &'static str {
    for &s in KNOWN_SCOPES {
        if s.eq_ignore_ascii_case(scope) {
            return s;
        }
    }
    "other"
}

/// Record one `LongTermMemory::remember_typed` call. `kind` is the
/// memory taxonomy label (`user` / `feedback` / `project` /
/// `reference`); unknown labels collapse to `"other"`.
pub fn record_write(agent_id: &str, kind: &str) {
    let key = WriteKey {
        agent: bound_agent(agent_id),
        kind: known_type_label(kind),
    };
    WRITE_TOTAL
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Record one body-byte size from a write fire-site (typically
/// `extract_memories::store_extracted`).
pub fn record_write_size(bytes: u64) {
    WRITE_SIZE.record(bytes, SIZE_BUCKETS);
}

/// Record one recall call. `scope` is the [`MutationScope`]
/// discriminant; unknown scopes collapse to `"other"`. `available`
/// is the candidate pool size, `selected` the count actually
/// returned to the caller. Both feed
/// `memory_recall_selected_ratio` so the operator sees recall
/// efficiency without per-call cardinality.
pub fn record_recall(agent_id: &str, scope: &str, available: u64, selected: u64) {
    let key = RecallKey {
        agent: bound_agent(agent_id),
        scope: known_scope_label(scope),
    };
    RECALL_TOTAL
        .entry(key.clone())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
    let cell = RECALL_RATIO.entry(key).or_default();
    cell.selected.fetch_add(selected, Ordering::Relaxed);
    cell.available.fetch_add(available, Ordering::Relaxed);
}

/// Record one recalled memory's age at retrieval time. `seconds`
/// is `Utc::now() - memory.created_at` clamped to non-negative.
/// Lets the operator surface long-tail retrieval ("memories aged
/// > 30 d are still being recalled").
pub fn record_age_at_recall(seconds: u64) {
    AGE_AT_RECALL.record(seconds, AGE_BUCKETS_SECS);
}

/// Render the four metric families as Prometheus text. Stitched
/// into the runtime's `/metrics` aggregator alongside the other
/// crate-local renderers.
pub fn render_prometheus() -> String {
    let mut out = String::with_capacity(2_048);

    out.push_str("# HELP memory_write_total Total memory writes (LongTermMemory::remember_typed) by agent + type.\n");
    out.push_str("# TYPE memory_write_total counter\n");
    for entry in WRITE_TOTAL.iter() {
        let key = entry.key();
        out.push_str(&format!(
            "memory_write_total{{agent_id=\"{}\",type=\"{}\"}} {}\n",
            esc(&key.agent),
            key.kind,
            entry.value().load(Ordering::Relaxed)
        ));
    }

    out.push_str("# HELP memory_recall_total Total recall calls by agent + scope.\n");
    out.push_str("# TYPE memory_recall_total counter\n");
    for entry in RECALL_TOTAL.iter() {
        let key = entry.key();
        out.push_str(&format!(
            "memory_recall_total{{agent_id=\"{}\",scope=\"{}\"}} {}\n",
            esc(&key.agent),
            key.scope,
            entry.value().load(Ordering::Relaxed)
        ));
    }

    out.push_str("# HELP memory_recall_selected_ratio Selected/available ratio of recall results (0.0–1.0).\n");
    out.push_str("# TYPE memory_recall_selected_ratio gauge\n");
    for entry in RECALL_RATIO.iter() {
        let key = entry.key();
        let cell = entry.value();
        let avail = cell.available.load(Ordering::Relaxed);
        let sel = cell.selected.load(Ordering::Relaxed);
        let ratio = if avail == 0 { 0.0 } else { sel as f64 / avail as f64 };
        out.push_str(&format!(
            "memory_recall_selected_ratio{{agent_id=\"{}\",scope=\"{}\"}} {ratio:.6}\n",
            esc(&key.agent),
            key.scope,
        ));
    }

    out.push_str("# HELP memory_write_size_bytes Distribution of memory write payload sizes.\n");
    out.push_str("# TYPE memory_write_size_bytes histogram\n");
    for (i, ceiling) in SIZE_BUCKETS.iter().enumerate() {
        out.push_str(&format!(
            "memory_write_size_bytes_bucket{{le=\"{}\"}} {}\n",
            ceiling,
            WRITE_SIZE.cumulative_at(i),
        ));
    }
    out.push_str(&format!(
        "memory_write_size_bytes_bucket{{le=\"+Inf\"}} {}\n",
        WRITE_SIZE.total(),
    ));
    out.push_str(&format!(
        "memory_write_size_bytes_count {}\n",
        WRITE_SIZE.total(),
    ));
    out.push_str(&format!(
        "memory_write_size_bytes_sum {}\n",
        WRITE_SIZE.sum_total(),
    ));

    out.push_str("# HELP memory_age_at_recall_seconds Distribution of recalled-memory ages.\n");
    out.push_str("# TYPE memory_age_at_recall_seconds histogram\n");
    for (i, ceiling) in AGE_BUCKETS_SECS.iter().enumerate() {
        out.push_str(&format!(
            "memory_age_at_recall_seconds_bucket{{le=\"{}\"}} {}\n",
            ceiling,
            AGE_AT_RECALL.cumulative_at(i),
        ));
    }
    out.push_str(&format!(
        "memory_age_at_recall_seconds_bucket{{le=\"+Inf\"}} {}\n",
        AGE_AT_RECALL.total(),
    ));
    out.push_str(&format!(
        "memory_age_at_recall_seconds_count {}\n",
        AGE_AT_RECALL.total(),
    ));
    out.push_str(&format!(
        "memory_age_at_recall_seconds_sum {}\n",
        AGE_AT_RECALL.sum_total(),
    ));

    out
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Test-only — drain every registry. Required because metric state
/// is process-global; tests that assert on counts must reset
/// between runs.
#[doc(hidden)]
pub fn _reset_for_tests() {
    WRITE_TOTAL.clear();
    RECALL_TOTAL.clear();
    RECALL_RATIO.clear();
    for slot in WRITE_SIZE.buckets.iter() {
        slot.store(0, Ordering::Relaxed);
    }
    WRITE_SIZE.overflow.store(0, Ordering::Relaxed);
    WRITE_SIZE.sum.store(0, Ordering::Relaxed);
    WRITE_SIZE.count.store(0, Ordering::Relaxed);
    for slot in AGE_AT_RECALL.buckets.iter() {
        slot.store(0, Ordering::Relaxed);
    }
    AGE_AT_RECALL.overflow.store(0, Ordering::Relaxed);
    AGE_AT_RECALL.sum.store(0, Ordering::Relaxed);
    AGE_AT_RECALL.count.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests share process-global state — serialise with a mutex
    /// so concurrent `cargo test` runs don't observe each other's
    /// counters. Each test starts with a `_reset_for_tests` to
    /// drop any state from a sibling test.
    static LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        _reset_for_tests();
        g
    }

    #[test]
    fn write_emit_increments_counter_with_known_type() {
        let _g = lock();
        record_write("ana", "user");
        record_write("ana", "user");
        record_write("ana", "feedback");
        let out = render_prometheus();
        assert!(out.contains("memory_write_total{agent_id=\"ana\",type=\"user\"} 2"));
        assert!(out.contains("memory_write_total{agent_id=\"ana\",type=\"feedback\"} 1"));
    }

    #[test]
    fn write_emit_collapses_unknown_type_to_other() {
        let _g = lock();
        record_write("ana", "ramblings");
        let out = render_prometheus();
        assert!(out.contains("memory_write_total{agent_id=\"ana\",type=\"other\"} 1"));
    }

    #[test]
    fn recall_hit_records_counter_and_ratio() {
        // Recall scenario where some candidates were selected.
        let _g = lock();
        record_recall("ana", "sqlite_long_term", 10, 4);
        record_recall("ana", "sqlite_long_term", 6, 3);
        let out = render_prometheus();
        assert!(out.contains(
            "memory_recall_total{agent_id=\"ana\",scope=\"sqlite_long_term\"} 2"
        ));
        // 7 selected / 16 available = 0.4375 (cumulative).
        assert!(
            out.contains("memory_recall_selected_ratio{agent_id=\"ana\",scope=\"sqlite_long_term\"} 0.437500"),
            "expected 0.437500 in:\n{out}"
        );
    }

    #[test]
    fn recall_miss_records_zero_ratio_when_available_is_zero() {
        // No candidates → zero division avoided, ratio is 0.0.
        let _g = lock();
        record_recall("ana", "sqlite_vector", 0, 0);
        let out = render_prometheus();
        assert!(out.contains(
            "memory_recall_total{agent_id=\"ana\",scope=\"sqlite_vector\"} 1"
        ));
        assert!(out.contains(
            "memory_recall_selected_ratio{agent_id=\"ana\",scope=\"sqlite_vector\"} 0.000000"
        ));
    }

    #[test]
    fn write_size_histogram_buckets_correctly() {
        let _g = lock();
        record_write_size(100); // 256 bucket
        record_write_size(2_000); // 4096 bucket (cumulative)
        record_write_size(100_000); // overflow
        let out = render_prometheus();
        // 100 ≤ 256, cumulative at le=256 is 1.
        assert!(out.contains("memory_write_size_bytes_bucket{le=\"256\"} 1"));
        // 2_000 ≤ 4_096, cumulative at le=4096 is 2.
        assert!(out.contains("memory_write_size_bytes_bucket{le=\"4096\"} 2"));
        // overflow lands in +Inf only.
        assert!(out.contains("memory_write_size_bytes_bucket{le=\"+Inf\"} 3"));
        assert!(out.contains("memory_write_size_bytes_count 3"));
    }

    #[test]
    fn age_at_recall_histogram_buckets_correctly() {
        let _g = lock();
        record_age_at_recall(0); // ≤1
        record_age_at_recall(30); // ≤60
        record_age_at_recall(86_400); // ≤1d
        record_age_at_recall(40_000_000); // overflow (>1yr)
        let out = render_prometheus();
        assert!(out.contains("memory_age_at_recall_seconds_bucket{le=\"1\"} 1"));
        assert!(out.contains("memory_age_at_recall_seconds_bucket{le=\"60\"} 2"));
        assert!(out.contains("memory_age_at_recall_seconds_bucket{le=\"86400\"} 3"));
        assert!(out.contains("memory_age_at_recall_seconds_count 4"));
    }

    #[test]
    fn agent_label_string_escapes_quotes_and_backslashes() {
        let _g = lock();
        record_write(r#"weird"agent\name"#, "user");
        let out = render_prometheus();
        assert!(
            out.contains(r#"memory_write_total{agent_id="weird\"agent\\name",type="user"} 1"#),
            "rendered: {out}"
        );
    }

    #[test]
    fn unknown_scope_collapses_to_other() {
        let _g = lock();
        record_recall("ana", "made_up", 1, 0);
        let out = render_prometheus();
        assert!(out.contains("memory_recall_total{agent_id=\"ana\",scope=\"other\"} 1"));
    }

    #[test]
    fn render_returns_help_and_type_lines() {
        let _g = lock();
        let out = render_prometheus();
        assert!(out.contains("# HELP memory_write_total"));
        assert!(out.contains("# TYPE memory_write_total counter"));
        assert!(out.contains("# HELP memory_recall_selected_ratio"));
        assert!(out.contains("# TYPE memory_recall_selected_ratio gauge"));
        assert!(out.contains("# TYPE memory_write_size_bytes histogram"));
        assert!(out.contains("# TYPE memory_age_at_recall_seconds histogram"));
    }
}
