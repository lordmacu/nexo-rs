//! Phase 67.A — project tracker. Parses `PHASES.md` and `FOLLOWUPS.md`
//! out of the workspace root so the agent can answer "qué fase va el
//! desarrollo?" via Telegram/WhatsApp tools without humans having to
//! grep the repo.
//!
//! This crate is intentionally read-only — it never edits the
//! markdown. Edits keep going through the existing `/forge ejecutar`
//! commit flow.

pub mod parser;
pub mod types;

pub use parser::phases::{parse_file as parse_phases_file, parse_str as parse_phases_str};

pub use types::{
    FollowUp, FollowUpStatus, Phase, PhaseStatus, SubPhase, TrackerError,
};
