//! Phase 67.D — agent-loop tools for dispatching driver goals.
//!
//! Layered as:
//!
//! - `policy_gate` (this step) — pure-function admission check.
//! - `program_phase` / `dispatch_followup` (67.E.1, 67.E.2) — tool
//!   handlers that build a `Goal` and ask the orchestrator to spawn.
//! - `chain` / `agent_control` / `agent_query` / `admin` / `hooks`
//!   (67.E.x onwards) — the rest of the multi-agent surface.

pub mod policy_gate;

pub use policy_gate::{DispatchDenied, DispatchGate, DispatchRequest};
