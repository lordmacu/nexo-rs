//! Phase 79.6 step 3 stub. Filled in commit 3.

use crate::types::{TeamEventRow, TeamMemberRow, TeamRow, TeamStoreError};
use async_trait::async_trait;

#[async_trait]
pub trait TeamStore: Send + Sync + 'static {
    async fn ping(&self) -> Result<(), TeamStoreError>;
}

pub struct SqliteTeamStore;

impl SqliteTeamStore {
    pub async fn open_in_memory() -> Result<Self, TeamStoreError> {
        Ok(Self)
    }
}

#[async_trait]
impl TeamStore for SqliteTeamStore {
    async fn ping(&self) -> Result<(), TeamStoreError> {
        Ok(())
    }
}

// Touch unused-imports so the stub compiles without warnings.
#[allow(dead_code)]
fn _touch(_t: TeamRow, _m: TeamMemberRow, _e: TeamEventRow) {}
