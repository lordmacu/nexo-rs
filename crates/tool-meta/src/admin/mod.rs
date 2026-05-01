//! Phase 82.10 — admin RPC wire-shape types.
//!
//! Shared between the daemon's [`nexo_core::agent::admin_rpc`]
//! handlers and the microapp SDK's `AdminClient`. Living here
//! (in `nexo-tool-meta`) keeps the SDK from depending on
//! `nexo-core` while still letting both sides agree on the
//! params + result shapes.
//!
//! Each sub-module corresponds to one admin domain. v1 ships
//! `agents`; subsequent phases land `credentials`, `pairing`,
//! `llm_providers`, `channels` (82.10.d-f).

pub mod agent_events;
pub mod agents;
pub mod channels;
pub mod credentials;
pub mod llm_providers;
pub mod pairing;
pub mod processing;
