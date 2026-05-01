//! Phase 82.10.b — admin RPC audit log writer.
//!
//! Records every admin call with `(microapp_id, method,
//! capability, args_hash, started_at, result, duration_ms)`.
//! Operator audit pipelines parse these rows for SaaS billing /
//! compliance.
//!
//! Sub-phase scope:
//! - **82.10.b** (this module): in-memory writer behind a trait.
//!   Production builds use [`InMemoryAuditWriter`]. Tests inject
//!   their own implementations via [`AdminAuditWriter`].
//! - **82.10.g** (deferred): SQLite-backed writer + retention
//!   sweep + `nexo microapp admin audit tail` CLI.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// One audit row.
#[derive(Debug, Clone, PartialEq)]
pub struct AdminAuditRow {
    /// Microapp identity (extension id from `extensions.yaml`).
    pub microapp_id: String,
    /// Full JSON-RPC method (`nexo/admin/<domain>/<method>`).
    pub method: String,
    /// Required capability (e.g. `agents_crud`). When the call
    /// was denied, this is the capability that was missing.
    pub capability: String,
    /// SHA-256 of canonicalized params (sorted keys). Lets
    /// operators detect repeated identical calls without storing
    /// PII payloads.
    pub args_hash: String,
    /// Epoch milliseconds when dispatch started.
    pub started_at_ms: u64,
    /// `"ok"` | `"error"` | `"denied"`.
    pub result: AdminAuditResult,
    /// Wall-clock duration of the dispatch.
    pub duration_ms: u64,
}

/// Outcome of a single admin call.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAuditResult {
    /// Handler returned a `result` payload.
    Ok,
    /// Handler returned an error other than capability denial
    /// (validation failure, internal error, method-not-found).
    Error,
    /// Capability gate refused the call before dispatch.
    Denied,
}

impl AdminAuditResult {
    /// Stable wire string used by the SQLite writer (82.10.g) and
    /// the CLI tail formatter.
    pub fn as_str(self) -> &'static str {
        match self {
            AdminAuditResult::Ok => "ok",
            AdminAuditResult::Error => "error",
            AdminAuditResult::Denied => "denied",
        }
    }
}

/// Audit writer abstraction. Async to keep the SQLite future
/// (82.10.g) plug-compatible without changing this trait.
#[async_trait::async_trait]
pub trait AdminAuditWriter: Send + Sync + std::fmt::Debug {
    /// Append one row. Errors are logged but not propagated —
    /// audit failures must never block admin dispatch.
    async fn append(&self, row: AdminAuditRow);
}

/// In-memory writer used in tests + as the default production
/// writer until 82.10.g ships SQLite persistence.
#[derive(Debug, Default, Clone)]
pub struct InMemoryAuditWriter {
    rows: Arc<Mutex<Vec<AdminAuditRow>>>,
}

impl InMemoryAuditWriter {
    /// Build an empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of recorded rows (test-only inspection).
    pub fn rows(&self) -> Vec<AdminAuditRow> {
        self.rows.lock().unwrap().clone()
    }

    /// Last row appended, if any.
    pub fn last(&self) -> Option<AdminAuditRow> {
        self.rows.lock().unwrap().last().cloned()
    }
}

#[async_trait::async_trait]
impl AdminAuditWriter for InMemoryAuditWriter {
    async fn append(&self, row: AdminAuditRow) {
        self.rows.lock().unwrap().push(row);
    }
}

/// Hash JSON params canonically (sorted keys) so repeated calls
/// with semantically-identical payloads produce identical
/// hashes. Operator audit pipelines use this to detect
/// duplicate-call abuse without storing the params themselves.
pub fn hash_params(params: &Value) -> String {
    let canonical = canonicalize(params);
    let serialized = serde_json::to_string(&canonical).unwrap_or_default();
    let digest = Sha256::digest(serialized.as_bytes());
    hex_encode(&digest)
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Helper used by the dispatcher to stamp `started_at_ms`.
pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_writer_records_each_call() {
        let writer = InMemoryAuditWriter::new();
        let row = AdminAuditRow {
            microapp_id: "agent-creator".into(),
            method: "nexo/admin/agents/list".into(),
            capability: "agents_crud".into(),
            args_hash: "abc123".into(),
            started_at_ms: 1_700_000_000_000,
            result: AdminAuditResult::Ok,
            duration_ms: 5,
        };
        writer.append(row.clone()).await;
        assert_eq!(writer.rows().len(), 1);
        assert_eq!(writer.last().unwrap(), row);
    }

    #[tokio::test]
    async fn in_memory_writer_records_denial() {
        let writer = InMemoryAuditWriter::new();
        writer
            .append(AdminAuditRow {
                microapp_id: "agent-creator".into(),
                method: "nexo/admin/llm_providers/upsert".into(),
                capability: "llm_keys_crud".into(),
                args_hash: hash_params(&serde_json::json!({})),
                started_at_ms: now_epoch_ms(),
                result: AdminAuditResult::Denied,
                duration_ms: 0,
            })
            .await;
        let last = writer.last().unwrap();
        assert_eq!(last.result, AdminAuditResult::Denied);
        assert_eq!(last.capability, "llm_keys_crud");
    }

    #[test]
    fn hash_params_is_deterministic_with_key_order() {
        let a = serde_json::json!({ "z": 1, "a": 2 });
        let b = serde_json::json!({ "a": 2, "z": 1 });
        assert_eq!(hash_params(&a), hash_params(&b));
    }

    #[test]
    fn hash_params_differs_for_different_payloads() {
        let a = serde_json::json!({ "x": 1 });
        let b = serde_json::json!({ "x": 2 });
        assert_ne!(hash_params(&a), hash_params(&b));
    }

    #[test]
    fn audit_result_as_str_table() {
        assert_eq!(AdminAuditResult::Ok.as_str(), "ok");
        assert_eq!(AdminAuditResult::Error.as_str(), "error");
        assert_eq!(AdminAuditResult::Denied.as_str(), "denied");
    }
}
