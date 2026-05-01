//! Phase 82.10.h.b.4 — integration test for
//! `nexo microapp admin audit tail`.
//!
//! Seeds a real SQLite admin audit DB via the production
//! `SqliteAdminAuditWriter`, spawns the `nexo` binary with
//! `microapp admin audit tail --db <fixture>`, and asserts the
//! formatted table output contains every seeded row.

use std::process::Command;
use std::sync::Arc;

use nexo_core::agent::admin_rpc::{
    AdminAuditResult, AdminAuditRow, AdminAuditWriter, SqliteAdminAuditWriter,
};

fn nexo_binary() -> std::path::PathBuf {
    // Cargo sets CARGO_BIN_EXE_<name> for binary integration tests.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nexo"))
}

#[tokio::test]
async fn audit_tail_renders_seeded_rows_in_table_format() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin_audit.db");
    let writer = Arc::new(SqliteAdminAuditWriter::open(&db).await.unwrap());

    let seed = vec![
        AdminAuditRow {
            microapp_id: "agent-creator".into(),
            method: "nexo/admin/agents/list".into(),
            capability: "agents_crud".into(),
            args_hash: "abcdef0123456789".into(),
            started_at_ms: 1_700_000_000_000,
            result: AdminAuditResult::Ok,
            duration_ms: 5,
        },
        AdminAuditRow {
            microapp_id: "agent-creator".into(),
            method: "nexo/admin/agents/upsert".into(),
            capability: "agents_crud".into(),
            args_hash: "fedcba9876543210".into(),
            started_at_ms: 1_700_000_010_000,
            result: AdminAuditResult::Denied,
            duration_ms: 2,
        },
    ];
    for row in &seed {
        writer.append(row.clone()).await;
    }
    drop(writer); // flush + close pool

    let output = Command::new(nexo_binary())
        .args([
            "microapp",
            "admin",
            "audit",
            "tail",
            "--db",
            db.to_str().unwrap(),
            "--limit",
            "10",
        ])
        .output()
        .expect("spawn nexo binary");
    assert!(
        output.status.success(),
        "exit status {:?}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("started_at"), "header present: {stdout}");
    assert!(stdout.contains("agent-creator"));
    assert!(stdout.contains("nexo/admin/agents/list"));
    assert!(stdout.contains("nexo/admin/agents/upsert"));
    assert!(stdout.contains("denied"));
    assert!(stdout.contains("ok"));
    assert!(stdout.contains("abcdef01"), "hash truncated to 8 chars");
}

#[tokio::test]
async fn audit_tail_filters_by_result_and_emits_json() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin_audit.db");
    let writer = Arc::new(SqliteAdminAuditWriter::open(&db).await.unwrap());
    writer
        .append(AdminAuditRow {
            microapp_id: "a".into(),
            method: "nexo/admin/agents/list".into(),
            capability: "agents_crud".into(),
            args_hash: "h1".into(),
            started_at_ms: 1_700_000_000_000,
            result: AdminAuditResult::Ok,
            duration_ms: 1,
        })
        .await;
    writer
        .append(AdminAuditRow {
            microapp_id: "a".into(),
            method: "nexo/admin/agents/upsert".into(),
            capability: "agents_crud".into(),
            args_hash: "h2".into(),
            started_at_ms: 1_700_000_010_000,
            result: AdminAuditResult::Denied,
            duration_ms: 2,
        })
        .await;
    drop(writer);

    let output = Command::new(nexo_binary())
        .args([
            "microapp",
            "admin",
            "audit",
            "tail",
            "--db",
            db.to_str().unwrap(),
            "--result",
            "denied",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn nexo binary");
    assert!(output.status.success(), "stderr={}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let arr = parsed.as_array().expect("json array");
    assert_eq!(arr.len(), 1, "only the denied row matches the filter");
    assert_eq!(arr[0]["result"], "denied");
    assert_eq!(arr[0]["method"], "nexo/admin/agents/upsert");
}

#[tokio::test]
async fn audit_tail_rejects_invalid_result_filter() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin_audit.db");
    let _writer = Arc::new(SqliteAdminAuditWriter::open(&db).await.unwrap());
    let output = Command::new(nexo_binary())
        .args([
            "microapp",
            "admin",
            "audit",
            "tail",
            "--db",
            db.to_str().unwrap(),
            "--result",
            "WRONG",
        ])
        .output()
        .expect("spawn nexo binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--result must be one of ok|error|denied"), "stderr: {stderr}");
}
