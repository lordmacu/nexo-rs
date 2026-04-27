//! Phase 76.7 — server-side `notifications/progress` emission.
//!
//! `ProgressReporter` is the thin handle a tool gets when the
//! caller's request supplied a `params._meta.progressToken`. The
//! reporter is `Clone` (cheap; clones share the same inner via
//! `Arc<Option<...>>`) so a long-running tool can hand it to
//! sub-tasks. `report()` is non-blocking — it pushes a
//! `notifications/progress` JSON-RPC message into the session's
//! broadcast sender, drop-oldest on overflow.
//!
//! ## Coalescing
//!
//! A naive tool that calls `report()` once per loop iteration
//! would generate thousands of notifications per second. The
//! reporter applies a per-instance **min-interval gate**: when
//! a `report()` lands inside the gate window, the update is
//! stored as `pending` and a one-shot flusher task is scheduled
//! at the deadline. Subsequent reports within the window
//! overwrite the pending update — "last call wins". When the
//! deadline expires the flusher emits the most recent values.
//!
//! Setting `min_interval = Duration::ZERO` disables coalescing
//! entirely (every `report` emits immediately).
//!
//! ## Reference
//!
//! The leak is client-side and consumes `notifications/progress`
//! from upstream MCP servers
//! (`/home/familia/claude-code-leak/src/services/mcp/useManageMCPConnections.ts:618-664`
//! for the analogous `tools/list_changed` flow); it does NOT
//! implement server-side progress emission. We port the wire
//! shape (JSON-RPC notification with `params._meta.progressToken`
//! echoed from the originating request) and build the
//! coalescing + per-session sink ourselves on top of the in-tree
//! `broadcast::Sender<SessionEvent>` primitive
//! (Phase 76.1, `crates/mcp/src/server/http_session.rs:39-46`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::http_session::SessionEvent;

#[derive(Clone)]
pub struct ProgressReporter {
    inner: Option<Arc<ProgressReporterInner>>,
}

struct ProgressReporterInner {
    progress_token: Value,
    sink: broadcast::Sender<SessionEvent>,
    min_interval: Duration,
    state: Mutex<ReporterState>,
}

#[derive(Default)]
struct ReporterState {
    last_emit: Option<Instant>,
    pending: Option<PendingProgress>,
    flusher: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
struct PendingProgress {
    progress: f64,
    total: Option<f64>,
    message: Option<String>,
}

impl ProgressReporter {
    /// No-op reporter for tools that didn't receive a
    /// `progressToken`. All `report()` calls return immediately.
    pub fn noop() -> Self {
        Self { inner: None }
    }

    /// Build a live reporter. `min_interval` is the coalescing
    /// gate (default 20 ms in `HttpTransportConfig`); pass
    /// `Duration::ZERO` to disable coalescing.
    pub(crate) fn new(
        progress_token: Value,
        sink: broadcast::Sender<SessionEvent>,
        min_interval: Duration,
    ) -> Self {
        Self {
            inner: Some(Arc::new(ProgressReporterInner {
                progress_token,
                sink,
                min_interval,
                state: Mutex::new(ReporterState::default()),
            })),
        }
    }

    pub fn is_noop(&self) -> bool {
        self.inner.is_none()
    }

    /// Record a progress update. Non-blocking. Coalesces with
    /// nearby calls per the reporter's `min_interval` window.
    /// `progress` is the current count; `total` is the optional
    /// upper bound; `message` is an optional human-readable
    /// status line. The wire shape mirrors MCP 2025-11-25
    /// `notifications/progress`.
    pub fn report(&self, progress: f64, total: Option<f64>, message: Option<String>) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let now = Instant::now();
        let min = inner.min_interval;

        let should_emit_now = {
            let mut st = inner.state.lock();
            let recent = st
                .last_emit
                .map(|t| now.saturating_duration_since(t) < min)
                .unwrap_or(false);
            if recent {
                // Stash the latest values for the deferred flusher.
                st.pending = Some(PendingProgress {
                    progress,
                    total,
                    message: message.clone(),
                });
                if st.flusher.is_none() {
                    let inner_clone = Arc::clone(inner);
                    let last = st.last_emit.unwrap_or(now);
                    let deadline_in = min.saturating_sub(now.saturating_duration_since(last));
                    let h = tokio::spawn(async move {
                        tokio::time::sleep(deadline_in).await;
                        flush_pending(&inner_clone);
                    });
                    st.flusher = Some(h);
                }
                false
            } else {
                st.last_emit = Some(now);
                st.pending = None;
                if let Some(h) = st.flusher.take() {
                    h.abort();
                }
                true
            }
        };

        if should_emit_now {
            emit(inner, progress, total, message);
        }
    }
}

fn flush_pending(inner: &Arc<ProgressReporterInner>) {
    let snapshot = {
        let mut st = inner.state.lock();
        st.flusher = None;
        let p = st.pending.take();
        if p.is_some() {
            st.last_emit = Some(Instant::now());
        }
        p
    };
    if let Some(p) = snapshot {
        emit(inner, p.progress, p.total, p.message);
    }
}

fn emit(inner: &ProgressReporterInner, progress: f64, total: Option<f64>, message: Option<String>) {
    let mut params = serde_json::Map::new();
    params.insert("progressToken".into(), inner.progress_token.clone());
    params.insert("progress".into(), json!(progress));
    if let Some(t) = total {
        params.insert("total".into(), json!(t));
    }
    if let Some(m) = message {
        params.insert("message".into(), Value::String(m));
    }
    let body = json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": Value::Object(params),
    });
    // Drop-oldest on overflow per Tokio broadcast semantics; never
    // blocks the caller. Receiver count being zero (nobody
    // subscribed) is also fine — the broadcast just discards.
    let _ = inner.sink.send(SessionEvent::Message(body));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (
        broadcast::Sender<SessionEvent>,
        broadcast::Receiver<SessionEvent>,
    ) {
        let (tx, rx) = broadcast::channel(64);
        (tx, rx)
    }

    fn extract_progress(ev: SessionEvent) -> Option<(f64, Option<f64>, Option<String>, Value)> {
        let SessionEvent::Message(v) = ev else {
            return None;
        };
        let method = v.get("method")?.as_str()?;
        if method != "notifications/progress" {
            return None;
        }
        let params = v.get("params")?;
        let token = params.get("progressToken")?.clone();
        let progress = params.get("progress")?.as_f64()?;
        let total = params.get("total").and_then(|t| t.as_f64());
        let message = params
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from);
        Some((progress, total, message, token))
    }

    #[tokio::test]
    async fn noop_reporter_drops_silently() {
        let r = ProgressReporter::noop();
        assert!(r.is_noop());
        // Hammer it; must not panic, must not allocate the inner.
        for i in 0..1000 {
            r.report(i as f64, Some(1000.0), None);
        }
    }

    #[tokio::test]
    async fn report_emits_when_no_recent_history() {
        let (tx, mut rx) = pair();
        let r = ProgressReporter::new(json!("tok-1"), tx, Duration::from_millis(10));
        r.report(1.0, Some(10.0), Some("step 1".into()));
        let (p, t, m, tok) = extract_progress(rx.recv().await.unwrap()).unwrap();
        assert_eq!(p, 1.0);
        assert_eq!(t, Some(10.0));
        assert_eq!(m, Some("step 1".into()));
        assert_eq!(tok, json!("tok-1"));
    }

    #[tokio::test]
    async fn coalescing_within_window_buffers() {
        let (tx, mut rx) = pair();
        let r = ProgressReporter::new(json!("tok-1"), tx, Duration::from_millis(50));
        r.report(1.0, None, None); // emits
        r.report(2.0, None, None); // suppressed
        r.report(3.0, None, None); // suppressed
                                   // First message arrives now.
        let (p, _, _, _) = extract_progress(rx.recv().await.unwrap()).unwrap();
        assert_eq!(p, 1.0);
        // Within the window, no further messages.
        let try_recv = tokio::time::timeout(Duration::from_millis(20), rx.recv()).await;
        assert!(try_recv.is_err(), "no message expected within the gate");
    }

    #[tokio::test]
    async fn coalesce_pending_flushes_after_interval() {
        let (tx, mut rx) = pair();
        let r = ProgressReporter::new(json!("tok-1"), tx, Duration::from_millis(50));
        r.report(1.0, None, None); // emits
        r.report(2.0, None, None); // suppressed → pending
                                   // Drain first.
        let (p1, _, _, _) = extract_progress(rx.recv().await.unwrap()).unwrap();
        assert_eq!(p1, 1.0);
        // Wait for the flusher to fire.
        let next = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("flusher must fire within 200ms")
            .unwrap();
        let (p2, _, _, _) = extract_progress(next).unwrap();
        assert_eq!(p2, 2.0, "pending update flushed");
    }

    #[tokio::test]
    async fn last_call_wins_under_storm() {
        let (tx, mut rx) = pair();
        let r = ProgressReporter::new(json!("tok-1"), tx, Duration::from_millis(50));
        r.report(1.0, None, None);
        for i in 2..=100 {
            r.report(i as f64, None, None);
        }
        // First message: progress=1.
        let (p1, _, _, _) = extract_progress(rx.recv().await.unwrap()).unwrap();
        assert_eq!(p1, 1.0);
        // Flush after 50 ms: progress=100 (last value).
        let next = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("flush")
            .unwrap();
        let (p2, _, _, _) = extract_progress(next).unwrap();
        assert_eq!(p2, 100.0);
        // No further messages.
        let extra = tokio::time::timeout(Duration::from_millis(80), rx.recv()).await;
        assert!(extra.is_err());
    }

    #[tokio::test]
    async fn report_when_sink_closed_no_panic() {
        let (tx, rx) = pair();
        drop(rx); // no subscribers
        let r = ProgressReporter::new(json!("tok-1"), tx, Duration::from_millis(0));
        // No subscribers → broadcast::send returns Err — must not
        // panic.
        for i in 0..50 {
            r.report(i as f64, None, None);
        }
    }

    #[tokio::test]
    async fn zero_min_interval_disables_coalescing() {
        let (tx, mut rx) = pair();
        let r = ProgressReporter::new(json!("tok-1"), tx, Duration::ZERO);
        for i in 1..=5 {
            r.report(i as f64, None, None);
        }
        // All 5 should arrive (no coalescing).
        let mut seen: Vec<f64> = Vec::new();
        while seen.len() < 5 {
            let ev = tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .expect("recv")
                .unwrap();
            let (p, _, _, _) = extract_progress(ev).unwrap();
            seen.push(p);
        }
        assert_eq!(seen, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    }
}
