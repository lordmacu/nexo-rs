//! SQLite-backed team registry + audit log.
//!
//! Three tables (`teams`, `team_members`, `team_events`) with
//! idempotent CREATE. Pattern lift from `crates/agent-registry/src/
//! turn_log.rs` (Phase 72) — same shape (composite PK +
//! `INSERT … ON CONFLICT DO NOTHING` for idempotency on replays).
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/utils/swarm/teamHelpers.ts:131-176`
//!     — JSON file shape (sync + async readers + writers). We
//!     reject the JSON-file pattern (race-prone, no FK, no
//!     query-by-secondary-index) and store the same data in
//!     normalised SQL.

use crate::types::{TeamEventRow, TeamMemberRow, TeamRow, TeamStoreError, TEAM_MAX_MEMBERS};
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// Internal cap on `tail_events` to keep a runaway tool call from
/// pulling the whole log into memory.
const MAX_TAIL_EVENTS: usize = 200;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS teams (
    team_id              TEXT PRIMARY KEY,
    display_name         TEXT NOT NULL,
    description          TEXT,
    lead_agent_id        TEXT NOT NULL,
    lead_goal_id         TEXT NOT NULL,
    flow_id              TEXT NOT NULL,
    worktree_per_member  INTEGER NOT NULL,
    created_at           INTEGER NOT NULL,
    deleted_at           INTEGER,
    last_active_at       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_teams_lead_agent ON teams(lead_agent_id, deleted_at);

CREATE TABLE IF NOT EXISTS team_members (
    team_id        TEXT NOT NULL,
    name           TEXT NOT NULL,
    agent_id       TEXT NOT NULL,
    agent_type     TEXT,
    model          TEXT,
    goal_id        TEXT NOT NULL,
    worktree_path  TEXT,
    joined_at      INTEGER NOT NULL,
    is_active      INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL,
    PRIMARY KEY (team_id, name),
    FOREIGN KEY (team_id) REFERENCES teams(team_id)
);

CREATE TABLE IF NOT EXISTS team_events (
    event_id          TEXT PRIMARY KEY,
    team_id           TEXT NOT NULL,
    kind              TEXT NOT NULL,
    actor_member_name TEXT,
    payload_json      TEXT NOT NULL,
    created_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_team_events_team_created ON team_events(team_id, created_at DESC);
"#;

/// Behaviour the team-tools handlers depend on. Tests can swap
/// in a mock impl.
#[async_trait]
pub trait TeamStore: Send + Sync + 'static {
    async fn create_team(&self, team: &TeamRow) -> Result<(), TeamStoreError>;
    async fn soft_delete_team(&self, team_id: &str, now: i64) -> Result<(), TeamStoreError>;
    async fn get_team(&self, team_id: &str) -> Result<Option<TeamRow>, TeamStoreError>;
    async fn list_teams(
        &self,
        owner_agent: Option<&str>,
        active_only: bool,
    ) -> Result<Vec<TeamRow>, TeamStoreError>;
    async fn count_active_for_agent(&self, agent: &str) -> Result<usize, TeamStoreError>;
    async fn touch_team(&self, team_id: &str, now: i64) -> Result<(), TeamStoreError>;

    async fn add_member(&self, member: &TeamMemberRow) -> Result<(), TeamStoreError>;
    async fn list_members(&self, team_id: &str) -> Result<Vec<TeamMemberRow>, TeamStoreError>;
    async fn set_member_active(
        &self,
        team_id: &str,
        name: &str,
        active: bool,
        now: i64,
    ) -> Result<(), TeamStoreError>;
    async fn remove_member(&self, team_id: &str, name: &str) -> Result<(), TeamStoreError>;

    async fn record_event(&self, event: &TeamEventRow) -> Result<(), TeamStoreError>;
    async fn tail_events(
        &self,
        team_id: Option<&str>,
        n: usize,
    ) -> Result<Vec<TeamEventRow>, TeamStoreError>;
}

pub struct SqliteTeamStore {
    pool: SqlitePool,
}

impl SqliteTeamStore {
    pub async fn open(url: &str) -> Result<Self, TeamStoreError> {
        let opts = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA_SQL).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_in_memory() -> Result<Self, TeamStoreError> {
        Self::open("sqlite::memory:").await
    }
}

#[async_trait]
impl TeamStore for SqliteTeamStore {
    async fn create_team(&self, team: &TeamRow) -> Result<(), TeamStoreError> {
        let res = sqlx::query(
            r#"INSERT INTO teams
                (team_id, display_name, description, lead_agent_id, lead_goal_id,
                 flow_id, worktree_per_member, created_at, deleted_at, last_active_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&team.team_id)
        .bind(&team.display_name)
        .bind(&team.description)
        .bind(&team.lead_agent_id)
        .bind(&team.lead_goal_id)
        .bind(&team.flow_id)
        .bind(team.worktree_per_member as i64)
        .bind(team.created_at)
        .bind(team.deleted_at)
        .bind(team.last_active_at)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db)) if is_unique_violation(&*db) => {
                Err(TeamStoreError::TeamNameTaken(team.team_id.clone()))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn soft_delete_team(&self, team_id: &str, now: i64) -> Result<(), TeamStoreError> {
        let res = sqlx::query(
            r#"UPDATE teams SET deleted_at = ?
                WHERE team_id = ? AND deleted_at IS NULL"#,
        )
        .bind(now)
        .bind(team_id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            // Either missing or already deleted — caller treats both as
            // "nothing to do". Surface as TeamNotFound for the missing
            // case so the handler can distinguish.
            let exists = self.get_team(team_id).await?.is_some();
            if !exists {
                return Err(TeamStoreError::TeamNotFound(team_id.to_string()));
            }
        }
        Ok(())
    }

    async fn get_team(&self, team_id: &str) -> Result<Option<TeamRow>, TeamStoreError> {
        let row = sqlx::query_as::<_, TeamRow>(
            r#"SELECT team_id, display_name, description, lead_agent_id, lead_goal_id,
                      flow_id, worktree_per_member, created_at, deleted_at, last_active_at
                 FROM teams
                WHERE team_id = ?"#,
        )
        .bind(team_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn list_teams(
        &self,
        owner_agent: Option<&str>,
        active_only: bool,
    ) -> Result<Vec<TeamRow>, TeamStoreError> {
        // Build the filter dynamically; sqlx doesn't take Option in
        // the binding for "skip filter".
        let mut sql = String::from(
            r#"SELECT team_id, display_name, description, lead_agent_id, lead_goal_id,
                      flow_id, worktree_per_member, created_at, deleted_at, last_active_at
                 FROM teams WHERE 1 = 1"#,
        );
        if owner_agent.is_some() {
            sql.push_str(" AND lead_agent_id = ?");
        }
        if active_only {
            sql.push_str(" AND deleted_at IS NULL");
        }
        sql.push_str(" ORDER BY created_at DESC, team_id ASC");
        let mut q = sqlx::query_as::<_, TeamRow>(&sql);
        if let Some(a) = owner_agent {
            q = q.bind(a);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows)
    }

    async fn count_active_for_agent(&self, agent: &str) -> Result<usize, TeamStoreError> {
        let n: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM teams
                WHERE lead_agent_id = ? AND deleted_at IS NULL"#,
        )
        .bind(agent)
        .fetch_one(&self.pool)
        .await?;
        Ok(n as usize)
    }

    async fn touch_team(&self, team_id: &str, now: i64) -> Result<(), TeamStoreError> {
        sqlx::query(r#"UPDATE teams SET last_active_at = ? WHERE team_id = ?"#)
            .bind(now)
            .bind(team_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn add_member(&self, member: &TeamMemberRow) -> Result<(), TeamStoreError> {
        // Cap check first — keep the team-store enforcing the
        // documented invariant. Counts active members of the same
        // team excluding any deleted parent (FK guarantees the team
        // exists).
        let count: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*) FROM team_members WHERE team_id = ?"#)
                .bind(&member.team_id)
                .fetch_one(&self.pool)
                .await?;
        if (count as usize) >= TEAM_MAX_MEMBERS {
            return Err(TeamStoreError::TeamFull {
                team_id: member.team_id.clone(),
                count: count as usize,
                cap: TEAM_MAX_MEMBERS,
            });
        }
        let res = sqlx::query(
            r#"INSERT INTO team_members
                (team_id, name, agent_id, agent_type, model, goal_id,
                 worktree_path, joined_at, is_active, last_active_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&member.team_id)
        .bind(&member.name)
        .bind(&member.agent_id)
        .bind(&member.agent_type)
        .bind(&member.model)
        .bind(&member.goal_id)
        .bind(&member.worktree_path)
        .bind(member.joined_at)
        .bind(member.is_active as i64)
        .bind(member.last_active_at)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db)) if is_unique_violation(&*db) => {
                Err(TeamStoreError::MemberNameTaken {
                    team_id: member.team_id.clone(),
                    name: member.name.clone(),
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn list_members(&self, team_id: &str) -> Result<Vec<TeamMemberRow>, TeamStoreError> {
        let rows = sqlx::query_as::<_, TeamMemberRow>(
            r#"SELECT team_id, name, agent_id, agent_type, model, goal_id,
                      worktree_path, joined_at, is_active, last_active_at
                 FROM team_members
                WHERE team_id = ?
                ORDER BY joined_at ASC, name ASC"#,
        )
        .bind(team_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn set_member_active(
        &self,
        team_id: &str,
        name: &str,
        active: bool,
        now: i64,
    ) -> Result<(), TeamStoreError> {
        let res = sqlx::query(
            r#"UPDATE team_members SET is_active = ?, last_active_at = ?
                WHERE team_id = ? AND name = ?"#,
        )
        .bind(active as i64)
        .bind(now)
        .bind(team_id)
        .bind(name)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(TeamStoreError::MemberNotFound {
                team_id: team_id.to_string(),
                name: name.to_string(),
            });
        }
        Ok(())
    }

    async fn remove_member(&self, team_id: &str, name: &str) -> Result<(), TeamStoreError> {
        let res = sqlx::query(r#"DELETE FROM team_members WHERE team_id = ? AND name = ?"#)
            .bind(team_id)
            .bind(name)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(TeamStoreError::MemberNotFound {
                team_id: team_id.to_string(),
                name: name.to_string(),
            });
        }
        Ok(())
    }

    async fn record_event(&self, event: &TeamEventRow) -> Result<(), TeamStoreError> {
        sqlx::query(
            r#"INSERT INTO team_events
                (event_id, team_id, kind, actor_member_name, payload_json, created_at)
                VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(event_id) DO NOTHING"#,
        )
        .bind(&event.event_id)
        .bind(&event.team_id)
        .bind(&event.kind)
        .bind(&event.actor_member_name)
        .bind(&event.payload_json)
        .bind(event.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn tail_events(
        &self,
        team_id: Option<&str>,
        n: usize,
    ) -> Result<Vec<TeamEventRow>, TeamStoreError> {
        let limit = n.clamp(1, MAX_TAIL_EVENTS) as i64;
        let rows =
            match team_id {
                Some(tid) => sqlx::query_as::<_, TeamEventRow>(
                    r#"SELECT event_id, team_id, kind, actor_member_name, payload_json, created_at
                         FROM team_events WHERE team_id = ?
                        ORDER BY created_at DESC, rowid DESC
                        LIMIT ?"#,
                )
                .bind(tid)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?,
                None => sqlx::query_as::<_, TeamEventRow>(
                    r#"SELECT event_id, team_id, kind, actor_member_name, payload_json, created_at
                         FROM team_events
                        ORDER BY created_at DESC, rowid DESC
                        LIMIT ?"#,
                )
                .bind(limit)
                .fetch_all(&self.pool)
                .await?,
            };
        Ok(rows)
    }
}

fn is_unique_violation(db: &dyn sqlx::error::DatabaseError) -> bool {
    db.code().as_deref() == Some("2067")  // SQLITE_CONSTRAINT_UNIQUE
        || db.code().as_deref() == Some("1555") // SQLITE_CONSTRAINT_PRIMARYKEY
        || db.message().contains("UNIQUE")
}

// ---------------------------------------------------------------
// FromRow impls for `query_as`.
// ---------------------------------------------------------------

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for TeamRow {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        let worktree_per_member: i64 = row.try_get("worktree_per_member")?;
        Ok(Self {
            team_id: row.try_get("team_id")?,
            display_name: row.try_get("display_name")?,
            description: row.try_get("description")?,
            lead_agent_id: row.try_get("lead_agent_id")?,
            lead_goal_id: row.try_get("lead_goal_id")?,
            flow_id: row.try_get("flow_id")?,
            worktree_per_member: worktree_per_member != 0,
            created_at: row.try_get("created_at")?,
            deleted_at: row.try_get("deleted_at")?,
            last_active_at: row.try_get("last_active_at")?,
        })
    }
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for TeamMemberRow {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        let is_active: i64 = row.try_get("is_active")?;
        Ok(Self {
            team_id: row.try_get("team_id")?,
            name: row.try_get("name")?,
            agent_id: row.try_get("agent_id")?,
            agent_type: row.try_get("agent_type")?,
            model: row.try_get("model")?,
            goal_id: row.try_get("goal_id")?,
            worktree_path: row.try_get("worktree_path")?,
            joined_at: row.try_get("joined_at")?,
            is_active: is_active != 0,
            last_active_at: row.try_get("last_active_at")?,
        })
    }
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for TeamEventRow {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            event_id: row.try_get("event_id")?,
            team_id: row.try_get("team_id")?,
            kind: row.try_get("kind")?,
            actor_member_name: row.try_get("actor_member_name")?,
            payload_json: row.try_get("payload_json")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_team(team_id: &str, lead: &str, created_at: i64) -> TeamRow {
        TeamRow {
            team_id: team_id.into(),
            display_name: team_id.into(),
            description: None,
            lead_agent_id: lead.into(),
            lead_goal_id: format!("goal-{team_id}"),
            flow_id: team_id.into(),
            worktree_per_member: false,
            created_at,
            deleted_at: None,
            last_active_at: created_at,
        }
    }

    fn fixture_member(team: &str, name: &str, joined_at: i64) -> TeamMemberRow {
        TeamMemberRow {
            team_id: team.into(),
            name: name.into(),
            agent_id: format!("agent-{name}"),
            agent_type: Some("worker".into()),
            model: None,
            goal_id: format!("goal-{name}"),
            worktree_path: None,
            joined_at,
            is_active: true,
            last_active_at: joined_at,
        }
    }

    fn fixture_event(event_id: &str, team: &str, kind: &str, created_at: i64) -> TeamEventRow {
        TeamEventRow {
            event_id: event_id.into(),
            team_id: team.into(),
            kind: kind.into(),
            actor_member_name: None,
            payload_json: "{}".into(),
            created_at,
        }
    }

    #[tokio::test]
    async fn open_in_memory_creates_schema() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        // No teams yet.
        let teams = store.list_teams(None, false).await.unwrap();
        assert!(teams.is_empty());
        let n = store.count_active_for_agent("nobody").await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn create_team_then_get_returns_row() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("feature-x", "cody", 100))
            .await
            .unwrap();
        let got = store.get_team("feature-x").await.unwrap().unwrap();
        assert_eq!(got.lead_agent_id, "cody");
        assert_eq!(got.flow_id, "feature-x");
    }

    #[tokio::test]
    async fn create_team_idempotent_on_conflict_errors_with_team_name_taken() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("feature-x", "cody", 100))
            .await
            .unwrap();
        let err = store
            .create_team(&fixture_team("feature-x", "cody", 200))
            .await
            .unwrap_err();
        assert!(matches!(err, TeamStoreError::TeamNameTaken(t) if t == "feature-x"));
    }

    #[tokio::test]
    async fn soft_delete_team_excludes_from_active_listing() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store
            .create_team(&fixture_team("b", "cody", 200))
            .await
            .unwrap();
        store.soft_delete_team("a", 300).await.unwrap();
        let active = store.list_teams(Some("cody"), true).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].team_id, "b");
        let all = store.list_teams(Some("cody"), false).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn add_member_round_trip() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store
            .add_member(&fixture_member("a", "researcher", 110))
            .await
            .unwrap();
        store
            .add_member(&fixture_member("a", "tester", 120))
            .await
            .unwrap();
        let members = store.list_members("a").await.unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "researcher");
        assert_eq!(members[1].name, "tester");
    }

    #[tokio::test]
    async fn add_member_rejects_duplicate_name_in_team() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store
            .add_member(&fixture_member("a", "researcher", 110))
            .await
            .unwrap();
        let err = store
            .add_member(&fixture_member("a", "researcher", 120))
            .await
            .unwrap_err();
        match err {
            TeamStoreError::MemberNameTaken { team_id, name } => {
                assert_eq!(team_id, "a");
                assert_eq!(name, "researcher");
            }
            other => panic!("expected MemberNameTaken, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_member_rejects_when_at_team_full() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        for i in 0..TEAM_MAX_MEMBERS {
            store
                .add_member(&fixture_member("a", &format!("m{i}"), 100 + i as i64))
                .await
                .unwrap();
        }
        let err = store
            .add_member(&fixture_member("a", "overflow", 200))
            .await
            .unwrap_err();
        assert!(matches!(err, TeamStoreError::TeamFull { count, cap, .. }
            if count == TEAM_MAX_MEMBERS && cap == TEAM_MAX_MEMBERS));
    }

    #[tokio::test]
    async fn set_member_active_toggles_state_with_last_active_at() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store
            .add_member(&fixture_member("a", "x", 100))
            .await
            .unwrap();
        store.set_member_active("a", "x", false, 200).await.unwrap();
        let members = store.list_members("a").await.unwrap();
        assert!(!members[0].is_active);
        assert_eq!(members[0].last_active_at, 200);
    }

    #[tokio::test]
    async fn set_member_active_unknown_returns_member_not_found() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        let err = store
            .set_member_active("a", "ghost", false, 200)
            .await
            .unwrap_err();
        assert!(matches!(err, TeamStoreError::MemberNotFound { .. }));
    }

    #[tokio::test]
    async fn count_active_for_agent_filters_deleted() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store
            .create_team(&fixture_team("b", "cody", 110))
            .await
            .unwrap();
        store
            .create_team(&fixture_team("c", "alice", 120))
            .await
            .unwrap();
        store.soft_delete_team("a", 300).await.unwrap();

        let cody = store.count_active_for_agent("cody").await.unwrap();
        assert_eq!(cody, 1, "a soft-deleted, b active");
        let alice = store.count_active_for_agent("alice").await.unwrap();
        assert_eq!(alice, 1);
    }

    #[tokio::test]
    async fn record_event_then_tail_returns_in_desc_order() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store
            .record_event(&fixture_event("e1", "a", "team_created", 100))
            .await
            .unwrap();
        store
            .record_event(&fixture_event("e2", "a", "member_joined", 200))
            .await
            .unwrap();
        store
            .record_event(&fixture_event("e3", "a", "member_idled", 300))
            .await
            .unwrap();

        let tail = store.tail_events(Some("a"), 10).await.unwrap();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].event_id, "e3");
        assert_eq!(tail[1].event_id, "e2");
        assert_eq!(tail[2].event_id, "e1");
    }

    #[tokio::test]
    async fn record_event_idempotent_on_event_id() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store
            .record_event(&fixture_event("e1", "a", "team_created", 100))
            .await
            .unwrap();
        store
            .record_event(&fixture_event("e1", "a", "team_created", 999))
            .await
            .unwrap();
        let tail = store.tail_events(Some("a"), 10).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].created_at, 100, "first insert wins, no overwrite");
    }

    #[tokio::test]
    async fn tail_events_filters_by_team_id() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store
            .create_team(&fixture_team("b", "cody", 200))
            .await
            .unwrap();
        store
            .record_event(&fixture_event("e1", "a", "team_created", 100))
            .await
            .unwrap();
        store
            .record_event(&fixture_event("e2", "b", "team_created", 200))
            .await
            .unwrap();

        let only_a = store.tail_events(Some("a"), 10).await.unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].team_id, "a");

        let global = store.tail_events(None, 10).await.unwrap();
        assert_eq!(global.len(), 2);
    }

    #[tokio::test]
    async fn tail_events_caps_at_max() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        for i in 0..250 {
            store
                .record_event(&fixture_event(&format!("e{i:03}"), "a", "noop", i as i64))
                .await
                .unwrap();
        }
        let tail = store.tail_events(Some("a"), 1000).await.unwrap();
        assert_eq!(tail.len(), MAX_TAIL_EVENTS);
    }

    #[tokio::test]
    async fn touch_team_updates_last_active_at() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        store.touch_team("a", 999).await.unwrap();
        let got = store.get_team("a").await.unwrap().unwrap();
        assert_eq!(got.last_active_at, 999);
    }

    #[tokio::test]
    async fn remove_member_returns_member_not_found_for_unknown() {
        let store = SqliteTeamStore::open_in_memory().await.unwrap();
        store
            .create_team(&fixture_team("a", "cody", 100))
            .await
            .unwrap();
        let err = store.remove_member("a", "ghost").await.unwrap_err();
        assert!(matches!(err, TeamStoreError::MemberNotFound { .. }));
    }
}
