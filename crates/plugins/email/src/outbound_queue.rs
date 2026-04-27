//! Append-only JSONL outbound queue per account (Phase 48.4).
//!
//! Persists `OutboundJob`s so a daemon restart resumes pending sends
//! and so transient SMTP failures can retry with exponential backoff
//! without losing the message. Two files per instance:
//!
//! - `<dir>/<instance>.jsonl` — pending + tombstones (`done=true`)
//! - `<dir>/<instance>.dlq.jsonl` — permanent failures (5xx) or
//!   attempts >= 5
//!
//! Single-writer assumption: one `OutboundDispatcher` per instance.
//! The internal `Mutex<File>` only serialises this writer's own
//! concurrent ticks (e.g. `enqueue` racing `update`); cross-process
//! locking is intentionally not provided — running two daemons against
//! the same `data_dir` is unsupported.
//!
//! Format: each line is JSON `OutboundJob`. `mark_done` / `update`
//! re-append a row with the new state; `compact_if_needed` rewrites the
//! file dropping tombstones once they're >50% of the rows. This trades
//! some growth for crash safety (no in-place edits).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmtpEnvelope {
    pub from: String,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundJob {
    pub message_id: String,
    pub instance: String,
    pub envelope: SmtpEnvelope,
    /// Raw RFC 5322 bytes already built by `mime_text::build_text_mime`.
    /// Persisted so a crash mid-flight reissues the exact same bytes
    /// (Message-ID dedupe relies on byte-stability).
    #[serde(with = "serde_bytes")]
    pub raw_mime: Vec<u8>,
    pub attempts: u32,
    pub next_attempt_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: i64,
    /// Tombstone marker. `compact_if_needed` rewrites the file
    /// dropping these once they outweigh live rows.
    #[serde(default)]
    pub done: bool,
}

/// Threshold at which compaction kicks in: the fraction of rows in the
/// queue file that are tombstones (`done=true`). Picked at 0.5 so a
/// queue oscillating around a steady-state size doesn't compact every
/// tick.
const COMPACT_DONE_RATIO: f64 = 0.5;

pub struct OutboundQueue {
    queue_path: PathBuf,
    dlq_path: PathBuf,
    /// Single-writer mutex. We don't keep the file handle open across
    /// awaits — the lock is held briefly per append while we
    /// `OpenOptions::append`.
    lock: Mutex<()>,
}

impl OutboundQueue {
    pub async fn open(dir: &Path, instance: &str) -> Result<Self> {
        tokio::fs::create_dir_all(dir).await.with_context(|| {
            format!("email/outbound: cannot create queue dir {}", dir.display())
        })?;
        let queue_path = dir.join(format!("{instance}.jsonl"));
        let dlq_path = dir.join(format!("{instance}.dlq.jsonl"));
        // Touch both files so list_pending / dlq_count never fail with
        // ENOENT on the first run.
        for p in [&queue_path, &dlq_path] {
            if !p.exists() {
                File::create(p).await.with_context(|| {
                    format!("email/outbound: cannot create {}", p.display())
                })?;
            }
        }
        Ok(Self {
            queue_path,
            dlq_path,
            lock: Mutex::new(()),
        })
    }

    pub async fn enqueue(&self, job: &OutboundJob) -> Result<()> {
        let _g = self.lock.lock().await;
        append_line(&self.queue_path, job).await
    }

    /// Re-append the row with the supplied state (typically bumped
    /// `attempts` / `next_attempt_at` / `last_error`). The previous
    /// row is left in place; readers fold by message_id taking the
    /// last write to win.
    pub async fn update(&self, job: &OutboundJob) -> Result<()> {
        let _g = self.lock.lock().await;
        append_line(&self.queue_path, job).await
    }

    pub async fn mark_done(&self, message_id: &str) -> Result<()> {
        let _g = self.lock.lock().await;
        // Find the most recent entry for `message_id` and re-append it
        // with `done=true`. Reading the whole file each call is fine —
        // queues are small (<1000) by design, and compaction keeps it
        // bounded.
        let mut latest = read_latest_per_id(&self.queue_path).await?;
        if let Some(j) = latest.remove(message_id) {
            let mut tomb = j;
            tomb.done = true;
            append_line(&self.queue_path, &tomb).await?;
        }
        Ok(())
    }

    pub async fn move_to_dlq(&self, job: &OutboundJob) -> Result<()> {
        let _g = self.lock.lock().await;
        // Audit #3 follow-up — write the queue tombstone *before*
        // the DLQ row. The two appends aren't cross-file atomic;
        // a SIGTERM between them leaves an inconsistency. Picking
        // the tombstone-first order means a hard kill mid-DLQ-
        // write yields "marked done in queue, missing from DLQ"
        // (mild visibility loss for that one row) rather than
        // "live in queue, also in DLQ" (duplicate SMTP sends on
        // restart, which is a correctness issue when the relay
        // doesn't dedupe by Message-ID). The Message-ID
        // idempotency we ship to RFC-conformant relays still
        // protects the recipient from a double delivery in the
        // duplicate-DLQ failure case, but that's a runtime
        // assumption we'd rather not depend on.
        let mut tomb = job.clone();
        tomb.done = true;
        append_line(&self.queue_path, &tomb).await?;
        append_line(&self.dlq_path, job).await?;
        Ok(())
    }

    /// Audit follow-up I — DLQ size cap. Trim the DLQ file to the
    /// most recent `max` lines when it grows past the limit.
    /// `max == 0` disables the cap. Returns the number of lines
    /// dropped (0 when no work was needed).
    pub async fn trim_dlq(&self, max: usize) -> Result<usize> {
        if max == 0 {
            return Ok(0);
        }
        let _g = self.lock.lock().await;
        let count = count_lines(&self.dlq_path).await?;
        if count <= max {
            return Ok(0);
        }
        // Read every line, keep only the tail.
        if !self.dlq_path.exists() {
            return Ok(0);
        }
        let f = File::open(&self.dlq_path).await?;
        let mut reader = BufReader::new(f).lines();
        let mut all: Vec<String> = Vec::new();
        while let Some(line) = reader.next_line().await? {
            if !line.trim().is_empty() {
                all.push(line);
            }
        }
        let drop_n = all.len().saturating_sub(max);
        let kept = &all[drop_n..];
        let tmp = self.dlq_path.with_extension("dlq.jsonl.compact");
        let mut out = File::create(&tmp).await?;
        for line in kept {
            out.write_all(line.as_bytes()).await?;
            out.write_all(b"\n").await?;
        }
        out.flush().await?;
        drop(out);
        tokio::fs::rename(&tmp, &self.dlq_path).await?;
        Ok(drop_n)
    }

    pub async fn list_pending(&self) -> Result<Vec<OutboundJob>> {
        let latest = read_latest_per_id(&self.queue_path).await?;
        let mut out: Vec<OutboundJob> = latest.into_values().filter(|j| !j.done).collect();
        out.sort_by_key(|j| j.created_at);
        Ok(out)
    }

    pub async fn pending_count(&self) -> Result<usize> {
        Ok(self.list_pending().await?.len())
    }

    pub async fn dlq_count(&self) -> Result<usize> {
        let lines = count_lines(&self.dlq_path).await?;
        Ok(lines)
    }

    /// Rewrite the queue file dropping `done=true` rows once they
    /// outweigh live rows. Returns `true` if compaction ran.
    pub async fn compact_if_needed(&self) -> Result<bool> {
        let _g = self.lock.lock().await;
        let latest = read_latest_per_id(&self.queue_path).await?;
        let total = latest.len();
        if total == 0 {
            return Ok(false);
        }
        let done = latest.values().filter(|j| j.done).count();
        let ratio = done as f64 / total as f64;
        if ratio < COMPACT_DONE_RATIO {
            return Ok(false);
        }
        let live: Vec<OutboundJob> = latest.into_values().filter(|j| !j.done).collect();
        let tmp = self.queue_path.with_extension("jsonl.compact");
        let mut f = File::create(&tmp).await.with_context(|| {
            format!("email/outbound: cannot create {}", tmp.display())
        })?;
        for j in &live {
            let mut s = serde_json::to_string(j)?;
            s.push('\n');
            f.write_all(s.as_bytes()).await?;
        }
        f.flush().await?;
        drop(f);
        tokio::fs::rename(&tmp, &self.queue_path).await?;
        Ok(true)
    }
}

async fn append_line(path: &Path, job: &OutboundJob) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .with_context(|| format!("email/outbound: open {}", path.display()))?;
    let mut s = serde_json::to_string(job)?;
    s.push('\n');
    f.write_all(s.as_bytes()).await?;
    f.flush().await?;
    Ok(())
}

async fn read_latest_per_id(path: &Path) -> Result<std::collections::HashMap<String, OutboundJob>> {
    let mut out: std::collections::HashMap<String, OutboundJob> = std::collections::HashMap::new();
    if !path.exists() {
        return Ok(out);
    }
    let f = File::open(path)
        .await
        .with_context(|| format!("email/outbound: open {}", path.display()))?;
    let mut reader = BufReader::new(f).lines();
    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<OutboundJob>(&line) {
            Ok(j) => {
                out.insert(j.message_id.clone(), j);
            }
            Err(e) => {
                tracing::warn!(
                    target: "plugin.email",
                    path = %path.display(),
                    error = %e,
                    "skipping malformed outbound queue line"
                );
            }
        }
    }
    Ok(out)
}

async fn count_lines(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let f = File::open(path).await?;
    let mut reader = BufReader::new(f).lines();
    let mut n = 0usize;
    while let Some(line) = reader.next_line().await? {
        if !line.trim().is_empty() {
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str) -> OutboundJob {
        OutboundJob {
            message_id: id.into(),
            instance: "ops".into(),
            envelope: SmtpEnvelope {
                from: "ops@x".into(),
                to: vec!["a@x".into()],
                cc: vec![],
                bcc: vec![],
            },
            raw_mime: b"hello".to_vec(),
            attempts: 0,
            next_attempt_at: 0,
            last_error: None,
            created_at: 0,
            done: false,
        }
    }

    async fn fresh() -> (tempfile::TempDir, OutboundQueue) {
        let dir = tempfile::tempdir().unwrap();
        let q = OutboundQueue::open(dir.path(), "ops").await.unwrap();
        (dir, q)
    }

    #[tokio::test]
    async fn pending_empty_on_new_queue() {
        let (_d, q) = fresh().await;
        assert!(q.list_pending().await.unwrap().is_empty());
        assert_eq!(q.pending_count().await.unwrap(), 0);
        assert_eq!(q.dlq_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn enqueue_then_pending_returns_one() {
        let (_d, q) = fresh().await;
        q.enqueue(&job("<a@x>")).await.unwrap();
        let p = q.list_pending().await.unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].message_id, "<a@x>");
    }

    #[tokio::test]
    async fn mark_done_clears_pending() {
        let (_d, q) = fresh().await;
        q.enqueue(&job("<a@x>")).await.unwrap();
        q.mark_done("<a@x>").await.unwrap();
        assert_eq!(q.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn update_keeps_latest_attempts() {
        let (_d, q) = fresh().await;
        q.enqueue(&job("<a@x>")).await.unwrap();
        let mut j = job("<a@x>");
        j.attempts = 3;
        j.last_error = Some("oops".into());
        q.update(&j).await.unwrap();
        let p = q.list_pending().await.unwrap();
        assert_eq!(p[0].attempts, 3);
        assert_eq!(p[0].last_error.as_deref(), Some("oops"));
    }

    #[tokio::test]
    async fn move_to_dlq_persists_and_clears_pending() {
        let (_d, q) = fresh().await;
        q.enqueue(&job("<a@x>")).await.unwrap();
        q.move_to_dlq(&job("<a@x>")).await.unwrap();
        assert_eq!(q.pending_count().await.unwrap(), 0);
        assert_eq!(q.dlq_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn move_to_dlq_writes_tombstone_before_dlq_row() {
        // Audit #3 follow-up — the queue tombstone is written
        // first so a SIGTERM mid-call leaves "marked done in
        // queue, missing from DLQ" rather than "live in queue,
        // also in DLQ". Verifying byte-for-byte order would
        // require crash injection; instead we verify the
        // resulting state is the tombstone-then-dlq-row layout
        // by reading the raw queue file.
        let (dir, q) = fresh().await;
        q.enqueue(&job("<a@x>")).await.unwrap();
        q.move_to_dlq(&job("<a@x>")).await.unwrap();
        let queue_body =
            std::fs::read_to_string(dir.path().join("ops.jsonl")).unwrap();
        // Two rows in the queue file: original enqueue, then tombstone.
        let lines: Vec<&str> = queue_body
            .lines()
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].contains("\"done\":true"));
        assert!(lines[1].contains("\"done\":true"));
    }

    #[tokio::test]
    async fn compact_runs_when_done_ratio_is_high() {
        let (_d, q) = fresh().await;
        for i in 0..3 {
            q.enqueue(&job(&format!("<{i}@x>"))).await.unwrap();
            q.mark_done(&format!("<{i}@x>")).await.unwrap();
        }
        // Three live IDs, all tombstoned → ratio 1.0 (>0.5).
        let did = q.compact_if_needed().await.unwrap();
        assert!(did);
        assert_eq!(q.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn compact_skips_when_done_ratio_is_low() {
        let (_d, q) = fresh().await;
        for i in 0..4 {
            q.enqueue(&job(&format!("<{i}@x>"))).await.unwrap();
        }
        q.mark_done("<0@x>").await.unwrap();
        let did = q.compact_if_needed().await.unwrap();
        assert!(!did);
        assert_eq!(q.pending_count().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let q = OutboundQueue::open(dir.path(), "ops").await.unwrap();
            q.enqueue(&job("<a@x>")).await.unwrap();
        }
        let q = OutboundQueue::open(dir.path(), "ops").await.unwrap();
        assert_eq!(q.pending_count().await.unwrap(), 1);
    }
}
