//! SQLite-backed store for the voice-mode toggle.
//!
//! One row per `conversation_key`. Defaults to disabled; absent
//! rows are treated as disabled by every reader so callers never
//! need to pre-seed conversations.
//!
//! The store rides on top of an existing `SqlitePool` — typically
//! the firehose pool from `crate::events::EventStore::pool()` —
//! so a single SQLite file holds every microapp-side persisted
//! bit. WAL mode lets readers proceed while the firehose listener
//! is appending.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::Result;

/// Default Microsoft Edge voice when the operator hasn't picked
/// one yet. Spanish-Mexico female; works well for LATAM operators
/// and matches the most common WhatsApp-bot use case. Microapps
/// that target other locales pick their own default in their
/// upsert helpers and keep this constant for migrations.
pub const DEFAULT_VOICE_ID: &str = "es-MX-DaliaNeural";

/// Persisted state for a single conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceModeRow {
    /// Application-defined conversation identifier
    /// (`<agent_id>:session:<session_id>` in the agent-creator
    /// microapp; any stable string works).
    pub conversation_key: String,
    /// Microsoft Edge voice id (e.g. `es-MX-DaliaNeural`).
    pub voice_id: String,
    /// `true` when outbound replies should be synthesized to audio.
    pub enabled: bool,
    /// Last write timestamp in epoch ms.
    pub updated_at_ms: i64,
}

/// Thin DAO wrapping a shared `SqlitePool`. Cloneable; clones
/// share the same pool.
#[derive(Debug, Clone)]
pub struct VoiceModeStore {
    pool: SqlitePool,
}

impl VoiceModeStore {
    /// Build the store on top of an existing pool. Idempotent
    /// DDL — safe to call on every boot. Pass the same pool the
    /// firehose owns so both stores share one SQLite file.
    pub async fn open(pool: SqlitePool) -> Result<Self> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS voice_mode (
                conversation_key TEXT PRIMARY KEY,
                voice_id         TEXT NOT NULL,
                enabled          INTEGER NOT NULL,
                updated_at_ms    INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    /// Read the current state. Returns `None` when no row exists
    /// — callers treat that as disabled.
    pub async fn get(&self, conversation_key: &str) -> Result<Option<VoiceModeRow>> {
        let row: Option<(String, i64, i64)> = sqlx::query_as(
            "SELECT voice_id, enabled, updated_at_ms FROM voice_mode WHERE conversation_key = ?1",
        )
        .bind(conversation_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(voice_id, enabled, updated_at_ms)| VoiceModeRow {
            conversation_key: conversation_key.to_string(),
            voice_id,
            enabled: enabled != 0,
            updated_at_ms,
        }))
    }

    /// Convenience: returns the row when one exists AND `enabled`
    /// is `true`. Used by the reply transformer hot path to
    /// short-circuit without materialising the full row.
    pub async fn get_active(&self, conversation_key: &str) -> Result<Option<VoiceModeRow>> {
        let row = self.get(conversation_key).await?;
        Ok(row.filter(|r| r.enabled))
    }

    /// Upsert. `voice_id == None` keeps any existing voice (or
    /// falls back to [`DEFAULT_VOICE_ID`] for fresh rows).
    pub async fn upsert(
        &self,
        conversation_key: &str,
        enabled: bool,
        voice_id: Option<&str>,
    ) -> Result<VoiceModeRow> {
        let existing = self.get(conversation_key).await?;
        let resolved_voice = voice_id
            .map(str::to_string)
            .or_else(|| existing.as_ref().map(|r| r.voice_id.clone()))
            .unwrap_or_else(|| DEFAULT_VOICE_ID.to_string());
        let now_ms = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO voice_mode (conversation_key, voice_id, enabled, updated_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(conversation_key) DO UPDATE SET
                    voice_id = excluded.voice_id,
                    enabled = excluded.enabled,
                    updated_at_ms = excluded.updated_at_ms",
        )
        .bind(conversation_key)
        .bind(&resolved_voice)
        .bind(if enabled { 1_i64 } else { 0_i64 })
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(VoiceModeRow {
            conversation_key: conversation_key.to_string(),
            voice_id: resolved_voice,
            enabled,
            updated_at_ms: now_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn fresh() -> VoiceModeStore {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        VoiceModeStore::open(pool).await.unwrap()
    }

    #[tokio::test]
    async fn get_returns_none_for_unknown_conversation() {
        let s = fresh().await;
        assert!(s.get("ana:session:zzz").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips() {
        let s = fresh().await;
        let row = s
            .upsert("ana:session:1", true, Some("es-CO-SalomeNeural"))
            .await
            .unwrap();
        assert!(row.enabled);
        assert_eq!(row.voice_id, "es-CO-SalomeNeural");
        let back = s.get("ana:session:1").await.unwrap().unwrap();
        assert_eq!(row, back);
    }

    #[tokio::test]
    async fn upsert_keeps_voice_when_none_passed() {
        let s = fresh().await;
        s.upsert("k", true, Some("es-CO-SalomeNeural"))
            .await
            .unwrap();
        let row = s.upsert("k", false, None).await.unwrap();
        assert!(!row.enabled);
        assert_eq!(row.voice_id, "es-CO-SalomeNeural");
    }

    #[tokio::test]
    async fn get_active_short_circuits_when_disabled() {
        let s = fresh().await;
        s.upsert("k", false, Some("v")).await.unwrap();
        assert!(s.get_active("k").await.unwrap().is_none());
        s.upsert("k", true, None).await.unwrap();
        assert!(s.get_active("k").await.unwrap().is_some());
    }
}
