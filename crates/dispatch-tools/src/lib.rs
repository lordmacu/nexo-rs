//! Phase 67.D — agent-loop tools for dispatching driver goals.
//!
//! Layered as:
//!
//! - `policy_gate` (this step) — pure-function admission check.
//! - `program_phase` / `dispatch_followup` (67.E.1, 67.E.2) — tool
//!   handlers that build a `Goal` and ask the orchestrator to spawn.
//! - `chain` / `agent_control` / `agent_query` / `admin` / `hooks`
//!   (67.E.x onwards) — the rest of the multi-agent surface.

pub mod admin;
pub mod agent_control;
pub mod subjects;
pub mod agent_query;
pub mod chain;
pub mod dispatch_followup;
pub mod event_forwarder;
pub mod hooks;
pub mod policy_gate;
pub mod program_phase;
pub mod tool_names;

pub use subjects::{
    registry_snapshot_subject, DispatchDeniedPayload, DispatchSpawnedPayload, DispatchTelemetry,
    HookDispatchedPayload, HookFailedPayload, NoopTelemetry,
};
pub use admin::{
    evict_completed, flush_agent_queue, set_concurrency_cap, AdminError, EvictCompletedInput,
    EvictCompletedOutput, FlushAgentQueueOutput, SetConcurrencyCapInput, SetConcurrencyCapOutput,
};
pub use agent_control::{
    cancel_agent, pause_agent, resume_agent, update_budget, AgentControlError, CancelAgentInput,
    CancelAgentOutput, PauseAgentInput, PauseAgentOutput, UpdateBudgetInput, UpdateBudgetOutput,
};
pub use agent_query::{
    agent_hooks_list, agent_logs_tail, agent_status, list_agents, AgentHooksListInput,
    AgentLogsTailInput, AgentStatusInput, ListAgentsInput,
};
pub use hooks::HookRegistry;
pub use chain::{
    program_phase_chain, program_phase_parallel, ProgramPhaseChainInput, ProgramPhaseChainOutput,
    ProgramPhaseParallelInput, ProgramPhaseParallelOutput,
};

pub use hooks::{
    CompletionHook, DefaultHookDispatcher, DispatchPhaseChainer, HookAction, HookDispatcher,
    HookError, HookPayload, HookRegistryStore, HookStoreError, HookTransition, HookTrigger,
    NatsHookPublisher, NoopNatsHookPublisher, SqliteHookRegistryStore,
};

pub use event_forwarder::EventForwarder;
pub use dispatch_followup::{
    dispatch_followup_call, followup_phase_id, DispatchFollowupInput, DispatchFollowupOutput,
};
pub use program_phase::{
    program_phase_dispatch, BudgetOverride, ProgramPhaseError, ProgramPhaseInput,
    ProgramPhaseOutput,
};

pub use policy_gate::{DispatchDenied, DispatchGate, DispatchRequest};
pub use tool_names::{
    allowed_tool_names, should_register, ToolGroup, ADMIN_TOOL_NAMES, READ_TOOL_NAMES,
    WRITE_TOOL_NAMES,
};
