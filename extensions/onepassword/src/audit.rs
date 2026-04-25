//! Append-only JSONL audit log of secret access. One line per event.
//! Path overridable via `OP_AUDIT_LOG_PATH` env (default
//! `./data/secrets-audit.jsonl`). Writes are best-effort: a write
//! failure is logged to stderr and the calling tool continues — the
//! secret was already read; blocking on log failure is worst-of-both
//! worlds.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

pub struct AuditEntry<'a> {
    pub action: &'a str,
    pub fields: Value,
}

pub fn audit_log_path() -> PathBuf {
    std::env::var("OP_AUDIT_LOG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data/secrets-audit.jsonl"))
}

fn agent_id() -> Option<String> {
    std::env::var("AGENT_ID").ok().filter(|s| !s.is_empty())
}

fn session_id() -> Option<String> {
    std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn append(entry: AuditEntry<'_>) {
    let mut record = json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "action": entry.action,
        "agent_id": agent_id(),
        "session_id": session_id(),
    });
    if let (Some(obj), Some(extra)) = (record.as_object_mut(), entry.fields.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    let line = match serde_json::to_string(&record) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("audit: serialize failed: {e}");
            return;
        }
    };
    let path = audit_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("audit: open `{}` failed: {e}", path.display());
            return;
        }
    };
    if let Err(e) = writeln!(file, "{line}") {
        eprintln!("audit: write to `{}` failed: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn read_lines(path: &std::path::Path) -> Vec<Value> {
        let raw = std::fs::read_to_string(path).unwrap();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    #[serial]
    fn appends_event_with_metadata() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        std::env::set_var("OP_AUDIT_LOG_PATH", &path);
        std::env::set_var("AGENT_ID", "kate");
        std::env::set_var("AGENT_SESSION_ID", "abc-123");
        append(AuditEntry {
            action: "read_secret",
            fields: json!({"op_reference": "op://P/I/f", "ok": true}),
        });
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["action"], "read_secret");
        assert_eq!(lines[0]["agent_id"], "kate");
        assert_eq!(lines[0]["session_id"], "abc-123");
        assert_eq!(lines[0]["op_reference"], "op://P/I/f");
        assert!(lines[0]["ts"].is_string());
        std::env::remove_var("OP_AUDIT_LOG_PATH");
        std::env::remove_var("AGENT_ID");
        std::env::remove_var("AGENT_SESSION_ID");
    }

    #[test]
    #[serial]
    fn missing_agent_context_yields_null_fields() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        std::env::set_var("OP_AUDIT_LOG_PATH", &path);
        std::env::remove_var("AGENT_ID");
        std::env::remove_var("AGENT_SESSION_ID");
        append(AuditEntry {
            action: "inject_template",
            fields: json!({"ok": true}),
        });
        let lines = read_lines(&path);
        assert_eq!(lines[0]["agent_id"], Value::Null);
        assert_eq!(lines[0]["session_id"], Value::Null);
        std::env::remove_var("OP_AUDIT_LOG_PATH");
    }

    #[test]
    #[serial]
    fn unwritable_path_does_not_panic() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("OP_AUDIT_LOG_PATH", "/proc/cannot/write/here.jsonl");
        append(AuditEntry {
            action: "read_secret",
            fields: json!({"ok": true}),
        });
        std::env::remove_var("OP_AUDIT_LOG_PATH");
    }
}
