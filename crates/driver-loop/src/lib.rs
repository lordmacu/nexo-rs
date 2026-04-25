//! Phase 67.4 — driver agent loop. See `README.md` and
//! `docs/src/architecture/driver-subsystem.md` for the architecture.

pub mod acceptance;
pub mod attempt;
pub mod config;
pub mod error;
pub mod events;
pub mod harness;
pub mod llm_decider;
pub mod mcp_config;
pub mod memory;
pub mod orchestrator;
pub mod prompt;
pub mod socket;
pub mod workspace;

pub use acceptance::{
    AcceptanceEvaluator, CustomVerifier, CustomVerifierRegistry, DefaultAcceptanceEvaluator,
    GitClean, NoPathsTouched, NoopAcceptanceEvaluator, ShellResult, ShellRunner,
};
pub use config::{
    AcceptanceConfig, BindingStoreConfig, BindingStoreKind, DeciderConfig, DriverBinConfig,
    DriverConfig, PermissionConfig, WorkspaceConfig, WorkspaceGitConfig,
};
pub use error::DriverError;
#[cfg(feature = "nats")]
pub use events::NatsEventSink;
pub use events::{DriverEvent, DriverEventSink, NoopEventSink};
pub use harness::ClaudeHarness;
pub use llm_decider::LlmDecider;
pub use mcp_config::write_mcp_config;
pub use memory::{DecisionMemory, NoopDecisionMemory};
pub use orchestrator::{DriverOrchestrator, DriverOrchestratorBuilder, GoalOutcome};
pub use prompt::compose_turn_prompt;
pub use socket::{DriverSocketServer, SocketMessage};
pub use workspace::{GitWorktreeMode, WorkspaceManager};
