//! Public types exposed by the tracker.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Status of a phase or sub-phase.
///
/// Sources understood by the parser: emoji suffixes (`✅` / `🔄` /
/// `⬜`) and GitHub-style checkboxes (`[x]` / `[ ]`). Strikethrough
/// titles wrapped in `~~ ~~` count as `Done` even without an explicit
/// emoji — that matches how FOLLOWUPS.md marks resolved items.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Done,
    InProgress,
    Pending,
}

impl PhaseStatus {
    /// True when the phase is shipped.
    pub fn is_done(self) -> bool {
        matches!(self, PhaseStatus::Done)
    }
}

/// One sub-phase entry, e.g. `67.9 — Compact opportunista (✅)`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubPhase {
    pub id: String,
    pub title: String,
    pub status: PhaseStatus,
    /// Optional body — paragraph(s) below the heading until the next
    /// heading. May be empty.
    pub body: Option<String>,
}

/// One top-level phase grouping (`## Phase 67`) with its sub-phases.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Phase {
    pub id: String,
    pub title: String,
    pub sub_phases: Vec<SubPhase>,
}

/// Resolved / open status for a follow-up item.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpStatus {
    Open,
    Resolved,
}

/// One follow-up entry under a section in FOLLOWUPS.md.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FollowUp {
    pub code: String,
    pub title: String,
    pub section: String,
    pub status: FollowUpStatus,
    pub body: String,
}

#[derive(Error, Debug)]
pub enum TrackerError {
    #[error("not tracked: {0:?} not found")]
    NotTracked(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("parse {file:?}: {msg}")]
    Parse { file: PathBuf, msg: String },
}
