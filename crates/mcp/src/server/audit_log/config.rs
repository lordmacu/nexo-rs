//! `AuditLogConfig` — runtime config for the per-call audit log.
//! YAML mirror lives in `crates/config/src/types/mcp_server.rs`
//! (Phase 76.11 step 8).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AuditLogConfig {
    /// Defaults to `true` so an operator who reaches Phase 76.11
    /// gets audit by default. Opt out with `enabled: false`.
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,
    /// Path to the SQLite file. Resolved relative to the process
    /// CWD when relative; recommend an absolute path in production.
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    /// Retention TTL in seconds. Default 90 days.
    #[serde(default = "default_retention_secs")]
    pub retention_secs: u64,
    /// `tokio::mpsc` channel capacity for the writer queue.
    /// Beyond this, `try_send` returns full → row dropped + a
    /// `tracing::error!` line + drop counter bumped.
    #[serde(default = "default_writer_buffer")]
    pub writer_buffer: usize,
    /// Worker batches every N ms. Smaller = less staleness on
    /// shutdown; larger = better fsync amortisation.
    #[serde(default = "default_flush_interval_ms")]
    pub flush_interval_ms: u64,
    /// Worker flushes when the in-memory batch reaches N rows
    /// (whichever comes first vs `flush_interval_ms`).
    #[serde(default = "default_flush_batch_size")]
    pub flush_batch_size: usize,
    /// Hash + size args by default (true). When false, args_hash
    /// stays None — useful for operator-side debugging when args
    /// are needed inline. Risky for compliance.
    #[serde(default = "default_redact_args")]
    pub redact_args: bool,
    /// Per-tool override. `key` is the tool name; `value` is
    /// "should redact?". Tools NOT in the map fall back to
    /// `redact_args`.
    #[serde(default)]
    pub per_tool_redact_args: BTreeMap<String, bool>,
    /// Hard cap on bytes hashed for `args_hash`. Beyond this,
    /// we record `args_size_bytes` only and skip the hash.
    /// 1 MiB default — captures reasonable args without burning
    /// CPU on multi-MB blobs.
    #[serde(default = "default_args_hash_max_bytes")]
    pub args_hash_max_bytes: usize,
}

fn default_audit_enabled() -> bool {
    true
}
fn default_db_path() -> PathBuf {
    PathBuf::from("data/mcp_audit.db")
}
fn default_retention_secs() -> u64 {
    90 * 86_400
}
fn default_writer_buffer() -> usize {
    4096
}
fn default_flush_interval_ms() -> u64 {
    50
}
fn default_flush_batch_size() -> usize {
    50
}
fn default_redact_args() -> bool {
    true
}
fn default_args_hash_max_bytes() -> usize {
    1024 * 1024
}

impl Default for AuditLogConfig {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            db_path: default_db_path(),
            retention_secs: default_retention_secs(),
            writer_buffer: default_writer_buffer(),
            flush_interval_ms: default_flush_interval_ms(),
            flush_batch_size: default_flush_batch_size(),
            redact_args: default_redact_args(),
            per_tool_redact_args: BTreeMap::new(),
            args_hash_max_bytes: default_args_hash_max_bytes(),
        }
    }
}

impl AuditLogConfig {
    /// Boot validation. Surfaces every misconfiguration as a
    /// hard error so the operator can't run with a silently-broken
    /// audit (e.g. `writer_buffer: 0` would block every dispatch
    /// or drop every row).
    pub fn validate(&self) -> Result<(), String> {
        if self.db_path.as_os_str().is_empty() {
            return Err("audit_log.db_path must not be empty".into());
        }
        if self.retention_secs == 0 {
            return Err("audit_log.retention_secs must be > 0".into());
        }
        if self.writer_buffer == 0 {
            return Err("audit_log.writer_buffer must be > 0".into());
        }
        if self.flush_interval_ms == 0 {
            return Err("audit_log.flush_interval_ms must be > 0".into());
        }
        if self.flush_batch_size == 0 {
            return Err("audit_log.flush_batch_size must be > 0".into());
        }
        // 0 disables hashing entirely (size-only). Above 16 MiB
        // is borderline-pathological; cap to a sane ceiling so a
        // typo can't burn the CPU.
        if self.args_hash_max_bytes > 16 * 1024 * 1024 {
            return Err(format!(
                "audit_log.args_hash_max_bytes ({}) exceeds 16 MiB ceiling",
                self.args_hash_max_bytes
            ));
        }
        Ok(())
    }

    /// Effective redaction policy for `tool`.
    pub fn redact_for(&self, tool: &str) -> bool {
        self.per_tool_redact_args
            .get(tool)
            .copied()
            .unwrap_or(self.redact_args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_validates() {
        assert!(AuditLogConfig::default().validate().is_ok());
    }

    #[test]
    fn rejects_empty_db_path() {
        let mut c = AuditLogConfig::default();
        c.db_path = PathBuf::new();
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_zero_retention() {
        let mut c = AuditLogConfig::default();
        c.retention_secs = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_zero_writer_buffer() {
        let mut c = AuditLogConfig::default();
        c.writer_buffer = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_zero_flush_interval_or_batch() {
        let mut c = AuditLogConfig::default();
        c.flush_interval_ms = 0;
        assert!(c.validate().is_err());
        let mut c = AuditLogConfig::default();
        c.flush_batch_size = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_excessive_args_hash_max() {
        let mut c = AuditLogConfig::default();
        c.args_hash_max_bytes = 32 * 1024 * 1024;
        assert!(c.validate().is_err());
    }

    #[test]
    fn redact_for_uses_per_tool_override() {
        let mut c = AuditLogConfig::default();
        c.per_tool_redact_args.insert("debug_dump".into(), false);
        assert!(!c.redact_for("debug_dump"));
        assert!(c.redact_for("anything_else"));
    }
}
