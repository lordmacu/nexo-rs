//! Prometheus metrics for snapshot operations.
//!
//! The pattern mirrors `nexo-llm::telemetry`, `nexo-web-search::telemetry`,
//! and `nexo-mcp::server::telemetry`: process-wide `LazyLock<DashMap>`
//! counters + a hand-rolled text rendering. No `prometheus` crate dep,
//! no metrics server here — operators stitch [`render_prometheus`] into
//! the runtime's `/metrics` aggregator.
//!
//! Cardinality discipline: `agent_id` and `tenant` segments are bounded
//! to 256 distinct values per process. Beyond that limit they collapse
//! into `"other"` so a misbehaving caller cannot blow up Prometheus
//! storage.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use dashmap::DashMap;

const AGENT_LABEL_CAP: usize = 256;
const TENANT_LABEL_CAP: usize = 256;

/// 8 buckets aligned with the durations operators care about for
/// snapshot work: 50 ms (in-memory only) up to 60 s (large memdir +
/// big SQLite + zstd-19 pack).
const DURATION_BUCKETS_MS: &[u64] = &[50, 100, 250, 500, 1_000, 5_000, 15_000, 60_000];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    Ok,
    Error,
    Concurrent,
    SchemaTooNew,
    ChecksumMismatch,
    Refused,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Error => "error",
            Outcome::Concurrent => "concurrent",
            Outcome::SchemaTooNew => "schema_too_new",
            Outcome::ChecksumMismatch => "checksum_mismatch",
            Outcome::Refused => "refused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OutcomeKey {
    agent: String,
    tenant: String,
    outcome: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AgentTenantKey {
    agent: String,
    tenant: String,
}

#[derive(Default)]
struct Histogram {
    buckets: [AtomicU64; 8],
    overflow: AtomicU64,
    sum_ms: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn record(&self, ms: u64) {
        for (i, ceiling) in DURATION_BUCKETS_MS.iter().enumerate() {
            if ms <= *ceiling {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                self.sum_ms.fetch_add(ms, Ordering::Relaxed);
                self.count.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.overflow.fetch_add(1, Ordering::Relaxed);
        self.sum_ms.fetch_add(ms, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

static SNAPSHOT_TOTAL: LazyLock<DashMap<OutcomeKey, AtomicU64>> = LazyLock::new(DashMap::new);
static RESTORE_TOTAL: LazyLock<DashMap<OutcomeKey, AtomicU64>> = LazyLock::new(DashMap::new);
static GC_TOTAL: LazyLock<DashMap<AgentTenantKey, AtomicU64>> = LazyLock::new(DashMap::new);
static SNAPSHOT_DURATION: LazyLock<DashMap<AgentTenantKey, Histogram>> = LazyLock::new(DashMap::new);
static SNAPSHOT_BYTES_TOTAL: LazyLock<DashMap<AgentTenantKey, AtomicU64>> =
    LazyLock::new(DashMap::new);

/// Bound the `agent` / `tenant` labels so a typo cascade or attacker
/// cannot blow Prometheus cardinality.
fn cap_label(value: &str, dict: &DashMap<String, ()>, cap: usize) -> String {
    if dict.contains_key(value) {
        return value.to_string();
    }
    if dict.len() >= cap {
        return "other".to_string();
    }
    dict.insert(value.to_string(), ());
    value.to_string()
}

static AGENT_DICT: LazyLock<DashMap<String, ()>> = LazyLock::new(DashMap::new);
static TENANT_DICT: LazyLock<DashMap<String, ()>> = LazyLock::new(DashMap::new);

pub fn record_snapshot(agent: &str, tenant: &str, outcome: Outcome, duration: Duration, size_bytes: u64) {
    let agent = cap_label(agent, &AGENT_DICT, AGENT_LABEL_CAP);
    let tenant = cap_label(tenant, &TENANT_DICT, TENANT_LABEL_CAP);
    let key = OutcomeKey {
        agent: agent.clone(),
        tenant: tenant.clone(),
        outcome: outcome.label(),
    };
    SNAPSHOT_TOTAL
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);

    let at_key = AgentTenantKey {
        agent: agent.clone(),
        tenant: tenant.clone(),
    };
    SNAPSHOT_DURATION
        .entry(at_key.clone())
        .or_insert_with(Histogram::default)
        .record(duration.as_millis().min(u128::from(u64::MAX)) as u64);
    if size_bytes > 0 {
        SNAPSHOT_BYTES_TOTAL
            .entry(at_key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(size_bytes, Ordering::Relaxed);
    }
}

pub fn record_restore(agent: &str, tenant: &str, outcome: Outcome) {
    let agent = cap_label(agent, &AGENT_DICT, AGENT_LABEL_CAP);
    let tenant = cap_label(tenant, &TENANT_DICT, TENANT_LABEL_CAP);
    RESTORE_TOTAL
        .entry(OutcomeKey {
            agent,
            tenant,
            outcome: outcome.label(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn record_gc(agent: &str, tenant: &str, deleted: u64) {
    let agent = cap_label(agent, &AGENT_DICT, AGENT_LABEL_CAP);
    let tenant = cap_label(tenant, &TENANT_DICT, TENANT_LABEL_CAP);
    GC_TOTAL
        .entry(AgentTenantKey { agent, tenant })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(deleted, Ordering::Relaxed);
}

/// Render every metric in the Prometheus text exposition format. Stitch
/// into the runtime's `/metrics` handler alongside the other crate
/// renderers.
pub fn render_prometheus() -> String {
    let mut out = String::new();

    out.push_str("# HELP nexo_memory_snapshot_total Snapshot operations.\n");
    out.push_str("# TYPE nexo_memory_snapshot_total counter\n");
    for entry in SNAPSHOT_TOTAL.iter() {
        let k = entry.key();
        let v = entry.value().load(Ordering::Relaxed);
        out.push_str(&format!(
            "nexo_memory_snapshot_total{{agent=\"{}\",tenant=\"{}\",outcome=\"{}\"}} {v}\n",
            k.agent, k.tenant, k.outcome
        ));
    }

    out.push_str("# HELP nexo_memory_snapshot_restore_total Restore operations.\n");
    out.push_str("# TYPE nexo_memory_snapshot_restore_total counter\n");
    for entry in RESTORE_TOTAL.iter() {
        let k = entry.key();
        let v = entry.value().load(Ordering::Relaxed);
        out.push_str(&format!(
            "nexo_memory_snapshot_restore_total{{agent=\"{}\",tenant=\"{}\",outcome=\"{}\"}} {v}\n",
            k.agent, k.tenant, k.outcome
        ));
    }

    out.push_str("# HELP nexo_memory_snapshot_gc_total Bundles deleted by retention.\n");
    out.push_str("# TYPE nexo_memory_snapshot_gc_total counter\n");
    for entry in GC_TOTAL.iter() {
        let k = entry.key();
        let v = entry.value().load(Ordering::Relaxed);
        out.push_str(&format!(
            "nexo_memory_snapshot_gc_total{{agent=\"{}\",tenant=\"{}\"}} {v}\n",
            k.agent, k.tenant
        ));
    }

    out.push_str("# HELP nexo_memory_snapshot_bytes_total Total bytes packed across snapshots.\n");
    out.push_str("# TYPE nexo_memory_snapshot_bytes_total counter\n");
    for entry in SNAPSHOT_BYTES_TOTAL.iter() {
        let k = entry.key();
        let v = entry.value().load(Ordering::Relaxed);
        out.push_str(&format!(
            "nexo_memory_snapshot_bytes_total{{agent=\"{}\",tenant=\"{}\"}} {v}\n",
            k.agent, k.tenant
        ));
    }

    out.push_str("# HELP nexo_memory_snapshot_duration_ms Snapshot wall-clock duration.\n");
    out.push_str("# TYPE nexo_memory_snapshot_duration_ms histogram\n");
    for entry in SNAPSHOT_DURATION.iter() {
        let k = entry.key();
        let h = entry.value();
        let mut cumulative: u64 = 0;
        for (i, ceiling) in DURATION_BUCKETS_MS.iter().enumerate() {
            cumulative += h.buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "nexo_memory_snapshot_duration_ms_bucket{{agent=\"{}\",tenant=\"{}\",le=\"{}\"}} {cumulative}\n",
                k.agent, k.tenant, ceiling
            ));
        }
        cumulative += h.overflow.load(Ordering::Relaxed);
        out.push_str(&format!(
            "nexo_memory_snapshot_duration_ms_bucket{{agent=\"{}\",tenant=\"{}\",le=\"+Inf\"}} {cumulative}\n",
            k.agent, k.tenant
        ));
        out.push_str(&format!(
            "nexo_memory_snapshot_duration_ms_sum{{agent=\"{}\",tenant=\"{}\"}} {}\n",
            k.agent,
            k.tenant,
            h.sum_ms.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "nexo_memory_snapshot_duration_ms_count{{agent=\"{}\",tenant=\"{}\"}} {}\n",
            k.agent,
            k.tenant,
            h.count.load(Ordering::Relaxed)
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Metrics globals are process-wide so the tests must serialise.
    /// The same pattern lands in `nexo-mcp`'s telemetry tests.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset() {
        SNAPSHOT_TOTAL.clear();
        RESTORE_TOTAL.clear();
        GC_TOTAL.clear();
        SNAPSHOT_DURATION.clear();
        SNAPSHOT_BYTES_TOTAL.clear();
        AGENT_DICT.clear();
        TENANT_DICT.clear();
    }

    #[test]
    fn records_snapshot_increments_counter_and_histogram() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        record_snapshot("ana", "default", Outcome::Ok, Duration::from_millis(120), 4096);
        record_snapshot("ana", "default", Outcome::Ok, Duration::from_millis(80), 2048);
        let body = render_prometheus();
        assert!(
            body.contains("nexo_memory_snapshot_total{agent=\"ana\",tenant=\"default\",outcome=\"ok\"} 2"),
            "{body}"
        );
        assert!(body.contains("nexo_memory_snapshot_bytes_total{agent=\"ana\",tenant=\"default\"} 6144"));
        // 120 ms hits the 250-bucket; 80 ms hits 100-bucket. So
        // cumulative at le=250 is 2 and at le=100 is 1.
        assert!(body.contains("le=\"100\"} 1"));
        assert!(body.contains("le=\"250\"} 2"));
    }

    #[test]
    fn restore_outcome_distinguishes_ok_from_error() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        record_restore("ana", "default", Outcome::Ok);
        record_restore("ana", "default", Outcome::SchemaTooNew);
        let body = render_prometheus();
        assert!(body.contains("outcome=\"ok\"} 1"));
        assert!(body.contains("outcome=\"schema_too_new\"} 1"));
    }

    #[test]
    fn cardinality_cap_collapses_excess_agents_to_other() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        for i in 0..(AGENT_LABEL_CAP + 5) {
            record_snapshot(
                &format!("agent-{i}"),
                "default",
                Outcome::Ok,
                Duration::from_millis(1),
                0,
            );
        }
        let body = render_prometheus();
        assert!(body.contains("agent=\"other\""));
        // The first AGENT_LABEL_CAP labels should each appear once
        // (no collapse), the remainder collapses into `other`.
        let other_count = body
            .lines()
            .filter(|l| l.starts_with("nexo_memory_snapshot_total{agent=\"other\""))
            .count();
        assert!(other_count >= 1);
    }

    #[test]
    fn gc_counter_aggregates_across_calls() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        record_gc("ana", "default", 3);
        record_gc("ana", "default", 2);
        let body = render_prometheus();
        assert!(body.contains("nexo_memory_snapshot_gc_total{agent=\"ana\",tenant=\"default\"} 5"));
    }

    #[test]
    fn render_emits_help_and_type_headers() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let body = render_prometheus();
        for prefix in [
            "nexo_memory_snapshot_total",
            "nexo_memory_snapshot_restore_total",
            "nexo_memory_snapshot_gc_total",
            "nexo_memory_snapshot_bytes_total",
            "nexo_memory_snapshot_duration_ms",
        ] {
            assert!(body.contains(&format!("# HELP {prefix}")), "missing HELP for {prefix}");
            assert!(body.contains(&format!("# TYPE {prefix}")), "missing TYPE for {prefix}");
        }
    }
}
