//! Phase 82.10 — admin RPC domain handlers.
//!
//! Each sub-module owns one of the 5 admin domains:
//! - `agents` — CRUD agents.yaml (82.10.c)
//! - `credentials` — register/revoke channel credentials (82.10.d)
//! - `pairing` — start/status/cancel WhatsApp QR pairing (82.10.e)
//! - `llm_providers` — manage llm.yaml provider entries (82.10.f)
//! - `channels` — approve/revoke MCP-channel servers in
//!   agents.yaml (82.10.f)
//!
//! Domains are registered with the [`super::AdminRpcDispatcher`]
//! at boot.

pub mod agent_events;
pub mod agents;
pub mod channels;
pub mod credentials;
pub mod llm_providers;
pub mod pairing;
pub mod processing;
