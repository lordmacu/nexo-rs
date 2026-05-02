//! Phase 82.10 — admin RPC dispatcher.
//!
//! Receives JSON-RPC requests with `app:` ID prefix initiated by
//! microapps over their stdio transport, looks up the handler for
//! `nexo/admin/<domain>/<method>`, checks the operator-granted
//! capability, runs the handler, and returns a typed result.
//!
//! Sub-phase scope:
//! - **82.10.a** (this file): single mock `nexo/admin/echo` handler
//!   for end-to-end shape validation. Capability gates + audit
//!   land in 82.10.b.
//! - **82.10.b**: `CapabilitySet`, boot validation, audit log.
//! - **82.10.c-f**: 5 admin domains (agents / credentials /
//!   pairing / llm_providers / channels).

pub mod audit;
pub mod audit_sqlite;
pub mod capabilities;
pub mod channel_outbound;
pub mod dispatcher;
pub mod domains;
pub mod escalations_sqlite;
pub mod processing_sqlite;
pub mod router;
pub mod transcript_appender;

pub use audit_sqlite::{
    format_rows_as_json, format_rows_as_table, AuditTailFilter, SqliteAdminAuditWriter,
};
pub use escalations_sqlite::SqliteEscalationStore;
pub use processing_sqlite::SqliteProcessingControlStore;

pub use audit::{
    hash_params, now_epoch_ms, AdminAuditResult, AdminAuditRow, AdminAuditWriter,
    InMemoryAuditWriter,
};
pub use capabilities::{
    validate_capabilities_at_boot, AdminCapabilityDecl, CapabilityBootError,
    CapabilityBootReport, CapabilityBootWarn, CapabilitySet,
};
pub use channel_outbound::{
    ChannelOutboundDispatcher, ChannelOutboundError, OutboundAck, OutboundMessage,
};
pub use dispatcher::{AdminRpcDispatcher, AdminRpcError, AdminRpcResult};
pub use router::{AdminOutboundWriter, DispatcherAdminRouter};
