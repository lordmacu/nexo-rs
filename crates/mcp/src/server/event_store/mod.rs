//! Phase 76.8 — durable session event store for the MCP HTTP
//! transport.
//!
//! ## Why this exists
//!
//! The HTTP transport (Phase 76.1 + 76.7) holds sessions in memory
//! only — when the daemon restarts, every connected client must
//! re-`initialize`. The MCP spec 2025-11-25 admits a softer recovery
//! path: an SSE consumer that knows its `Last-Event-ID` can ask the
//! server to replay missed events from a server-side store.
//!
//! ## Wire contract (matches the leak)
//!
//! * `claude-code-leak/src/cli/transports/SSETransport.ts:159-266`
//!   — SSE frames carry `id: <seq>` + `data: <json>`. Client tracks
//!   `lastSequenceNum` and sends `Last-Event-ID: <seq>` on reconnect.
//! * `claude-code-leak/src/services/mcp/client.ts:189-206` — when
//!   a session is unknown the server returns HTTP 404 + JSON-RPC
//!   `{"error":{"code":-32001,"message":"Session not found"}}`.
//!   Clients treat `-32001` as permanent and re-`initialize`.
//!
//! ## Trait shape
//!
//! Mirrors the [`TurnLogStore`] pattern from
//! `crates/agent-registry/src/turn_log.rs:64-89` (Phase 72) — async
//! trait, in-memory test impl, SQLite prod impl, idempotent upsert
//! via `INSERT OR IGNORE`.
//!
//! Subscriptions (`resources/subscribe` URIs, Phase 76.7) live in a
//! sibling table so a reconnecting client recovers the filtered
//! delivery list along with the missed events.

pub mod config;
pub mod sqlite_store;
pub mod store;
pub mod types;

pub use config::SessionEventStoreConfig;
pub use sqlite_store::SqliteSessionEventStore;
pub use store::{EventStoreError, MemorySessionEventStore, SessionEventStore};
pub use types::StoredEvent;
