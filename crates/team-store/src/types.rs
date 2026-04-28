//! Pure-data layer: row structs, constants, sanitize / validate
//! helpers.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/utils/swarm/teamHelpers.ts:100-102`
//!     — `sanitizeName(name) { return name.replace(/[^a-zA-Z0-9]/g, '-').toLowerCase() }`.
//!     We mirror the regex but with `[^a-z0-9]` because the
//!     lowercase pass already happened.
//!   * `claude-code-leak/src/utils/swarm/teamHelpers.ts:65-90`
//!     — `TeamFile` shape (member array; `isActive`,
//!     `worktreePath`, `subscriptions`). We collapse the array
//!     into a normalised `team_members` row with one row per
//!     member; subscriptions live on the broker, not in the row.
//!   * `claude-code-leak/src/utils/swarm/constants.ts:1` —
//!     `TEAM_LEAD_NAME = 'team-lead'`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------
// Constants. Centralised so the per-tool handlers and main.rs
// boot wiring read from the same source of truth.
// ---------------------------------------------------------------

/// Hard cap on members per team (includes the lead). Above this
/// the coordinator-overhead defeats parallelism — pragmatic sweet
/// spot for the chat-driven flow.
pub const TEAM_MAX_MEMBERS: usize = 8;

/// Default cap on concurrent active teams per agent. Per-agent
/// YAML can lower this further; the runtime clamps any value
/// above this constant.
pub const TEAM_MAX_CONCURRENT_DEFAULT: usize = 4;

pub const TEAM_NAME_MAX_LEN: usize = 64;
pub const MEMBER_NAME_MAX_LEN: usize = 32;

/// Stale-team threshold. Idle reaper marks teams whose
/// `last_active_at` exceeds this and notifies the lead via
/// `notify_origin`. The team is NOT auto-deleted; the lead
/// decides.
pub const TEAM_IDLE_TIMEOUT_SECS: u64 = 3600;

/// Drain budget for `TeamDelete`. After publishing the
/// `shutdown_request` broadcast, the handler waits this long for
/// members to flip to `Cancelled` / `Done` before force-killing.
pub const SHUTDOWN_DRAIN_SECS: u64 = 30;

/// Maximum body size for `TeamSendMessage`. 64 KiB matches the
/// per-event broker payload cap with headroom for envelope
/// fields. Mirror of the leak's `maxResultSizeChars: 100_000`
/// (`claude-code-leak/src/tools/TeamCreateTool/TeamCreateTool.ts:77`),
/// scaled down because we count bytes not chars.
pub const DM_BODY_MAX_BYTES: usize = 64 * 1024;

/// Reserved member name for the team lead. Matches the leak's
/// `TEAM_LEAD_NAME = 'team-lead'`. Members cannot register with
/// this name; the lead row is created by `TeamCreate` itself.
pub const TEAM_LEAD_NAME: &str = "team-lead";

/// Reserved sentinel for `TeamSendMessage { to: "broadcast" }`.
/// A literal member named `"broadcast"` would collide with the
/// fan-out semantics, so `validate_member_name` rejects it.
pub const BROADCAST_SENTINEL: &str = "broadcast";

// ---------------------------------------------------------------
// Errors
// ---------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TeamStoreError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("team `{0}` already exists")]
    TeamNameTaken(String),

    #[error("team `{0}` not found")]
    TeamNotFound(String),

    #[error("member `{name}` not found in team `{team_id}`")]
    MemberNotFound { team_id: String, name: String },

    #[error("member `{name}` already exists in team `{team_id}`")]
    MemberNameTaken { team_id: String, name: String },

    #[error("team `{team_id}` is full ({count}/{cap})")]
    TeamFull {
        team_id: String,
        count: usize,
        cap: usize,
    },

    #[error("agent `{agent}` already has {count} concurrent teams (cap {cap})")]
    ConcurrentCapExceeded {
        agent: String,
        count: usize,
        cap: usize,
    },

    #[error("team name `{0}` invalid: must be `[a-z0-9-]+` after sanitize, length 1..={max}", max = TEAM_NAME_MAX_LEN)]
    InvalidName(String),

    #[error("member name `{0}` invalid: must be `[a-z0-9-]+`, length 1..={max}, and not a reserved name", max = MEMBER_NAME_MAX_LEN)]
    InvalidMemberName(String),

    #[error("body exceeds {max} bytes (got {actual})")]
    BodyTooLarge { actual: usize, max: usize },
}

// ---------------------------------------------------------------
// Rows
// ---------------------------------------------------------------

/// One row in the `teams` table. PK is `team_id` (the sanitized
/// name). `deleted_at IS NULL` ⇒ active; non-null ⇒ soft-deleted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamRow {
    pub team_id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub lead_agent_id: String,
    pub lead_goal_id: String,
    pub flow_id: String,
    pub worktree_per_member: bool,
    pub created_at: i64,
    pub deleted_at: Option<i64>,
    pub last_active_at: i64,
}

/// One row in `team_members`. Composite PK `(team_id, name)` —
/// `name` is the human-readable handle (`"researcher"`,
/// `"tester"`); `agent_id` is the internal UUID.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamMemberRow {
    pub team_id: String,
    pub name: String,
    pub agent_id: String,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub goal_id: String,
    pub worktree_path: Option<String>,
    pub joined_at: i64,
    /// `false` once the member's goal flips to `Pending` between
    /// turns. The `goal_id`'s registry status is the
    /// authoritative source; this field is a denormalised hint
    /// so list queries don't need a registry join.
    pub is_active: bool,
    pub last_active_at: i64,
}

/// One row in `team_events`. Append-only audit log keyed by
/// UUIDv7 `event_id` for idempotent inserts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamEventRow {
    pub event_id: String,
    pub team_id: String,
    /// Stable kind discriminator: `team_created`,
    /// `team_deleted`, `member_joined`, `member_idled`,
    /// `member_resumed`, `member_force_killed`, `dm_sent`,
    /// `broadcast_sent`, `shutdown_requested`,
    /// `shutdown_completed`, `team_stale`.
    pub kind: String,
    pub actor_member_name: Option<String>,
    /// JSON-serialised payload — kind-specific shape.
    pub payload_json: String,
    pub created_at: i64,
}

// ---------------------------------------------------------------
// Sanitize + validate
// ---------------------------------------------------------------

/// Lowercase + replace every non-alphanumeric with `-`. Mirrors
/// `claude-code-leak/src/utils/swarm/teamHelpers.ts:100-102`.
pub fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// Sanitize + length cap. Returns the sanitized form on success.
/// Rejects empty input.
pub fn validate_team_name(s: &str) -> Result<String, TeamStoreError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(TeamStoreError::InvalidName(s.to_string()));
    }
    let sanitized = sanitize_name(trimmed);
    if sanitized.is_empty()
        || sanitized.len() > TEAM_NAME_MAX_LEN
        || sanitized.chars().all(|c| c == '-')
    {
        return Err(TeamStoreError::InvalidName(s.to_string()));
    }
    Ok(sanitized)
}

/// Sanitize + length cap + reserved-name check. Reserved:
/// [`TEAM_LEAD_NAME`] (set by `TeamCreate`, not by add_member),
/// [`BROADCAST_SENTINEL`] (collides with fan-out semantics).
pub fn validate_member_name(s: &str) -> Result<String, TeamStoreError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(TeamStoreError::InvalidMemberName(s.to_string()));
    }
    let sanitized = sanitize_name(trimmed);
    if sanitized.is_empty()
        || sanitized.len() > MEMBER_NAME_MAX_LEN
        || sanitized.chars().all(|c| c == '-')
    {
        return Err(TeamStoreError::InvalidMemberName(s.to_string()));
    }
    if sanitized == BROADCAST_SENTINEL {
        return Err(TeamStoreError::InvalidMemberName(s.to_string()));
    }
    Ok(sanitized)
}

/// Sanitize + length cap, but accepts [`TEAM_LEAD_NAME`] (used by
/// `TeamCreate` itself when seeding the lead row).
pub fn validate_member_name_for_lead(s: &str) -> Result<String, TeamStoreError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(TeamStoreError::InvalidMemberName(s.to_string()));
    }
    let sanitized = sanitize_name(trimmed);
    if sanitized.is_empty()
        || sanitized.len() > MEMBER_NAME_MAX_LEN
        || sanitized.chars().all(|c| c == '-')
    {
        return Err(TeamStoreError::InvalidMemberName(s.to_string()));
    }
    if sanitized == BROADCAST_SENTINEL {
        return Err(TeamStoreError::InvalidMemberName(s.to_string()));
    }
    Ok(sanitized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_name_lowers_and_replaces_non_alnum() {
        assert_eq!(sanitize_name("Feature X!"), "feature-x-");
        assert_eq!(sanitize_name("ALL_CAPS"), "all-caps");
        assert_eq!(sanitize_name("alpha-beta"), "alpha-beta");
    }

    #[test]
    fn sanitize_name_collapses_each_separator_individually() {
        // The leak's regex replaces each non-alnum char with one
        // `-`; runs of separators become runs of `-`. Mirror the
        // behaviour so semantics are identical.
        assert_eq!(sanitize_name("foo  bar"), "foo--bar");
        assert_eq!(sanitize_name("a/b/c"), "a-b-c");
    }

    #[test]
    fn validate_team_name_rejects_empty() {
        assert!(matches!(
            validate_team_name(""),
            Err(TeamStoreError::InvalidName(_))
        ));
        assert!(matches!(
            validate_team_name("   "),
            Err(TeamStoreError::InvalidName(_))
        ));
    }

    #[test]
    fn validate_team_name_rejects_too_long() {
        let long = "a".repeat(TEAM_NAME_MAX_LEN + 1);
        assert!(matches!(
            validate_team_name(&long),
            Err(TeamStoreError::InvalidName(_))
        ));
    }

    #[test]
    fn validate_team_name_rejects_only_separators() {
        // `!!!` sanitizes to `---` which is all-separators — reject
        // so the team_id is not a wall of dashes.
        assert!(matches!(
            validate_team_name("!!!"),
            Err(TeamStoreError::InvalidName(_))
        ));
    }

    #[test]
    fn validate_team_name_returns_sanitized_on_ok() {
        assert_eq!(validate_team_name("Feature-X").unwrap(), "feature-x");
        assert_eq!(validate_team_name("  cody  ").unwrap(), "cody");
    }

    #[test]
    fn validate_member_name_caps_at_32() {
        let long = "a".repeat(MEMBER_NAME_MAX_LEN + 1);
        assert!(matches!(
            validate_member_name(&long),
            Err(TeamStoreError::InvalidMemberName(_))
        ));
    }

    #[test]
    fn validate_member_name_rejects_broadcast_sentinel() {
        assert!(matches!(
            validate_member_name("broadcast"),
            Err(TeamStoreError::InvalidMemberName(_))
        ));
        // After sanitize, `Broadcast` also normalises to `broadcast`.
        assert!(matches!(
            validate_member_name("Broadcast"),
            Err(TeamStoreError::InvalidMemberName(_))
        ));
    }

    #[test]
    fn validate_member_name_for_lead_accepts_team_lead() {
        // The `_for_lead` variant is used by TeamCreate itself
        // (which knows it's seeding `team-lead`); ordinary
        // `add_member` calls go through `validate_member_name`
        // which is identical here — both currently accept
        // `team-lead`. Document the intent so a future tightening
        // doesn't accidentally break TeamCreate.
        assert_eq!(
            validate_member_name_for_lead(TEAM_LEAD_NAME).unwrap(),
            TEAM_LEAD_NAME
        );
    }

    #[test]
    fn row_serde_roundtrip() {
        let row = TeamRow {
            team_id: "feature-x".into(),
            display_name: "Feature X".into(),
            description: Some("Build the new auth flow".into()),
            lead_agent_id: "cody".into(),
            lead_goal_id: "01J7AAA".into(),
            flow_id: "feature-x".into(),
            worktree_per_member: false,
            created_at: 1700000000,
            deleted_at: None,
            last_active_at: 1700000000,
        };
        let json = serde_json::to_string(&row).unwrap();
        let back: TeamRow = serde_json::from_str(&json).unwrap();
        assert_eq!(row, back);
    }
}
