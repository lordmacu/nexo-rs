//! Phase 67.B.1 — schema v1 -> v2 migration adds
//! `origin_channel_json` and `dispatcher_json` nullable columns.
//! Bindings written by older daemons must keep deserialising into
//! `SessionBinding { origin_channel: None, dispatcher: None }` after
//! the running process re-opens the database.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use nexo_driver_claude::{
    DispatcherIdentity, OriginChannel, SessionBinding, SessionBindingStore, SqliteBindingStore,
};
use nexo_driver_types::GoalId;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use uuid::Uuid;

fn tmp_db_path() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "nexo-binding-v1v2-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// Hand-builds a v1 schema row, then opens via the modern store and
/// asserts the migration adds the new columns and reads succeed.
#[tokio::test]
async fn v1_db_migrates_in_place_and_reads_keep_working() {
    let db = tmp_db_path();
    let path = db.to_string_lossy().into_owned();

    // Stage v1: original columns only, user_version = 1.
    {
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(opts).await.unwrap();
        sqlx::query(
            "CREATE TABLE claude_session_bindings (\
                goal_id              TEXT    PRIMARY KEY,\
                session_id           TEXT    NOT NULL,\
                model                TEXT,\
                workspace            TEXT,\
                schema_version       INTEGER NOT NULL DEFAULT 1,\
                last_session_invalid INTEGER NOT NULL DEFAULT 0,\
                created_at           INTEGER NOT NULL,\
                updated_at           INTEGER NOT NULL,\
                last_active_at       INTEGER NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let g = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO claude_session_bindings ( \
                goal_id, session_id, model, workspace, \
                schema_version, last_session_invalid, \
                created_at, updated_at, last_active_at \
             ) VALUES (?1, 'sess-legacy', NULL, NULL, 1, 0, ?2, ?2, ?2)",
        )
        .bind(&g)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("PRAGMA user_version = 1")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    // Open via the v2 store. Migration must run.
    let store = SqliteBindingStore::open(&path)
        .await
        .unwrap()
        .with_idle_ttl(Duration::ZERO);

    // The legacy row should still be readable; no origin/dispatcher.
    let active = store.list_active().await.unwrap();
    assert_eq!(active.len(), 1, "legacy row survived migration");
    assert_eq!(active[0].session_id, "sess-legacy");
    assert!(active[0].origin_channel.is_none());
    assert!(active[0].dispatcher.is_none());

    // New writes carry origin + dispatcher round-trip.
    let g = GoalId(Uuid::new_v4());
    let binding = SessionBinding::new(g, "sess-new", None, None)
        .with_origin(OriginChannel {
            plugin: "telegram".into(),
            instance: "family".into(),
            sender_id: "@cris".into(),
            correlation_id: Some("msg-7".into()),
        })
        .with_dispatcher(DispatcherIdentity {
            agent_id: "asistente".into(),
            sender_id: Some("@cris".into()),
            parent_goal_id: None,
            chain_depth: 0,
        });
    store.upsert(binding.clone()).await.unwrap();

    let read = store.get(g).await.unwrap().expect("just upserted");
    let origin = read.origin_channel.expect("origin persisted");
    assert_eq!(origin.plugin, "telegram");
    assert_eq!(origin.sender_id, "@cris");
    assert_eq!(origin.correlation_id.as_deref(), Some("msg-7"));
    let disp = read.dispatcher.expect("dispatcher persisted");
    assert_eq!(disp.agent_id, "asistente");
    assert_eq!(disp.chain_depth, 0);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn upsert_without_origin_does_not_clobber_existing_origin() {
    // Phase 67.B.1 — touch / mid-turn upserts must preserve origin.
    let db = tmp_db_path();
    let path = db.to_string_lossy().into_owned();
    let store = SqliteBindingStore::open(&path).await.unwrap();

    let g = GoalId(Uuid::new_v4());
    let initial = SessionBinding::new(g, "sess-1", None, None).with_origin(OriginChannel {
        plugin: "whatsapp".into(),
        instance: "main".into(),
        sender_id: "+57...".into(),
        correlation_id: None,
    });
    store.upsert(initial).await.unwrap();

    // Subsequent upsert from the loop without origin (the loop has
    // no chat context).
    let next = SessionBinding::new(g, "sess-2", Some("M".into()), None);
    store.upsert(next).await.unwrap();

    let read = store.get(g).await.unwrap().unwrap();
    assert_eq!(read.session_id, "sess-2");
    let origin = read.origin_channel.expect("origin must be preserved");
    assert_eq!(origin.plugin, "whatsapp");

    let _ = std::fs::remove_file(&db);
}
