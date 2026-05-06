//! Generic event firehose scaffolding for microapps.
//!
//! Lifted from `agent-creator-microapp::firehose` so any microapp
//! that wants the same shape — typed daemon notifications fanned
//! out over SSE while persisted to a SQLite append-log for
//! backfill — can drop a few lines into `main.rs` instead of
//! rewriting ~900 LOC of store + listener + sweep.
//!
//! The four pieces, gated behind the `events` feature:
//!
//! - [`EventMetadata`] — projections (`kind`, `agent_id`,
//!   `tenant_id`, `at_ms`) the store needs to index any event
//!   type. Implemented out of the box for
//!   `nexo_tool_meta::admin::agent_events::AgentEventKind`.
//! - [`EventStore`] — SQLite append + filtered list + two-phase
//!   retention sweep. Generic over `T: EventMetadata + Serialize
//!   + DeserializeOwned`.
//! - [`EventBroadcastState`] — store + `tokio::broadcast::Sender`
//!   pair. SSE handlers `subscribe()` per connection.
//! - [`build_persisting_listener`] — the
//!   `Arc<dyn Fn(serde_json::Value)>` you hand to
//!   `Microapp::with_notification_listener`. Decodes, broadcasts,
//!   and asynchronously appends to the store.
//! - [`SweepConfig`] + [`spawn_sweep_loop`] — background retention
//!   task with operator-driven cancellation.
//!
//! HTTP routes (backfill `GET …` + SSE stream `GET …/stream`) stay
//! in the microapp because per-connection filter logic varies by
//! event type. See `agent-creator-microapp::firehose::routes` for
//! a reference implementation that uses
//! [`nexo_microapp_http::sse::sse_filtered_broadcast`].

pub mod broadcast;
pub mod metadata;
pub mod store;
pub mod sweep;

use thiserror::Error;

/// Result alias used by [`EventStore`] operations.
pub type Result<T> = std::result::Result<T, EventsError>;

/// Typed error surface for the `events` feature. Distinct from
/// the SDK runtime [`crate::Error`] so callers can `match` on
/// SQLite vs JSON vs invalid-config without falling back to
/// downcast.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum EventsError {
    /// SQLite operation failed (open, DDL, INSERT, SELECT, …).
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),

    /// JSON encode / decode failed for an event payload. Decode
    /// failures during list-time round-trip are usually warn-logged
    /// + skipped (see [`store::EventStore::list`]); this variant
    /// surfaces the rare cases where serialisation on append fails.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// Table identifier failed validation (must match
    /// `[A-Za-z_][A-Za-z0-9_]*`).
    #[error("invalid table name: {0}")]
    InvalidTable(String),
}

pub use broadcast::{
    build_persisting_listener, EventBroadcastState, DEFAULT_BROADCAST_CAPACITY,
};
pub use metadata::EventMetadata;
pub use store::{
    EventStore, ListFilter, DEFAULT_LIST_LIMIT, DEFAULT_TABLE, MAX_LIST_LIMIT,
};
pub use sweep::{
    spawn_sweep_loop, SweepConfig, SweepHandle, DEFAULT_MAX_ROWS, DEFAULT_RETENTION_DAYS,
    DEFAULT_SWEEP_INTERVAL_SECS,
};
