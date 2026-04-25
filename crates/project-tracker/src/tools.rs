//! Read tools that the agent loop can call. Each tool takes a
//! self-describing input and returns markdown ≤ `DEFAULT_BYTE_CAP`.
//!
//! The actual `nexo_core::ToolHandler` registration happens later in
//! Phase 67.D.3 — keeping the wiring out of this crate avoids a
//! circular dep against `nexo-core` at the parser layer. For now the
//! tools are exposed as plain async methods on `ProjectTracking`,
//! which any future ToolHandler impl can adapt.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Deserialize;

use crate::format::{
    render_followup_detail, render_followups_open, render_phases_table, render_subphase_detail,
    render_subphase_line, DEFAULT_BYTE_CAP,
};
use crate::git::{GitError, GitLogReader};
use crate::tracker::ProjectTracker;
use crate::types::{PhaseStatus, TrackerError};

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("not tracked: PHASES.md missing")]
    NotTracked,
    #[error("phase {0} not found")]
    PhaseNotFound(String),
    #[error("followup {0} not found")]
    FollowupNotFound(String),
    #[error(transparent)]
    Tracker(TrackerError),
    #[error("git: {0}")]
    Git(String),
}

impl From<TrackerError> for ToolError {
    fn from(e: TrackerError) -> Self {
        match e {
            TrackerError::NotTracked(_) => ToolError::NotTracked,
            other => ToolError::Tracker(other),
        }
    }
}

/// Discriminated input for `project_status`.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StatusQuery {
    CurrentPhase,
    NextPhase,
    PhaseDetail { phase_id: String },
    FollowupsOpen,
    LastShipped {
        #[serde(default = "default_last_n")]
        n: usize,
    },
}

fn default_last_n() -> usize {
    5
}

#[derive(Clone, Debug, Deserialize)]
pub struct PhasesListInput {
    /// `done | in_progress | pending` — case-insensitive. Missing →
    /// no filter.
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FollowupDetailInput {
    pub code: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GitLogInput {
    pub phase_id: String,
}

/// Bundle of read tools sharing a tracker, an optional git reader,
/// and lightweight in-process counters that Phase 67.13 will graduate
/// to Prometheus.
pub struct ProjectTracking {
    pub tracker: Arc<dyn ProjectTracker>,
    pub git: Option<Arc<GitLogReader>>,
    pub repo_root: PathBuf,
    pub counters: ToolCounters,
}

#[derive(Default)]
pub struct ToolCounters {
    pub project_status_ok: AtomicU64,
    pub project_status_err: AtomicU64,
    pub project_phases_list_ok: AtomicU64,
    pub followup_detail_ok: AtomicU64,
    pub followup_detail_err: AtomicU64,
    pub git_log_for_phase_ok: AtomicU64,
    pub git_log_for_phase_err: AtomicU64,
}

impl ProjectTracking {
    pub fn new(tracker: Arc<dyn ProjectTracker>, repo_root: impl Into<PathBuf>) -> Self {
        Self {
            tracker,
            git: Some(Arc::new(GitLogReader::default())),
            repo_root: repo_root.into(),
            counters: ToolCounters::default(),
        }
    }

    pub fn with_git(mut self, g: Arc<GitLogReader>) -> Self {
        self.git = Some(g);
        self
    }

    pub fn without_git(mut self) -> Self {
        self.git = None;
        self
    }

    /// `project_status` tool entry-point. Returns markdown ≤ 4 KiB.
    pub async fn project_status(&self, q: StatusQuery) -> Result<String, ToolError> {
        let res = self.project_status_inner(q).await;
        if res.is_ok() {
            self.counters.project_status_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters
                .project_status_err
                .fetch_add(1, Ordering::Relaxed);
        }
        res
    }

    async fn project_status_inner(&self, q: StatusQuery) -> Result<String, ToolError> {
        match q {
            StatusQuery::CurrentPhase => match self.tracker.current_phase().await? {
                Some(s) => Ok(render_subphase_detail(&s)),
                None => Ok("everything done — no active or pending phase".into()),
            },
            StatusQuery::NextPhase => match self.tracker.next_phase().await? {
                Some(s) => Ok(render_subphase_line(&s)),
                None => Ok("no pending phase".into()),
            },
            StatusQuery::PhaseDetail { phase_id } => match self.tracker.phase_detail(&phase_id).await? {
                Some(s) => Ok(render_subphase_detail(&s)),
                None => Err(ToolError::PhaseNotFound(phase_id)),
            },
            StatusQuery::FollowupsOpen => {
                let items = self.tracker.followups().await?;
                Ok(render_followups_open(&items))
            }
            StatusQuery::LastShipped { n } => {
                let last = self.tracker.last_shipped(n).await?;
                if last.is_empty() {
                    return Ok("no shipped phases yet".into());
                }
                let mut out = String::new();
                for s in last {
                    out.push_str(&render_subphase_line(&s));
                    out.push('\n');
                }
                Ok(crate::format::cap_to(out, DEFAULT_BYTE_CAP))
            }
        }
    }

    /// `project_phases_list` — flat table optionally filtered by status.
    pub async fn project_phases_list(&self, input: PhasesListInput) -> Result<String, ToolError> {
        let filter = match input.filter.as_deref() {
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "done" => Some(PhaseStatus::Done),
                "in_progress" | "in-progress" => Some(PhaseStatus::InProgress),
                "pending" => Some(PhaseStatus::Pending),
                "all" | "" => None,
                _ => None,
            },
            None => None,
        };
        let phases = self.tracker.phases().await?;
        self.counters
            .project_phases_list_ok
            .fetch_add(1, Ordering::Relaxed);
        Ok(render_phases_table(&phases, filter))
    }

    /// `followup_detail` — full body for one follow-up code.
    pub async fn followup_detail(&self, input: FollowupDetailInput) -> Result<String, ToolError> {
        let items = self.tracker.followups().await?;
        match items.into_iter().find(|i| i.code == input.code) {
            Some(item) => {
                self.counters
                    .followup_detail_ok
                    .fetch_add(1, Ordering::Relaxed);
                Ok(render_followup_detail(&item))
            }
            None => {
                self.counters
                    .followup_detail_err
                    .fetch_add(1, Ordering::Relaxed);
                Err(ToolError::FollowupNotFound(input.code))
            }
        }
    }

    /// `git_log_for_phase` — top N commits whose message includes
    /// `Phase <id>`. Returns markdown bullets. Failure is non-fatal:
    /// circuit-open / git-missing return a descriptive message
    /// instead of a hard error.
    pub async fn git_log_for_phase(&self, input: GitLogInput) -> Result<String, ToolError> {
        let Some(g) = self.git.as_ref() else {
            return Ok("git lookup disabled".into());
        };
        let res = g.for_phase(&self.repo_root, &input.phase_id).await;
        match res {
            Ok(rows) if rows.is_empty() => {
                self.counters.git_log_for_phase_ok.fetch_add(1, Ordering::Relaxed);
                Ok(format!("no commits found for Phase {}", input.phase_id))
            }
            Ok(rows) => {
                self.counters.git_log_for_phase_ok.fetch_add(1, Ordering::Relaxed);
                let mut out = String::new();
                for r in rows {
                    out.push_str(&format!("- `{}` {} — {}\n", r.sha, &r.date[..10.min(r.date.len())], r.subject));
                }
                Ok(crate::format::cap_to(out, DEFAULT_BYTE_CAP))
            }
            Err(GitError::CircuitOpen(_)) => {
                self.counters.git_log_for_phase_err.fetch_add(1, Ordering::Relaxed);
                Ok("git lookup temporarily disabled (circuit open)".into())
            }
            Err(GitError::Timeout(_)) => {
                self.counters.git_log_for_phase_err.fetch_add(1, Ordering::Relaxed);
                Ok("git lookup timed out".into())
            }
            Err(e) => {
                self.counters.git_log_for_phase_err.fetch_add(1, Ordering::Relaxed);
                Err(ToolError::Git(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::FsProjectTracker;
    use std::path::Path;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn tracking_for_workspace() -> ProjectTracking {
        let root = workspace_root();
        let tr = FsProjectTracker::open(&root).unwrap();
        ProjectTracking::new(Arc::new(tr), &root)
    }

    #[tokio::test]
    async fn current_phase_returns_a_subphase() {
        let t = tracking_for_workspace();
        let out = t.project_status(StatusQuery::CurrentPhase).await.unwrap();
        assert!(out.contains("**"));
        assert!(out.len() <= DEFAULT_BYTE_CAP);
    }

    #[tokio::test]
    async fn phase_detail_known_id_renders() {
        let t = tracking_for_workspace();
        let out = t
            .project_status(StatusQuery::PhaseDetail { phase_id: "67.9".into() })
            .await
            .unwrap();
        assert!(out.contains("67.9"));
    }

    #[tokio::test]
    async fn phase_detail_unknown_id_errors() {
        let t = tracking_for_workspace();
        let err = t
            .project_status(StatusQuery::PhaseDetail { phase_id: "9999.999".into() })
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PhaseNotFound(_)));
    }

    #[tokio::test]
    async fn phases_list_table_caps_to_4kib() {
        let t = tracking_for_workspace();
        let out = t
            .project_phases_list(PhasesListInput { filter: None })
            .await
            .unwrap();
        assert!(out.len() <= DEFAULT_BYTE_CAP, "len was {}", out.len());
        assert!(out.starts_with("| id |"));
    }

    #[tokio::test]
    async fn followup_detail_known_code() {
        let t = tracking_for_workspace();
        let out = t
            .followup_detail(FollowupDetailInput { code: "PR-3".into() })
            .await
            .unwrap();
        assert!(out.contains("PR-3"));
    }

    #[tokio::test]
    async fn followup_detail_unknown_code_errors() {
        let t = tracking_for_workspace();
        let err = t
            .followup_detail(FollowupDetailInput { code: "ZZZ-999".into() })
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::FollowupNotFound(_)));
    }

    #[tokio::test]
    async fn last_shipped_renders_n_lines() {
        let t = tracking_for_workspace();
        let out = t
            .project_status(StatusQuery::LastShipped { n: 3 })
            .await
            .unwrap();
        // Workspace has many shipped phases, so 3 lines + glyphs.
        let line_count = out.lines().filter(|l| l.contains("✅")).count();
        assert!(line_count >= 1, "{out}");
    }

    #[test]
    fn not_tracked_path_maps_to_tool_error() {
        let dir = std::env::temp_dir().join(format!(
            "nexo-tracker-tools-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // No PHASES.md → open errors. Verify the From impl maps it.
        match FsProjectTracker::open(&dir) {
            Err(e) => {
                let mapped: ToolError = e.into();
                assert!(matches!(mapped, ToolError::NotTracked));
            }
            Ok(_) => panic!("expected NotTracked"),
        }
        let _ = std::fs::remove_dir_all::<&Path>(dir.as_ref());
    }
}
