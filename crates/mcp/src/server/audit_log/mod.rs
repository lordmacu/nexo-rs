//! Phase 76.11 — per-call audit log for the MCP HTTP/stdio
//! dispatcher. SQLite-backed durable trail of every `tools/call`
//! (and other dispatched method): who, what, when, outcome,
//! duration, redacted args hash. Survives daemon restart.
//!
//! ## Why per-call audit, separate from per-turn (Phase 72)?
//!
//! Phase 72 `TurnLogStore` records one row per **agent turn**
//! (one chat completion + tool fan-out). Phase 76.11 records one
//! row per **MCP call** (one tool invocation in our server).
//! Both are append-only SQLite tables with idempotent upserts;
//! 76.11 is the structural mirror of 72 at one level of
//! granularity finer.
//!
//! ## Reference patterns
//!
//! * **In-tree precedent** — `crates/agent-registry/src/turn_log.rs`
//!   (Phase 72) is the structural template: SQLite WAL,
//!   idempotent INSERT OR REPLACE, async append, tail-N read API.
//! * **Cardinality / PII discipline** — args hashed (SHA-256) and
//!   sized; raw args NEVER go to the row. Ported principle from
//!   `claude-code-leak/src/services/analytics/metadata.ts:70-77`
//!   (`sanitizeToolNameForAnalytics`) and the `_PROTO_*` field
//!   redaction concept in `src/services/analytics/index.ts:22-58`.
//! * **`Outcome` enum** — re-exported from
//!   `crate::server::telemetry::Outcome` so dashboards (76.10)
//!   and audit trail (76.11) speak identical labels.

pub mod config;
mod hash;
pub mod sqlite_store;
pub mod store;
pub mod types;
pub mod writer;

pub use config::AuditLogConfig;
pub(crate) use hash::{args_hash_truncated, compute_args_metrics};
pub use sqlite_store::SqliteAuditLogStore;
pub use store::{AuditError, AuditLogStore, MemoryAuditLogStore};
pub use types::{AuditFilter, AuditOutcome, AuditRow};
pub use writer::AuditWriter;
