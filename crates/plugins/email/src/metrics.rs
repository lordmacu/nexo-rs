//! Prometheus metrics for the email plugin (Phase 48 follow-up #8).
//!
//! Two flavours mixed:
//! - **Counters incremented at event sites** (`messages_fetched`,
//!   `loop_skipped`, `bounces`, `outbound_sent`, `outbound_failed`).
//!   Workers / handlers call the `inc_*` helpers; the values
//!   accumulate in process-globals.
//! - **Gauges sampled at render time** from a `health_map`
//!   reference passed to `render_prometheus`. Pull-on-scrape
//!   keeps the values authoritative without a background tick.
//!
//! Output format follows the Prometheus text format used by every
//! other crate's telemetry module (deterministic series ordering,
//! one-line `# HELP` / `# TYPE` headers).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use dashmap::DashMap;

use crate::dsn::BounceClassification;
use crate::health::WorkerState;
use crate::inbound::HealthMap;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct InstanceKey {
    instance: String,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct LoopKey {
    reason: &'static str,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct BounceKey {
    instance: String,
    classification: &'static str,
}

static MESSAGES_FETCHED: LazyLock<DashMap<InstanceKey, AtomicU64>> = LazyLock::new(DashMap::new);
static LOOP_SKIPPED: LazyLock<DashMap<LoopKey, AtomicU64>> = LazyLock::new(DashMap::new);
static BOUNCES: LazyLock<DashMap<BounceKey, AtomicU64>> = LazyLock::new(DashMap::new);
static OUTBOUND_SENT: LazyLock<DashMap<InstanceKey, AtomicU64>> = LazyLock::new(DashMap::new);
static OUTBOUND_FAILED: LazyLock<DashMap<InstanceKey, AtomicU64>> = LazyLock::new(DashMap::new);
// Audit follow-up H — attachment write failure counter (process-
// global, not per-instance because the de-dup dir is shared) and
// per-instance MIME parse errors (currently only WARN-logged).
static ATTACHMENT_WRITE_ERRORS: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static PARSE_ERRORS: LazyLock<DashMap<InstanceKey, AtomicU64>> = LazyLock::new(DashMap::new);

pub fn inc_messages_fetched(instance: &str) {
    MESSAGES_FETCHED
        .entry(InstanceKey {
            instance: instance.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_loop_skipped(reason: &'static str) {
    LOOP_SKIPPED
        .entry(LoopKey { reason })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_bounce(instance: &str, classification: BounceClassification) {
    let label = match classification {
        BounceClassification::Permanent => "permanent",
        BounceClassification::Transient => "transient",
        BounceClassification::Unknown => "unknown",
    };
    BOUNCES
        .entry(BounceKey {
            instance: instance.to_string(),
            classification: label,
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_outbound_sent(instance: &str) {
    OUTBOUND_SENT
        .entry(InstanceKey {
            instance: instance.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_attachment_write_error() {
    ATTACHMENT_WRITE_ERRORS.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_parse_error(instance: &str) {
    PARSE_ERRORS
        .entry(InstanceKey {
            instance: instance.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_outbound_failed(instance: &str) {
    OUTBOUND_FAILED
        .entry(InstanceKey {
            instance: instance.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Render every email metric in Prometheus text format. Caller
/// concatenates with the rest of the `/metrics` body. Pass the
/// optional `health` map so the gauges (`imap_state`,
/// `outbound_queue_depth`, `outbound_dlq_depth`) reflect the
/// current runtime instead of last-incremented values.
pub async fn render_prometheus(health: Option<&HealthMap>) -> String {
    let mut out = String::new();

    // Counters first — they have stable cardinality.
    out.push_str("# HELP email_imap_messages_fetched_total Total IMAP messages fetched per instance.\n");
    out.push_str("# TYPE email_imap_messages_fetched_total counter\n");
    {
        let mut rows: Vec<_> = MESSAGES_FETCHED
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.instance.cmp(&b.0.instance));
        if rows.is_empty() {
            out.push_str("email_imap_messages_fetched_total{instance=\"\"} 0\n");
        }
        for (key, v) in rows {
            out.push_str(&format!(
                "email_imap_messages_fetched_total{{instance=\"{}\"}} {v}\n",
                escape(&key.instance)
            ));
        }
    }

    out.push_str("# HELP email_loop_skipped_total Inbound messages suppressed by the loop guard.\n");
    out.push_str("# TYPE email_loop_skipped_total counter\n");
    {
        let mut rows: Vec<_> = LOOP_SKIPPED
            .iter()
            .map(|e| (e.key().reason, e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by_key(|a| a.0);
        if rows.is_empty() {
            out.push_str("email_loop_skipped_total{reason=\"\"} 0\n");
        }
        for (reason, v) in rows {
            out.push_str(&format!(
                "email_loop_skipped_total{{reason=\"{reason}\"}} {v}\n"
            ));
        }
    }

    out.push_str("# HELP email_bounces_total DSN events received per instance + classification.\n");
    out.push_str("# TYPE email_bounces_total counter\n");
    {
        let mut rows: Vec<_> = BOUNCES
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.instance.as_str(), a.0.classification)
                .cmp(&(b.0.instance.as_str(), b.0.classification))
        });
        if rows.is_empty() {
            out.push_str("email_bounces_total{classification=\"\",instance=\"\"} 0\n");
        }
        for (key, v) in rows {
            out.push_str(&format!(
                "email_bounces_total{{classification=\"{}\",instance=\"{}\"}} {v}\n",
                key.classification,
                escape(&key.instance),
            ));
        }
    }

    out.push_str("# HELP email_outbound_sent_total SMTP messages accepted by the relay.\n");
    out.push_str("# TYPE email_outbound_sent_total counter\n");
    render_instance_counter(&mut out, &OUTBOUND_SENT, "email_outbound_sent_total");

    out.push_str("# HELP email_outbound_failed_total SMTP messages that ended in DLQ (5xx or 5+ transients).\n");
    out.push_str("# TYPE email_outbound_failed_total counter\n");
    render_instance_counter(&mut out, &OUTBOUND_FAILED, "email_outbound_failed_total");

    out.push_str("# HELP email_attachment_write_errors_total Attachment write failures (disk full, fs error). Process-global because the de-dup dir is shared.\n");
    out.push_str("# TYPE email_attachment_write_errors_total counter\n");
    out.push_str(&format!(
        "email_attachment_write_errors_total {}\n",
        ATTACHMENT_WRITE_ERRORS.load(Ordering::Relaxed)
    ));

    out.push_str("# HELP email_parse_errors_total Inbound MIME parse failures per instance — message published raw-only.\n");
    out.push_str("# TYPE email_parse_errors_total counter\n");
    render_instance_counter(&mut out, &PARSE_ERRORS, "email_parse_errors_total");

    // Gauges — sampled at scrape time from health_map.
    out.push_str("# HELP email_imap_state IMAP worker state (0=connecting, 1=idle, 2=polling, 3=down).\n");
    out.push_str("# TYPE email_imap_state gauge\n");
    out.push_str("# HELP email_outbound_queue_depth Pending outbound jobs per instance.\n");
    out.push_str("# TYPE email_outbound_queue_depth gauge\n");
    out.push_str("# HELP email_outbound_dlq_depth Dead-lettered outbound jobs per instance.\n");
    out.push_str("# TYPE email_outbound_dlq_depth gauge\n");

    let mut state_lines = Vec::new();
    let mut queue_lines = Vec::new();
    let mut dlq_lines = Vec::new();
    if let Some(map) = health {
        let mut keys: Vec<String> = map.iter().map(|e| e.key().clone()).collect();
        keys.sort();
        for k in keys {
            if let Some(arc) = map.get(&k).map(|v| v.value().clone()) {
                let h = arc.read().await;
                let st = match h.state {
                    WorkerState::Connecting => 0,
                    WorkerState::Idle => 1,
                    WorkerState::Polling => 2,
                    WorkerState::Down => 3,
                };
                state_lines.push(format!(
                    "email_imap_state{{instance=\"{}\"}} {st}\n",
                    escape(&k)
                ));
                queue_lines.push(format!(
                    "email_outbound_queue_depth{{instance=\"{}\"}} {}\n",
                    escape(&k),
                    h.outbound_queue_depth
                ));
                dlq_lines.push(format!(
                    "email_outbound_dlq_depth{{instance=\"{}\"}} {}\n",
                    escape(&k),
                    h.outbound_dlq_depth
                ));
            }
        }
    }
    if state_lines.is_empty() {
        out.push_str("email_imap_state{instance=\"\"} 0\n");
        out.push_str("email_outbound_queue_depth{instance=\"\"} 0\n");
        out.push_str("email_outbound_dlq_depth{instance=\"\"} 0\n");
    } else {
        out.extend(state_lines);
        out.extend(queue_lines);
        out.extend(dlq_lines);
    }

    out
}

fn render_instance_counter(out: &mut String, map: &DashMap<InstanceKey, AtomicU64>, name: &str) {
    let mut rows: Vec<_> = map
        .iter()
        .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
        .collect();
    rows.sort_by(|a, b| a.0.instance.cmp(&b.0.instance));
    if rows.is_empty() {
        out.push_str(&format!("{name}{{instance=\"\"}} 0\n"));
    }
    for (key, v) in rows {
        out.push_str(&format!(
            "{name}{{instance=\"{}\"}} {v}\n",
            escape(&key.instance)
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn renders_with_no_data_emits_zero_lines() {
        let body = render_prometheus(None).await;
        for name in [
            "email_imap_messages_fetched_total",
            "email_loop_skipped_total",
            "email_bounces_total",
            "email_outbound_sent_total",
            "email_outbound_failed_total",
            "email_imap_state",
            "email_outbound_queue_depth",
            "email_outbound_dlq_depth",
        ] {
            assert!(body.contains(&format!("# TYPE {name}")), "missing TYPE for {name}");
        }
    }

    #[tokio::test]
    async fn counters_render_with_labels() {
        inc_messages_fetched("ops");
        inc_loop_skipped("auto_submitted");
        inc_bounce("ops", BounceClassification::Permanent);
        inc_outbound_sent("ops");
        inc_outbound_failed("ops");
        let body = render_prometheus(None).await;
        assert!(body.contains("email_imap_messages_fetched_total{instance=\"ops\"}"));
        assert!(body.contains("email_loop_skipped_total{reason=\"auto_submitted\"}"));
        assert!(body.contains(
            "email_bounces_total{classification=\"permanent\",instance=\"ops\"}"
        ));
        assert!(body.contains("email_outbound_sent_total{instance=\"ops\"}"));
        assert!(body.contains("email_outbound_failed_total{instance=\"ops\"}"));
    }
}
