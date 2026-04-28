//! Phase 79.6 step 2 stub. Filled in next commit.

#[derive(Debug, thiserror::Error)]
pub enum TeamStoreError {
    #[error("placeholder error")]
    Placeholder,
}

pub const TEAM_MAX_MEMBERS: usize = 8;
pub const TEAM_MAX_CONCURRENT_DEFAULT: usize = 4;
pub const TEAM_NAME_MAX_LEN: usize = 64;
pub const MEMBER_NAME_MAX_LEN: usize = 32;
pub const TEAM_IDLE_TIMEOUT_SECS: u64 = 3600;
pub const SHUTDOWN_DRAIN_SECS: u64 = 30;
pub const DM_BODY_MAX_BYTES: usize = 64 * 1024;
pub const TEAM_LEAD_NAME: &str = "team-lead";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TeamRow {
    pub team_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TeamMemberRow {
    pub team_id: String,
    pub name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TeamEventRow {
    pub event_id: String,
    pub team_id: String,
    pub kind: String,
}

pub fn sanitize_name(_s: &str) -> String {
    String::new()
}
pub fn validate_team_name(_s: &str) -> Result<String, TeamStoreError> {
    Ok(String::new())
}
pub fn validate_member_name(_s: &str) -> Result<String, TeamStoreError> {
    Ok(String::new())
}
