//! TaskFlow runtime — durable, multi-step workflows.
//!
//! Phase 14 of the agent framework. A `Flow` represents a long-running job
//! with persisted state, revision-checked mutations, and child-task linkage.
//! Designed to survive process restart and to coordinate work that waits on
//! external events (timers, NATS messages, agent-to-agent delegation).

pub mod engine;
pub mod manager;
pub mod store;
pub mod types;

pub use engine::{TickReport, WaitCondition, WaitEngine};
pub use manager::{CreateManagedInput, FlowManager, StepObservation};
pub use store::{FlowStore, SqliteFlowStore};
pub use types::{Flow, FlowError, FlowEvent, FlowStatus, FlowStep, FlowStepStatus, StepRuntime};
