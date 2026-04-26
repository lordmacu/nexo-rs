//! Phase 67.B — multi-agent registry.
//!
//! Tracks every in-flight driver goal with a live snapshot
//! (turn N/M, last decision, last acceptance, diff_stat) and a
//! persistent backing store so the daemon can rehydrate after a
//! restart and answer "qué hace el agente X" via the chat tools.
//!
//! 67.B.2 ships the trait surface + SQLite store + lifecycle tests.
//! 67.B.3 adds the cap / queue / live event subscriber. 67.B.4 wires
//! reattach + log buffer.

pub mod log_buffer;
pub mod reattach;
pub mod registry;
pub mod store;
pub mod types;

pub use log_buffer::{LogBuffer, LogLine};
pub use reattach::{reattach, ReattachOptions, ReattachOutcome};
pub use registry::{AdmitOutcome, AgentRegistry};
pub use store::{
    AgentRegistryStore, AgentRegistryStoreError, MemoryAgentRegistryStore, SqliteAgentRegistryStore,
};
pub use types::{AgentHandle, AgentRunStatus, AgentSnapshot, AgentSummary, RegistryError};
