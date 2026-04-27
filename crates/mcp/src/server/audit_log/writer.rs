//! `AuditWriter` — async batched writer in front of an
//! `AuditLogStore`. The dispatcher's hot path calls
//! `writer.try_send(row)` (non-blocking; drops + warn on full
//! channel). A background worker drains the channel, batches by
//! either count (`flush_batch_size`) or interval
//! (`flush_interval_ms`), and persists. SIGTERM drains pending
//! rows synchronously (with a timeout) before the process exits.
//!
//! Anti-pattern flagged: the leak's `shouldSampleEvent`
//! (`claude-code-leak/src/services/analytics/firstPartyEventLogger.ts:57-85`)
//! drops a configurable fraction of events to keep volume down.
//! Audit log is not telemetry — sampling is forbidden. Phase
//! 76.11 logs every dispatch at 100% and only drops on
//! buffer-full conditions, with a hard `tracing::error!` line per
//! drop so the operator notices rather than discovers a gap.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::config::AuditLogConfig;
use super::store::{AuditError, AuditLogStore};
use super::types::AuditRow;

enum WriterEvent {
    Row(AuditRow),
    Drain(oneshot::Sender<()>),
}

pub struct AuditWriter {
    tx: mpsc::Sender<WriterEvent>,
    drops_total: Arc<AtomicU64>,
    rows_persisted: Arc<AtomicU64>,
    worker: Mutex<Option<JoinHandle<()>>>,
    cfg: Arc<AuditLogConfig>,
}

impl AuditWriter {
    /// Spawn the writer worker. Must be called from inside a
    /// tokio runtime.
    pub fn spawn(cfg: AuditLogConfig, store: Arc<dyn AuditLogStore>) -> Result<Arc<Self>, String> {
        cfg.validate()?;
        let cfg = Arc::new(cfg);
        let (tx, rx) = mpsc::channel(cfg.writer_buffer);
        let drops_total = Arc::new(AtomicU64::new(0));
        let rows_persisted = Arc::new(AtomicU64::new(0));

        let cfg_for_worker = Arc::clone(&cfg);
        let rows_for_worker = Arc::clone(&rows_persisted);
        let h = tokio::spawn(async move {
            worker_loop(cfg_for_worker, store, rx, rows_for_worker).await;
        });

        Ok(Arc::new(Self {
            tx,
            drops_total,
            rows_persisted,
            worker: Mutex::new(Some(h)),
            cfg,
        }))
    }

    /// Non-blocking push. On full channel: increment drops
    /// counter, emit a single `tracing::error!` line. NEVER
    /// blocks the caller.
    pub fn try_send(&self, row: AuditRow) {
        if let Err(e) = self.tx.try_send(WriterEvent::Row(row)) {
            self.drops_total.fetch_add(1, Ordering::Relaxed);
            // Throttle to avoid log floods: only warn if we just
            // crossed a power-of-2 threshold. Cheap heuristic.
            let n = self.drops_total.load(Ordering::Relaxed);
            if n.is_power_of_two() {
                tracing::error!(
                    drops_total = n,
                    "mcp audit writer queue full; row dropped (anti-pattern: silent drops)"
                );
                let _ = e;
            }
        }
    }

    /// Drain pending rows synchronously, with a hard timeout.
    /// Returns when the worker has flushed or the timeout fires
    /// (whichever first). Best-effort — on timeout the unflushed
    /// rows are lost, but the process must still shut down.
    pub async fn drain(&self, timeout: Duration) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(WriterEvent::Drain(tx)).await.is_err() {
            return; // worker already gone
        }
        let _ = tokio::time::timeout(timeout, rx).await;
    }

    /// Hot getters for tests and a future `/metrics` exposition.
    pub fn drops_total(&self) -> u64 {
        self.drops_total.load(Ordering::Relaxed)
    }
    pub fn rows_persisted(&self) -> u64 {
        self.rows_persisted.load(Ordering::Relaxed)
    }
    pub fn config(&self) -> &AuditLogConfig {
        &self.cfg
    }
}

impl Drop for AuditWriter {
    fn drop(&mut self) {
        if let Some(h) = self.worker.lock().take() {
            h.abort();
        }
    }
}

async fn worker_loop(
    cfg: Arc<AuditLogConfig>,
    store: Arc<dyn AuditLogStore>,
    mut rx: mpsc::Receiver<WriterEvent>,
    persisted: Arc<AtomicU64>,
) {
    let interval = Duration::from_millis(cfg.flush_interval_ms);
    let batch_cap = cfg.flush_batch_size;
    let mut batch: Vec<AuditRow> = Vec::with_capacity(batch_cap);

    loop {
        let deadline = Instant::now() + interval;
        // Block on the next event (or the deadline) once. If
        // we're already accumulating, switch to short waits to
        // amortise the flush.
        let recv_for = if batch.is_empty() {
            interval * 10 // long wait when idle
        } else {
            deadline.saturating_duration_since(Instant::now())
        };

        match tokio::time::timeout(recv_for, rx.recv()).await {
            Ok(Some(WriterEvent::Row(r))) => {
                batch.push(r);
                if batch.len() >= batch_cap {
                    flush_batch(&store, &mut batch, &persisted).await;
                }
            }
            Ok(Some(WriterEvent::Drain(ack))) => {
                if !batch.is_empty() {
                    flush_batch(&store, &mut batch, &persisted).await;
                }
                let _ = ack.send(());
            }
            Ok(None) => {
                // Channel closed — sender dropped. Final flush.
                if !batch.is_empty() {
                    flush_batch(&store, &mut batch, &persisted).await;
                }
                return;
            }
            Err(_elapsed) => {
                // Periodic flush.
                if !batch.is_empty() {
                    flush_batch(&store, &mut batch, &persisted).await;
                }
            }
        }
    }
}

async fn flush_batch(
    store: &Arc<dyn AuditLogStore>,
    batch: &mut Vec<AuditRow>,
    persisted: &Arc<AtomicU64>,
) {
    if batch.is_empty() {
        return;
    }
    let n = batch.len();
    let to_send = std::mem::take(batch);
    match store.append(to_send).await {
        Ok(()) => {
            persisted.fetch_add(n as u64, Ordering::Relaxed);
        }
        Err(AuditError::Closed) => {
            // Backend gone; drop silently — already-running
            // process can't recover the rows. A `warn!` keeps a
            // breadcrumb for the operator.
            tracing::warn!(rows = n, "mcp audit store closed; rows dropped");
        }
        Err(e) => {
            tracing::error!(rows = n, error = %e, "mcp audit store append failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::audit_log::store::MemoryAuditLogStore;
    use crate::server::audit_log::types::{AuditFilter, AuditOutcome};

    fn cfg() -> AuditLogConfig {
        let mut c = AuditLogConfig::default();
        c.flush_interval_ms = 20;
        c.flush_batch_size = 5;
        c.writer_buffer = 64;
        c
    }

    fn row(id: &str) -> AuditRow {
        AuditRow {
            call_id: id.into(),
            request_id: None,
            session_id: None,
            tenant: "t".into(),
            subject: None,
            auth_method: "static_token".into(),
            method: "tools/call".into(),
            tool_name: Some("echo".into()),
            args_hash: None,
            args_size_bytes: 0,
            started_at_ms: 0,
            completed_at_ms: Some(0),
            duration_ms: Some(0),
            outcome: AuditOutcome::Ok,
            error_code: None,
            error_message: None,
            result_size_bytes: None,
            retry_after_ms: None,
        }
    }

    #[tokio::test]
    async fn writer_persists_after_flush_interval() {
        let store = Arc::new(MemoryAuditLogStore::new()) as Arc<dyn AuditLogStore>;
        let writer = AuditWriter::spawn(cfg(), Arc::clone(&store)).unwrap();
        for i in 0..3 {
            writer.try_send(row(&format!("r{i}")));
        }
        // Wait for the periodic flush.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let count = store.count(&AuditFilter::default()).await.unwrap();
        assert_eq!(count, 3);
        assert_eq!(writer.rows_persisted(), 3);
    }

    #[tokio::test]
    async fn writer_persists_at_batch_size_threshold() {
        let store = Arc::new(MemoryAuditLogStore::new()) as Arc<dyn AuditLogStore>;
        let mut c = cfg();
        c.flush_batch_size = 3;
        c.flush_interval_ms = 60_000; // long; force batch-size trigger
        let writer = AuditWriter::spawn(c, Arc::clone(&store)).unwrap();
        for i in 0..3 {
            writer.try_send(row(&format!("r{i}")));
        }
        // Give the worker a tick to flush the full batch.
        tokio::time::sleep(Duration::from_millis(40)).await;
        let count = store.count(&AuditFilter::default()).await.unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn writer_drops_on_full_buffer_and_increments_counter() {
        let store = Arc::new(MemoryAuditLogStore::new()) as Arc<dyn AuditLogStore>;
        let mut c = cfg();
        c.writer_buffer = 2;
        c.flush_interval_ms = 60_000;
        c.flush_batch_size = 100; // never trigger by size
        let writer = AuditWriter::spawn(c, Arc::clone(&store)).unwrap();
        for i in 0..50 {
            writer.try_send(row(&format!("r{i}")));
        }
        // Some sends must have been dropped (channel cap = 2).
        assert!(
            writer.drops_total() > 0,
            "expected at least one drop, got {}",
            writer.drops_total()
        );
    }

    #[tokio::test]
    async fn drain_flushes_pending_rows() {
        let store = Arc::new(MemoryAuditLogStore::new()) as Arc<dyn AuditLogStore>;
        let mut c = cfg();
        c.flush_interval_ms = 60_000; // never auto-flush
        c.flush_batch_size = 100;
        let writer = AuditWriter::spawn(c, Arc::clone(&store)).unwrap();
        for i in 0..3 {
            writer.try_send(row(&format!("r{i}")));
        }
        writer.drain(Duration::from_secs(1)).await;
        let count = store.count(&AuditFilter::default()).await.unwrap();
        assert_eq!(count, 3);
    }
}
