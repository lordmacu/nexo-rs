//! Per-call audit log for the MCP HTTP/stdio dispatcher.
//! SQLite-backed durable trail of every `tools/call` (and other
//! dispatched method): who, what, when, outcome, duration, redacted
//! args hash. Survives daemon restart.
//!
//! ## Why per-call audit, separate from the per-turn log?
//!
//! `TurnLogStore` records one row per **agent turn** (one chat
//! completion + tool fan-out). This records one row per **MCP
//! call** (one tool invocation in our server). Both are append-only
//! SQLite tables with idempotent upserts; this is the structural
//! mirror of the turn log at one level of granularity finer.
//!
//! ## Design notes
//!
//! * In-tree precedent — `crates/agent-registry/src/turn_log.rs` is
//!   the structural template: SQLite WAL, idempotent INSERT OR
//!   REPLACE, async append, tail-N read API.
//! * Cardinality / PII discipline — args hashed (SHA-256) and
//!   sized; raw args NEVER go to the row.
//! * `Outcome` enum — re-exported from
//!   `crate::server::telemetry::Outcome` so dashboards and the
//!   audit trail speak identical labels.

pub mod config;
mod hash;
pub mod sqlite_store;
pub mod store;
pub mod types;
pub mod writer;

pub use config::AuditLogConfig;
pub(crate) use hash::compute_args_metrics;
pub use sqlite_store::SqliteAuditLogStore;
pub use store::{AuditError, AuditLogStore, MemoryAuditLogStore};
pub use types::{AuditFilter, AuditOutcome, AuditRow};
pub use writer::AuditWriter;
