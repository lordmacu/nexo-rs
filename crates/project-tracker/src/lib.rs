//! Phase 67.A — project tracker. Parses `PHASES.md` and `FOLLOWUPS.md`
//! out of the workspace root so the agent can answer "qué fase va el
//! desarrollo?" via Telegram/WhatsApp tools without humans having to
//! grep the repo.
//!
//! This crate is intentionally read-only — it never edits the
//! markdown. Edits keep going through the existing `/forge ejecutar`
//! commit flow.

pub mod config;
pub mod format;
pub mod git;
pub mod mutable;
pub mod parser;
pub mod tools;
pub mod tracker;
pub mod types;

pub use mutable::MutableTracker;

pub use config::{
    AgentRegistryConfig, GitLogConfig, ProgramPhaseConfig, ProjectTrackerConfig, TrackerConfig,
};

pub use git::{CommitRow, GitError, GitLogReader};

pub use parser::followups::{parse_file as parse_followups_file, parse_str as parse_followups_str};
pub use parser::phases::{parse_file as parse_phases_file, parse_str as parse_phases_str};
pub use tracker::{FsProjectTracker, ProjectTracker};

pub use types::{FollowUp, FollowUpStatus, Phase, PhaseStatus, SubPhase, TrackerError};
