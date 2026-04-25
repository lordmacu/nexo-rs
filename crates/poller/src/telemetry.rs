//! Prometheus surface for the poller subsystem. Self-contained:
//! `main.rs` concatenates `render_prometheus()` into the same body
//! served at `/metrics` — same pattern as `agent_auth::telemetry`.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::LazyLock;

use dashmap::DashMap;

const LATENCY_BUCKETS_MS: [u64; 8] = [50, 100, 250, 500, 1000, 2500, 5000, 10000];

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct TickKey {
    kind: &'static str,
    agent: String,
    job_id: String,
    status: &'static str, // "ok" | "transient" | "permanent" | "skipped"
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct LatencyKey {
    kind: &'static str,
    agent: String,
    job_id: String,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct ItemsKey {
    kind: &'static str,
    agent: String,
    job_id: String,
}

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
    fn observe(&self, ms: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_ms.fetch_add(ms, Ordering::Relaxed);
        for (i, upper) in LATENCY_BUCKETS_MS.iter().enumerate() {
            if ms <= *upper {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

static TICKS: LazyLock<DashMap<TickKey, AtomicU64>> = LazyLock::new(DashMap::new);
static LATENCY: LazyLock<DashMap<LatencyKey, Histogram>> = LazyLock::new(DashMap::new);
static ITEMS_SEEN: LazyLock<DashMap<ItemsKey, AtomicU64>> = LazyLock::new(DashMap::new);
static ITEMS_DISPATCHED: LazyLock<DashMap<ItemsKey, AtomicU64>> = LazyLock::new(DashMap::new);
static CONSEC_ERR: LazyLock<DashMap<String, AtomicI64>> = LazyLock::new(DashMap::new);
static BREAKER: LazyLock<DashMap<String, AtomicI64>> = LazyLock::new(DashMap::new);
static LEASE_TAKEOVERS: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);

#[derive(Copy, Clone, Debug)]
pub enum BreakerState {
    Closed = 0,
    HalfOpen = 1,
    Open = 2,
}

pub fn inc_tick(kind: &'static str, agent: &str, job_id: &str, status: &'static str) {
    TICKS
        .entry(TickKey {
            kind,
            agent: agent.to_string(),
            job_id: job_id.to_string(),
            status,
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn observe_latency(kind: &'static str, agent: &str, job_id: &str, ms: u64) {
    LATENCY
        .entry(LatencyKey {
            kind,
            agent: agent.to_string(),
            job_id: job_id.to_string(),
        })
        .or_insert_with(Histogram::new)
        .observe(ms);
}

pub fn add_items_seen(kind: &'static str, agent: &str, job_id: &str, n: u32) {
    ITEMS_SEEN
        .entry(ItemsKey {
            kind,
            agent: agent.to_string(),
            job_id: job_id.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(n as u64, Ordering::Relaxed);
}

pub fn add_items_dispatched(kind: &'static str, agent: &str, job_id: &str, n: u32) {
    ITEMS_DISPATCHED
        .entry(ItemsKey {
            kind,
            agent: agent.to_string(),
            job_id: job_id.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(n as u64, Ordering::Relaxed);
}

pub fn set_consecutive_errors(job_id: &str, count: i64) {
    CONSEC_ERR
        .entry(job_id.to_string())
        .or_insert_with(|| AtomicI64::new(0))
        .store(count, Ordering::Relaxed);
}

pub fn set_breaker_state(job_id: &str, state: BreakerState) {
    BREAKER
        .entry(job_id.to_string())
        .or_insert_with(|| AtomicI64::new(0))
        .store(state as i64, Ordering::Relaxed);
}

pub fn inc_lease_takeover(job_id: &str) {
    LEASE_TAKEOVERS
        .entry(job_id.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
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

pub fn render_prometheus() -> String {
    let mut out = String::new();

    out.push_str("# HELP poller_ticks_total Total poller ticks by kind/agent/job/status.\n");
    out.push_str("# TYPE poller_ticks_total counter\n");
    {
        let mut rows: Vec<_> = TICKS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.kind, &a.0.agent, &a.0.job_id, a.0.status).cmp(&(
                b.0.kind,
                &b.0.agent,
                &b.0.job_id,
                b.0.status,
            ))
        });
        if rows.is_empty() {
            out.push_str(
                "poller_ticks_total{agent=\"\",job_id=\"\",kind=\"\",status=\"\"} 0\n",
            );
        }
        for (k, v) in rows {
            out.push_str(&format!(
                "poller_ticks_total{{agent=\"{}\",job_id=\"{}\",kind=\"{}\",status=\"{}\"}} {v}\n",
                escape(&k.agent),
                escape(&k.job_id),
                k.kind,
                k.status,
            ));
        }
    }

    out.push_str("# HELP poller_latency_ms Tick wall-clock duration histogram.\n");
    out.push_str("# TYPE poller_latency_ms histogram\n");
    {
        let mut keys: Vec<_> = LATENCY.iter().map(|e| e.key().clone()).collect();
        keys.sort_by(|a, b| {
            (a.kind, &a.agent, &a.job_id).cmp(&(b.kind, &b.agent, &b.job_id))
        });
        if keys.is_empty() {
            for upper in LATENCY_BUCKETS_MS.iter() {
                out.push_str(&format!(
                    "poller_latency_ms_bucket{{agent=\"\",job_id=\"\",kind=\"\",le=\"{upper}\"}} 0\n"
                ));
            }
            out.push_str(
                "poller_latency_ms_bucket{agent=\"\",job_id=\"\",kind=\"\",le=\"+Inf\"} 0\n",
            );
            out.push_str("poller_latency_ms_sum{agent=\"\",job_id=\"\",kind=\"\"} 0\n");
            out.push_str("poller_latency_ms_count{agent=\"\",job_id=\"\",kind=\"\"} 0\n");
        }
        for k in keys {
            let Some(h) = LATENCY.get(&k) else { continue };
            let agent = escape(&k.agent);
            let job_id = escape(&k.job_id);
            for (i, upper) in LATENCY_BUCKETS_MS.iter().enumerate() {
                out.push_str(&format!(
                    "poller_latency_ms_bucket{{agent=\"{agent}\",job_id=\"{job_id}\",kind=\"{}\",le=\"{upper}\"}} {}\n",
                    k.kind,
                    h.buckets[i].load(Ordering::Relaxed)
                ));
            }
            let count = h.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "poller_latency_ms_bucket{{agent=\"{agent}\",job_id=\"{job_id}\",kind=\"{}\",le=\"+Inf\"}} {count}\n",
                k.kind
            ));
            out.push_str(&format!(
                "poller_latency_ms_sum{{agent=\"{agent}\",job_id=\"{job_id}\",kind=\"{}\"}} {}\n",
                k.kind,
                h.sum_ms.load(Ordering::Relaxed),
            ));
            out.push_str(&format!(
                "poller_latency_ms_count{{agent=\"{agent}\",job_id=\"{job_id}\",kind=\"{}\"}} {count}\n",
                k.kind,
            ));
        }
    }

    for (name, help, store) in [
        (
            "poller_items_seen_total",
            "Items the source returned this tick (whether dispatched or not).",
            &*ITEMS_SEEN,
        ),
        (
            "poller_items_dispatched_total",
            "Items the runner published downstream this tick.",
            &*ITEMS_DISPATCHED,
        ),
    ] {
        out.push_str(&format!("# HELP {name} {help}\n"));
        out.push_str(&format!("# TYPE {name} counter\n"));
        let mut rows: Vec<_> = store
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.kind, &a.0.agent, &a.0.job_id).cmp(&(b.0.kind, &b.0.agent, &b.0.job_id))
        });
        if rows.is_empty() {
            out.push_str(&format!(
                "{name}{{agent=\"\",job_id=\"\",kind=\"\"}} 0\n"
            ));
        }
        for (k, v) in rows {
            out.push_str(&format!(
                "{name}{{agent=\"{}\",job_id=\"{}\",kind=\"{}\"}} {v}\n",
                escape(&k.agent),
                escape(&k.job_id),
                k.kind,
            ));
        }
    }

    out.push_str("# HELP poller_consecutive_errors Consecutive failures since last success.\n");
    out.push_str("# TYPE poller_consecutive_errors gauge\n");
    {
        let mut rows: Vec<_> = CONSEC_ERR
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        if rows.is_empty() {
            out.push_str("poller_consecutive_errors{job_id=\"\"} 0\n");
        }
        for (id, v) in rows {
            out.push_str(&format!(
                "poller_consecutive_errors{{job_id=\"{}\"}} {v}\n",
                escape(&id)
            ));
        }
    }

    out.push_str("# HELP poller_breaker_state 0=closed,1=half-open,2=open per job.\n");
    out.push_str("# TYPE poller_breaker_state gauge\n");
    {
        let mut rows: Vec<_> = BREAKER
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        if rows.is_empty() {
            out.push_str("poller_breaker_state{job_id=\"\"} 0\n");
        }
        for (id, v) in rows {
            out.push_str(&format!(
                "poller_breaker_state{{job_id=\"{}\"}} {v}\n",
                escape(&id)
            ));
        }
    }

    out.push_str("# HELP poller_lease_takeovers_total Times another worker took over an expired lease.\n");
    out.push_str("# TYPE poller_lease_takeovers_total counter\n");
    {
        let mut rows: Vec<_> = LEASE_TAKEOVERS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        if rows.is_empty() {
            out.push_str("poller_lease_takeovers_total{job_id=\"\"} 0\n");
        }
        for (id, v) in rows {
            out.push_str(&format!(
                "poller_lease_takeovers_total{{job_id=\"{}\"}} {v}\n",
                escape(&id)
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_every_metric_when_empty() {
        let body = render_prometheus();
        for name in [
            "poller_ticks_total",
            "poller_latency_ms",
            "poller_items_seen_total",
            "poller_items_dispatched_total",
            "poller_consecutive_errors",
            "poller_breaker_state",
            "poller_lease_takeovers_total",
        ] {
            assert!(body.contains(&format!("# TYPE {name}")), "missing {name}");
        }
    }

    #[test]
    fn ticks_render_with_labels() {
        inc_tick("gmail", "ana", "ana_leads", "ok");
        inc_tick("gmail", "ana", "ana_leads", "ok");
        inc_tick("rss", "kate", "kate_blog", "transient");
        let body = render_prometheus();
        assert!(body.contains(
            "poller_ticks_total{agent=\"ana\",job_id=\"ana_leads\",kind=\"gmail\",status=\"ok\"} 2"
        ));
        assert!(body.contains(
            "poller_ticks_total{agent=\"kate\",job_id=\"kate_blog\",kind=\"rss\",status=\"transient\"} 1"
        ));
    }

    #[test]
    fn breaker_gauge_round_trips() {
        set_breaker_state("ana_leads", BreakerState::Open);
        let body = render_prometheus();
        assert!(body.contains("poller_breaker_state{job_id=\"ana_leads\"} 2"));
    }

    #[test]
    fn escape_quotes_and_backslashes() {
        assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
