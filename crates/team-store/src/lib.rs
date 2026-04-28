//! Phase 79.6 — `nexo-team-store` crate.
//!
//! SQLite-backed registry + audit log for the five `Team*` tools
//! that ship in `nexo-core`. Three tables — `teams`,
//! `team_members`, `team_events` — accessed via the [`TeamStore`]
//! async trait. Production wiring uses [`SqliteTeamStore`]; tests
//! can swap a mock impl trivially.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/utils/swarm/teamHelpers.ts:65-176`
//!     — the leak's `TeamFile` shape (JSON file per team). We
//!     adapt the column set into normalised SQL because:
//!       - concurrent member writes don't race;
//!       - foreign keys + indexes are free;
//!       - the audit log gets a separate table for clean
//!         filtering, not a stream of out-of-tree analytics
//!         events (`tengu_team_*` from `TeamCreateTool.ts:214-221`).
//!   * `claude-code-leak/src/utils/swarm/teamHelpers.ts:100-102`
//!     — `sanitizeName` regex `[^a-zA-Z0-9]/g → '-'` mirrored.
//!
//! Reference (secondary):
//!   * OpenClaw `research/src/` — no equivalent. `grep -rln
//!     "TeamCreate\|spawnTeam\|swarm" research/src/` returns 0
//!     relevant matches.

pub mod store;
pub mod types;

pub use store::{SqliteTeamStore, TeamStore};
pub use types::{
    sanitize_name, validate_member_name, validate_member_name_for_lead, validate_team_name,
    TeamEventRow, TeamMemberRow, TeamRow, TeamStoreError, DM_BODY_MAX_BYTES,
    MEMBER_NAME_MAX_LEN, SHUTDOWN_DRAIN_SECS, TEAM_IDLE_TIMEOUT_SECS, TEAM_LEAD_NAME,
    TEAM_MAX_CONCURRENT_DEFAULT, TEAM_MAX_MEMBERS, TEAM_NAME_MAX_LEN,
};
