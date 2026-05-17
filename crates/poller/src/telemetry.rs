//! Prometheus surface for the poller subsystem. Self-contained:
//! `main.rs` concatenates `render_prometheus()` into the same body
//! served at `/metrics` — same pattern as `nexo_auth::telemetry`.

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

/// Phase 96 follow-up — generic named counter store the
/// `PollerHost::metric_inc` API + the daemon-side reverse-RPC
/// handler fan out into. Lets a subprocess poller emit arbitrary
/// per-plugin metrics that show up in `/metrics` without the
/// daemon needing per-kind hardcoded counters.
///
/// Key shape: `(name, sorted_label_pairs)`. Names + labels are
/// sanitized to the Prometheus naming alphabet
/// (`[a-zA-Z_:][a-zA-Z0-9_:]*` for names,
/// `[a-zA-Z_][a-zA-Z0-9_]*` for label keys) — bad inputs get
/// `_` substituted. Label value escaping reuses [`escape`].
static PLUGIN_COUNTERS: LazyLock<DashMap<PluginCounterKey, AtomicU64>> =
    LazyLock::new(DashMap::new);

/// Per-counter label cardinality cap. Beyond this, the
/// excess labels are silently dropped — a single misbehaving
/// plugin cannot blow up the daemon's `/metrics` payload.
const PLUGIN_COUNTER_MAX_LABELS: usize = 10;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct PluginCounterKey {
    name: String,
    /// Sorted-by-key pairs so two equivalent labelings hash to
    /// the same counter regardless of insertion order in the JSON
    /// object the plugin sent.
    labels: Vec<(String, String)>,
}

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

/// Phase 96 follow-up — generic named counter fan-out for
/// `PollerHost::metric_inc`. `name` is sanitized to the
/// Prometheus name alphabet, `labels` must be a JSON object whose
/// string values become label values; non-object inputs are
/// silently treated as labelless. Counter increments by 1 per call;
/// the host could later expose `inc_by(n)` if a use case justifies.
///
/// Cardinality is capped at [`PLUGIN_COUNTER_MAX_LABELS`] labels
/// per counter to prevent a misbehaving plugin from blowing up the
/// `/metrics` payload. Excess labels (sorted lexicographically) are
/// silently dropped.
pub fn inc_named_counter(name: &str, labels: &serde_json::Value) {
    let key = build_plugin_counter_key(name, labels);
    PLUGIN_COUNTERS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

fn build_plugin_counter_key(name: &str, labels: &serde_json::Value) -> PluginCounterKey {
    let mut pairs: Vec<(String, String)> = match labels {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                let key = sanitize_label_name(k);
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                };
                (key, val)
            })
            .collect(),
        _ => Vec::new(),
    };
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    if pairs.len() > PLUGIN_COUNTER_MAX_LABELS {
        pairs.truncate(PLUGIN_COUNTER_MAX_LABELS);
    }
    PluginCounterKey {
        name: sanitize_counter_name(name),
        labels: pairs,
    }
}

/// Prometheus counter name alphabet: `[a-zA-Z_:][a-zA-Z0-9_:]*`.
/// Replace invalid chars with `_`. Empty input → `_invalid_`.
fn sanitize_counter_name(input: &str) -> String {
    if input.is_empty() {
        return "_invalid_".to_string();
    }
    let mut out = String::with_capacity(input.len());
    for (i, c) in input.chars().enumerate() {
        let ok = if i == 0 {
            c.is_ascii_alphabetic() || c == '_' || c == ':'
        } else {
            c.is_ascii_alphanumeric() || c == '_' || c == ':'
        };
        out.push(if ok { c } else { '_' });
    }
    out
}

/// Prometheus label name alphabet: `[a-zA-Z_][a-zA-Z0-9_]*`.
/// Label names starting with `__` are reserved; we replace the
/// leading `_` with `x` so well-meaning plugins can't reach the
/// reserved namespace.
fn sanitize_label_name(input: &str) -> String {
    if input.is_empty() {
        return "_invalid_".to_string();
    }
    let mut out = String::with_capacity(input.len());
    for (i, c) in input.chars().enumerate() {
        let ok = if i == 0 {
            c.is_ascii_alphabetic() || c == '_'
        } else {
            c.is_ascii_alphanumeric() || c == '_'
        };
        out.push(if ok { c } else { '_' });
    }
    if out.starts_with("__") {
        out.replace_range(0..1, "x");
    }
    out
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
            out.push_str("poller_ticks_total{agent=\"\",job_id=\"\",kind=\"\",status=\"\"} 0\n");
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
        keys.sort_by(|a, b| (a.kind, &a.agent, &a.job_id).cmp(&(b.kind, &b.agent, &b.job_id)));
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
            out.push_str(&format!("{name}{{agent=\"\",job_id=\"\",kind=\"\"}} 0\n"));
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

    out.push_str(
        "# HELP poller_lease_takeovers_total Times another worker took over an expired lease.\n",
    );
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

    // Phase 96 follow-up — emit every plugin-side counter fanned in
    // via `PollerHost::metric_inc`. Grouped by name so consumers of
    // the textual `/metrics` output see one HELP/TYPE pair per
    // counter family. Counters with empty store are not emitted
    // (they'd require a HELP without any sample lines).
    {
        let mut grouped: std::collections::BTreeMap<String, Vec<(Vec<(String, String)>, u64)>> =
            std::collections::BTreeMap::new();
        for entry in PLUGIN_COUNTERS.iter() {
            let key = entry.key().clone();
            let v = entry.value().load(Ordering::Relaxed);
            grouped
                .entry(key.name.clone())
                .or_default()
                .push((key.labels, v));
        }
        for (name, mut rows) in grouped {
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            out.push_str(&format!(
                "# HELP {name} Plugin-emitted counter (via PollerHost::metric_inc).\n"
            ));
            out.push_str(&format!("# TYPE {name} counter\n"));
            for (labels, v) in rows {
                if labels.is_empty() {
                    out.push_str(&format!("{name} {v}\n"));
                } else {
                    let label_str = labels
                        .iter()
                        .map(|(k, val)| format!("{k}=\"{}\"", escape(val)))
                        .collect::<Vec<_>>()
                        .join(",");
                    out.push_str(&format!("{name}{{{label_str}}} {v}\n"));
                }
            }
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
    fn sanitize_counter_name_replaces_invalid_chars() {
        assert_eq!(
            sanitize_counter_name("gmail.tick.total"),
            "gmail_tick_total"
        );
        assert_eq!(sanitize_counter_name("gmail tick"), "gmail_tick");
        assert_eq!(sanitize_counter_name("9bad_lead"), "_bad_lead");
        assert_eq!(sanitize_counter_name(""), "_invalid_");
        assert_eq!(sanitize_counter_name("ok_name"), "ok_name");
        assert_eq!(
            sanitize_counter_name("namespace:counter"),
            "namespace:counter"
        );
    }

    #[test]
    fn sanitize_label_name_disallows_reserved_prefix() {
        assert_eq!(sanitize_label_name("__reserved"), "x_reserved");
        assert_eq!(sanitize_label_name("ok"), "ok");
        assert_eq!(sanitize_label_name("with.dot"), "with_dot");
        assert_eq!(sanitize_label_name(""), "_invalid_");
    }

    #[test]
    fn plugin_counter_increments_under_same_label_set() {
        inc_named_counter(
            "test_plugin_counter_one",
            &serde_json::json!({ "kind": "rss", "agent": "ana" }),
        );
        inc_named_counter(
            "test_plugin_counter_one",
            &serde_json::json!({ "agent": "ana", "kind": "rss" }),
        );
        // Differently-ordered keys collapse to the same key — both
        // ticks land on a single counter at value 2.
        let key = build_plugin_counter_key(
            "test_plugin_counter_one",
            &serde_json::json!({ "kind": "rss", "agent": "ana" }),
        );
        let v = PLUGIN_COUNTERS
            .get(&key)
            .map(|e| e.value().load(Ordering::Relaxed))
            .unwrap_or(0);
        assert_eq!(v, 2);
    }

    #[test]
    fn plugin_counter_label_cardinality_capped() {
        let mut labels = serde_json::Map::new();
        for i in 0..50 {
            labels.insert(format!("k{i}"), serde_json::json!(format!("v{i}")));
        }
        let key = build_plugin_counter_key(
            "test_plugin_counter_cap",
            &serde_json::Value::Object(labels),
        );
        assert_eq!(key.labels.len(), PLUGIN_COUNTER_MAX_LABELS);
        // Sorted-lex truncation keeps the first 10 keys (k0,k1,...,k17
        // with leading-zero sort caveat — k0,k1,k10..k17 actually,
        // because k10 < k2 lexically).
        assert!(key.labels.iter().any(|(k, _)| k == "k0"));
    }

    #[test]
    fn plugin_counter_renders_with_labels() {
        inc_named_counter(
            "test_plugin_counter_render",
            &serde_json::json!({ "channel": "whatsapp" }),
        );
        inc_named_counter(
            "test_plugin_counter_render",
            &serde_json::json!({ "channel": "telegram" }),
        );
        inc_named_counter(
            "test_plugin_counter_render",
            &serde_json::json!({ "channel": "whatsapp" }),
        );
        let body = render_prometheus();
        assert!(body.contains("# TYPE test_plugin_counter_render counter"));
        assert!(body.contains("test_plugin_counter_render{channel=\"whatsapp\"} 2"));
        assert!(body.contains("test_plugin_counter_render{channel=\"telegram\"} 1"));
    }

    #[test]
    fn plugin_counter_renders_labelless() {
        inc_named_counter("test_plugin_counter_no_labels", &serde_json::Value::Null);
        let body = render_prometheus();
        assert!(body.contains("test_plugin_counter_no_labels 1"));
    }

    #[test]
    fn plugin_counter_escapes_label_value_quotes() {
        inc_named_counter(
            "test_plugin_counter_escape",
            &serde_json::json!({ "topic": "with\"quote" }),
        );
        let body = render_prometheus();
        assert!(body.contains("test_plugin_counter_escape{topic=\"with\\\"quote\"} 1"));
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
