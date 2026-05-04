// Module name intentionally matches parent — `agent::agent::Agent` is the
// canonical path for the aggregate type. Splitting the struct out of this
// module would ripple through every `use nexo_core::agent::Agent;`.
#[allow(clippy::module_inception)]
pub mod agent;
pub mod admin_rpc;
pub mod agent_events;
pub mod agents_directory;
pub mod approval_correlator;
pub mod behavior;
pub mod binding_validate;
pub mod built_in_deferred;
pub mod channel_adapter;
pub mod compaction;
pub mod config_changes_tail_tool;
#[cfg(feature = "config-self-edit")]
pub mod config_tool;
pub mod context;
pub mod cron_tool;
pub mod delegation_tool;
pub mod dispatch_handlers;
pub mod dreaming;
pub mod effective;
pub mod event_subscriber;
pub mod extension_hook;
pub mod extension_tool;
pub mod followup_tool;
pub mod heartbeat_tool;
pub mod hook_registry;
pub mod inbox;
pub mod inbox_router;
pub mod list_peers_tool;
pub mod llm_behavior;
pub mod lsp_tool;
pub mod mcp_catalog;
pub mod mcp_resource_tool;
pub mod mcp_router_tool;
pub mod mcp_server_bridge;
pub mod mcp_session;
pub mod mcp_tool;
pub mod memory_checkpoint_tool;
pub mod memory_history_tool;
pub mod memory_snapshot_tool;
pub mod memory_tool;
pub mod mock_plugin;
pub mod nexo_plugin_registry;
pub mod noop;
pub mod notebook_edit_tool;
pub mod peer_directory;
pub mod personas;
pub mod plan_mode_tool;
pub mod plugin;
pub mod plugin_host;
pub mod proactive_hint;
pub mod prompt_assembly;
pub mod rate_limit;
pub mod redaction;
pub mod repl_registry;
#[cfg(feature = "repl-tool")]
pub mod repl_tool;
pub mod registry;
pub mod remote_trigger_tool;
pub mod routing;
pub mod runtime;
pub mod schema_validator;
pub mod scoped_tool_registry;
pub mod self_report;
pub mod channel_list_tool;
pub mod channel_send_tool;
pub mod channel_status_tool;
pub mod send_message_to_worker_tool;
pub mod send_to_peer_tool;
pub mod worker_registry;
pub mod send_user_message_tool;
pub mod sender_rate_limit;
pub mod session_logs_tool;
pub mod skills;
pub mod sleep_tool;
pub mod synthetic_output_tool;
pub mod taskflow_tool;
pub mod team_tools;
pub mod todo_write_tool;
pub mod tool_filter;
pub mod tool_policy;
pub mod tool_registry;
pub mod tool_registry_cache;
pub mod tool_search_tool;
pub mod transcripts;
pub mod transcripts_index;
pub mod types;
pub mod web_fetch_tool;
pub mod web_search_tool;
pub mod workspace;
pub mod workspace_cache;
pub mod workspace_git;
pub use agent::Agent;
pub use agents_directory::{AgentInfo, AgentsDirectory};
pub use behavior::AgentBehavior;
pub use built_in_deferred::{mark_built_in_deferred, BUILT_IN_DEFERRED_TOOLS};
pub use nexo_driver_types::{
    AutoCompactBreaker, CompactContext, CompactPolicy, CompactTrigger, DefaultCompactPolicy,
};
pub use binding_validate::{
    collect_binding_errors, collect_binding_errors_with_providers, validate_agent, validate_agents,
    validate_agents_with_providers, BindingValidationError, KnownProviders, KnownTools,
};
pub use context::AgentContext;
pub use delegation_tool::DelegationTool;
pub use dreaming::{DreamCandidate, DreamEngine, DreamReport, DreamWeights, DreamingConfig};
pub use effective::EffectiveBindingPolicy;
pub use extension_hook::ExtensionHook;
pub use extension_tool::{ExtensionTool, EXT_NAME_PREFIX};
pub use followup_tool::{CancelFollowupTool, CheckFollowupTool, StartFollowupTool};
pub use heartbeat_tool::HeartbeatTool;
pub use hook_registry::{HookHandler, HookOutcome, HookRegistry};
pub use llm_behavior::LlmAgentBehavior;
pub use mcp_catalog::{McpCatalogEntry, McpServerSummary, McpToolCatalog};
pub use mcp_resource_tool::{
    McpResourceListTool, McpResourceReadTool, RESOURCE_LIST_SUFFIX, RESOURCE_READ_SUFFIX,
};
pub use mcp_server_bridge::ToolRegistryBridge;
pub use mcp_session::{
    build_session_catalog, build_session_catalog_with_context, register_session_tools,
    register_session_tools_with_context, register_session_tools_with_overrides,
};
pub use mcp_tool::{sanitize_name_fragment, McpTool, MCP_NAME_PREFIX};
pub use memory_checkpoint_tool::MemoryCheckpointTool;
pub use memory_history_tool::MemoryHistoryTool;
pub use memory_snapshot_tool::MemorySnapshotTool;
pub use memory_tool::MemoryTool;
pub use mock_plugin::MockPlugin;
pub use noop::NoOpAgent;
pub use peer_directory::{PeerDirectory, PeerSummary};
pub use plugin::{Command, Plugin, Response};
pub use plugin_host::{
    NexoPlugin, PluginInitContext, PluginInitError, PluginShutdownError,
    DEFAULT_PLUGIN_SHUTDOWN_TIMEOUT,
};
pub use rate_limit::{ToolRateLimitConfig, ToolRateLimiter, ToolRateLimitsConfig};
pub use redaction::{RedactionReport, Redactor};
pub use repl_registry::{ReplOutput, ReplRegistry, ReplSession};
#[cfg(feature = "repl-tool")]
pub use repl_tool::ReplTool;
pub use registry::PluginRegistry;
pub use routing::{AgentMessage, AgentPayload, AgentRouter};
pub use runtime::AgentRuntime;
pub use schema_validator::ToolArgsValidator;
pub use self_report::{MyStatsTool, WhatDoIKnowTool, WhoAmITool};
pub use session_logs_tool::SessionLogsTool;
pub use skills::{
    BinVersionSpec, LoadedSkill, MissingVersion, SkillLoadAction, SkillLoadStatus, SkillLoader,
    VersionFailReason,
};
pub use sleep_tool::{extract_sleep_ms, is_sleep_result, SleepTool, SLEEP_SENTINEL};
pub use taskflow_tool::{TaskFlowTool, TaskFlowToolGuardrails};
pub use scoped_tool_registry::{
    NamespaceEnforcement, NamespaceViolation, NamespaceViolationReason, ScopedToolRegistry,
    RESERVED_PREFIXES,
};
pub use tool_registry::{ToolHandler, ToolRegistry};
pub use tool_registry_cache::ToolRegistryCache;
pub use transcripts::{
    SessionHeader, TranscriptEntry, TranscriptLine, TranscriptRole, TranscriptWriter,
    TRANSCRIPT_VERSION,
};
pub use transcripts_index::{IndexedHit, TranscriptsIndex};
pub use types::{InboundMessage, MessagePriority, RunTrigger};
pub use web_fetch_tool::WebFetchTool;
pub use web_search_tool::WebSearchTool;
pub use workspace::{
    AgentIdentity, DailyNote, LoadLimits, SessionScope, WorkspaceBundle, WorkspaceLoader,
};
pub use workspace_git::{CommitSummary, MemoryGitCheckpointer, MemoryGitRepo};
