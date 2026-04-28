//! Phase 76.8 — row types for the session event store.

use serde_json::Value;

/// One persisted SSE frame, keyed by `(session_id, seq)`.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub seq: u64,
    pub frame: Value,
    pub created_at_ms: i64,
}
