//! Process-wide tunnel lifecycle telemetry — Phase 92.10.
//!
//! Mirrors the lock-free Prometheus pattern set by
//! `nexo-memory::metrics` (Phase 86): `LazyLock<AtomicU64>` /
//! `LazyLock<DashMap>` storage, hand-rolled text rendering, no
//! `prometheus` crate dep, no metrics server here. Operators
//! stitch [`render_prometheus`] into the runtime's `/metrics`
//! aggregator alongside memory + LLM + web-search exposers.
//!
//! Per-tunnel counters (streams, bytes, reconnects) live inside
//! the supervisor and are read via [`TunnelHandle::metrics`] —
//! this module owns only the *lifecycle* counters (starts /
//! failures / shutdowns) that aggregate across every tunnel the
//! daemon ever brought up.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use dashmap::DashMap;

use crate::TunnelHandle;

static STARTS_TOTAL: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static STARTS_FAILED_TOTAL: LazyLock<DashMap<&'static str, AtomicU64>> =
    LazyLock::new(DashMap::new);
static SHUTDOWNS_TOTAL: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));

/// Cardinality cap on the `reason` label of
/// `tunnel_starts_failed_total` — bound the label space so a
/// misbehaving error path can't blow up Prometheus storage.
const FAILURE_REASON_CAP: usize = 16;

/// Record one successful `TunnelManager::start`.
pub fn record_start_success() {
    STARTS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Record one failed `TunnelManager::start`. `reason` is a short
/// stable tag (`"api"` / `"edge"` / `"dial"` / `"register"` / …);
/// unbounded callers collapse to `"other"` once the cardinality
/// cap is hit.
pub fn record_start_failure(reason: &'static str) {
    if !STARTS_FAILED_TOTAL.contains_key(reason)
        && STARTS_FAILED_TOTAL.len() >= FAILURE_REASON_CAP
    {
        STARTS_FAILED_TOTAL
            .entry("other")
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
        return;
    }
    STARTS_FAILED_TOTAL
        .entry(reason)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Record one graceful shutdown (called from
/// [`TunnelHandle::shutdown_with`] regardless of inner result).
pub fn record_shutdown() {
    SHUTDOWNS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Render the lifecycle counters as Prometheus text. Per-tunnel
/// supervisor counters (streams / bytes / reconnects) are not
/// included here — pass live [`TunnelHandle`] references to
/// [`render_prometheus_with_handles`] when a daemon wants those
/// surfaced too.
pub fn render_prometheus() -> String {
    render_prometheus_with_handles(&[])
}

/// Like [`render_prometheus`] but also emits a snapshot of every
/// supplied handle's supervisor counters (`tunnel_streams_total`,
/// `tunnel_bytes_in_total`, `tunnel_bytes_out_total`,
/// `tunnel_reconnects_total`, labelled by `tunnel_id`).
///
/// Callers gather the snapshots upstream (the snapshot itself is
/// `async` on [`TunnelHandle::metrics`]) and hand the resulting
/// `(tunnel_id, TunnelMetrics)` pairs in.
pub fn render_prometheus_with_handles(
    handles: &[(&str, crate::TunnelMetrics)],
) -> String {
    let mut out = String::new();

    out.push_str(
        "# HELP tunnel_starts_total Total successful TunnelManager::start calls.\n",
    );
    out.push_str("# TYPE tunnel_starts_total counter\n");
    out.push_str(&format!(
        "tunnel_starts_total {}\n",
        STARTS_TOTAL.load(Ordering::Relaxed)
    ));

    out.push_str(
        "# HELP tunnel_starts_failed_total Failed TunnelManager::start attempts by reason.\n",
    );
    out.push_str("# TYPE tunnel_starts_failed_total counter\n");
    for entry in STARTS_FAILED_TOTAL.iter() {
        out.push_str(&format!(
            "tunnel_starts_failed_total{{reason=\"{}\"}} {}\n",
            entry.key(),
            entry.value().load(Ordering::Relaxed)
        ));
    }

    out.push_str(
        "# HELP tunnel_shutdowns_total Total TunnelHandle graceful-shutdown invocations.\n",
    );
    out.push_str("# TYPE tunnel_shutdowns_total counter\n");
    out.push_str(&format!(
        "tunnel_shutdowns_total {}\n",
        SHUTDOWNS_TOTAL.load(Ordering::Relaxed)
    ));

    if !handles.is_empty() {
        out.push_str(
            "# HELP tunnel_streams_total Streams proxied by the supervisor (per tunnel).\n",
        );
        out.push_str("# TYPE tunnel_streams_total counter\n");
        for (id, m) in handles {
            out.push_str(&format!(
                "tunnel_streams_total{{tunnel_id=\"{id}\"}} {}\n",
                m.streams_total
            ));
        }

        out.push_str(
            "# HELP tunnel_bytes_in_total Bytes proxied edge→local (per tunnel).\n",
        );
        out.push_str("# TYPE tunnel_bytes_in_total counter\n");
        for (id, m) in handles {
            out.push_str(&format!(
                "tunnel_bytes_in_total{{tunnel_id=\"{id}\"}} {}\n",
                m.bytes_in
            ));
        }

        out.push_str(
            "# HELP tunnel_bytes_out_total Bytes proxied local→edge (per tunnel).\n",
        );
        out.push_str("# TYPE tunnel_bytes_out_total counter\n");
        for (id, m) in handles {
            out.push_str(&format!(
                "tunnel_bytes_out_total{{tunnel_id=\"{id}\"}} {}\n",
                m.bytes_out
            ));
        }

        out.push_str(
            "# HELP tunnel_reconnects_total Supervisor reconnect cycles (per tunnel).\n",
        );
        out.push_str("# TYPE tunnel_reconnects_total counter\n");
        for (id, m) in handles {
            out.push_str(&format!(
                "tunnel_reconnects_total{{tunnel_id=\"{id}\"}} {}\n",
                m.reconnects
            ));
        }
    }

    out
}

/// Async helper that snapshots a slice of [`TunnelHandle`]s and
/// renders them alongside the process-wide counters. Hides the
/// per-handle locking the caller would otherwise repeat.
pub async fn render_prometheus_for(handles: &[&TunnelHandle]) -> String {
    let mut snaps: Vec<(String, crate::TunnelMetrics)> = Vec::with_capacity(handles.len());
    for h in handles {
        if let Some(m) = h.metrics().await {
            snaps.push((h.tunnel_id.clone(), m));
        }
    }
    let view: Vec<(&str, crate::TunnelMetrics)> =
        snaps.iter().map(|(id, m)| (id.as_str(), m.clone())).collect();
    render_prometheus_with_handles(&view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_emits_lifecycle_help_lines() {
        let text = render_prometheus();
        assert!(text.contains("# TYPE tunnel_starts_total counter"));
        assert!(text.contains("# TYPE tunnel_shutdowns_total counter"));
    }

    #[test]
    fn record_failure_reason_is_bounded() {
        for i in 0..(FAILURE_REASON_CAP + 4) {
            let leaked: &'static str = Box::leak(format!("reason-{i}").into_boxed_str());
            record_start_failure(leaked);
        }
        let text = render_prometheus();
        assert!(text.contains("tunnel_starts_failed_total{reason=\"other\"}"));
    }
}
