//! Phase 79.M — boot context for the exposable-tool dispatcher.
//!
//! Holds the optional handles available when `nexo mcp-server` is
//! standing up its tool registry. Each handle is `Option<_>` so that
//! a missing piece of infrastructure (e.g. `cron.store` not configured
//! in YAML) is recoverable: the dispatcher returns
//! `BootResult::SkippedInfraMissing` for the affected tool while
//! still booting the rest of the catalog.

use std::sync::Arc;

use nexo_broker::AnyBroker;
use nexo_lsp::LspManager;
use nexo_mcp::SessionMcpRuntime;
use nexo_memory::LongTermMemory;
use nexo_taskflow::FlowManager;
use nexo_team_store::TeamStore;
use nexo_web_search::WebSearchRouter;

#[cfg(feature = "config-self-edit")]
use crate::agent::approval_correlator::ApprovalCorrelator;
#[cfg(feature = "config-self-edit")]
use crate::agent::config_tool::{DenylistChecker, ReloadTrigger, SecretRedactor, YamlPatchApplier};
use crate::agent::context::AgentContext;
use crate::agent::workspace_git::MemoryGitRepo;
use crate::config_changes_store::ConfigChangesStore;
use crate::cron_schedule::CronStore;
use crate::link_understanding::LinkExtractor;

/// Read-only bundle of handles consumed by per-tool boot helpers.
#[derive(Clone)]
pub struct McpServerBootContext {
    pub agent_id: String,
    pub broker: AnyBroker,
    pub cron_store: Option<Arc<dyn CronStore>>,
    pub mcp_runtime: Option<Arc<SessionMcpRuntime>>,
    pub config_changes_store: Option<Arc<dyn ConfigChangesStore>>,
    pub web_search_router: Option<Arc<WebSearchRouter>>,
    pub link_extractor: Option<Arc<LinkExtractor>>,
    /// Long-term memory handle. Required by durable follow-up tools.
    pub long_term_memory: Option<Arc<LongTermMemory>>,
    /// Workspace-git audit log handle. Required by
    /// `forge_memory_checkpoint` + `memory_history`.
    pub memory_git: Option<Arc<MemoryGitRepo>>,
    /// TaskFlow manager wrapping the FlowStore. Required by
    /// `taskflow` (single tool, multi-op).
    pub taskflow_manager: Option<Arc<FlowManager>>,
    /// In-process LSP manager. Required by `Lsp`.
    pub lsp_manager: Option<Arc<LspManager>>,
    /// Team store. Required by `TeamList` / `TeamStatus`
    /// (read-only). The mutating Team tools also need a
    /// `TeamMessageRouter`; until then they stay Deferred.
    pub team_store: Option<Arc<dyn TeamStore>>,
    /// Phase 79.M.c.full — Config self-edit handles. Wired only
    /// when the `config-self-edit` Cargo feature is enabled.
    /// Compile-time gate keeps the trait objects out of the type
    /// when the feature is off so default builds carry no overhead.
    #[cfg(feature = "config-self-edit")]
    pub config_yaml_applier: Option<Arc<dyn YamlPatchApplier>>,
    #[cfg(feature = "config-self-edit")]
    pub config_denylist_checker: Option<Arc<dyn DenylistChecker>>,
    #[cfg(feature = "config-self-edit")]
    pub config_secret_redactor: Option<Arc<dyn SecretRedactor>>,
    #[cfg(feature = "config-self-edit")]
    pub config_approval_correlator: Option<Arc<ApprovalCorrelator>>,
    #[cfg(feature = "config-self-edit")]
    pub config_reload_trigger: Option<Arc<dyn ReloadTrigger>>,
    /// Per-agent ConfigToolPolicy (allowed_paths + approval_timeout_secs).
    /// Read from `agents.yaml` at boot time.
    #[cfg(feature = "config-self-edit")]
    pub config_tool_policy: Option<nexo_config::types::config_tool::ConfigToolPolicy>,
    /// Filesystem path where staged proposals land
    /// (`<state_dir>/config-proposals/`).
    #[cfg(feature = "config-self-edit")]
    pub config_proposals_dir: Option<std::path::PathBuf>,
    /// Snapshot of the runtime AgentContext bound to the mcp-server
    /// process. Tools that read context-injected handles
    /// (e.g. `mcp_router_tool` reads `ctx.mcp`) require the caller
    /// to have wired those onto the AgentContext before booting.
    pub agent_context: Arc<AgentContext>,
}

impl McpServerBootContext {
    pub fn builder(
        agent_id: impl Into<String>,
        broker: AnyBroker,
        agent_context: Arc<AgentContext>,
    ) -> McpServerBootContextBuilder {
        McpServerBootContextBuilder {
            agent_id: agent_id.into(),
            broker,
            agent_context,
            cron_store: None,
            mcp_runtime: None,
            config_changes_store: None,
            web_search_router: None,
            link_extractor: None,
            long_term_memory: None,
            memory_git: None,
            taskflow_manager: None,
            lsp_manager: None,
            team_store: None,
            #[cfg(feature = "config-self-edit")]
            config_yaml_applier: None,
            #[cfg(feature = "config-self-edit")]
            config_denylist_checker: None,
            #[cfg(feature = "config-self-edit")]
            config_secret_redactor: None,
            #[cfg(feature = "config-self-edit")]
            config_approval_correlator: None,
            #[cfg(feature = "config-self-edit")]
            config_reload_trigger: None,
            #[cfg(feature = "config-self-edit")]
            config_tool_policy: None,
            #[cfg(feature = "config-self-edit")]
            config_proposals_dir: None,
        }
    }
}

/// Fluent builder. Optional handles default to `None`; per-tool boot
/// helpers translate `None` into `SkippedInfraMissing` with a clear
/// label so operators can fix their YAML.
pub struct McpServerBootContextBuilder {
    agent_id: String,
    broker: AnyBroker,
    agent_context: Arc<AgentContext>,
    cron_store: Option<Arc<dyn CronStore>>,
    mcp_runtime: Option<Arc<SessionMcpRuntime>>,
    config_changes_store: Option<Arc<dyn ConfigChangesStore>>,
    web_search_router: Option<Arc<WebSearchRouter>>,
    link_extractor: Option<Arc<LinkExtractor>>,
    long_term_memory: Option<Arc<LongTermMemory>>,
    memory_git: Option<Arc<MemoryGitRepo>>,
    taskflow_manager: Option<Arc<FlowManager>>,
    lsp_manager: Option<Arc<LspManager>>,
    team_store: Option<Arc<dyn TeamStore>>,
    #[cfg(feature = "config-self-edit")]
    config_yaml_applier: Option<Arc<dyn YamlPatchApplier>>,
    #[cfg(feature = "config-self-edit")]
    config_denylist_checker: Option<Arc<dyn DenylistChecker>>,
    #[cfg(feature = "config-self-edit")]
    config_secret_redactor: Option<Arc<dyn SecretRedactor>>,
    #[cfg(feature = "config-self-edit")]
    config_approval_correlator: Option<Arc<ApprovalCorrelator>>,
    #[cfg(feature = "config-self-edit")]
    config_reload_trigger: Option<Arc<dyn ReloadTrigger>>,
    #[cfg(feature = "config-self-edit")]
    config_tool_policy: Option<nexo_config::types::config_tool::ConfigToolPolicy>,
    #[cfg(feature = "config-self-edit")]
    config_proposals_dir: Option<std::path::PathBuf>,
}

impl McpServerBootContextBuilder {
    pub fn cron_store(mut self, store: Arc<dyn CronStore>) -> Self {
        self.cron_store = Some(store);
        self
    }

    pub fn mcp_runtime(mut self, runtime: Arc<SessionMcpRuntime>) -> Self {
        self.mcp_runtime = Some(runtime);
        self
    }

    pub fn config_changes_store(mut self, store: Arc<dyn ConfigChangesStore>) -> Self {
        self.config_changes_store = Some(store);
        self
    }

    pub fn web_search_router(mut self, router: Arc<WebSearchRouter>) -> Self {
        self.web_search_router = Some(router);
        self
    }

    pub fn link_extractor(mut self, extractor: Arc<LinkExtractor>) -> Self {
        self.link_extractor = Some(extractor);
        self
    }

    pub fn long_term_memory(mut self, memory: Arc<LongTermMemory>) -> Self {
        self.long_term_memory = Some(memory);
        self
    }

    pub fn memory_git(mut self, git: Arc<MemoryGitRepo>) -> Self {
        self.memory_git = Some(git);
        self
    }

    pub fn taskflow_manager(mut self, mgr: Arc<FlowManager>) -> Self {
        self.taskflow_manager = Some(mgr);
        self
    }

    pub fn lsp_manager(mut self, mgr: Arc<LspManager>) -> Self {
        self.lsp_manager = Some(mgr);
        self
    }

    pub fn team_store(mut self, store: Arc<dyn TeamStore>) -> Self {
        self.team_store = Some(store);
        self
    }

    #[cfg(feature = "config-self-edit")]
    pub fn config_handles(
        mut self,
        applier: Arc<dyn YamlPatchApplier>,
        denylist: Arc<dyn DenylistChecker>,
        redactor: Arc<dyn SecretRedactor>,
        correlator: Arc<ApprovalCorrelator>,
        reload: Arc<dyn ReloadTrigger>,
        policy: nexo_config::types::config_tool::ConfigToolPolicy,
        proposals_dir: std::path::PathBuf,
    ) -> Self {
        self.config_yaml_applier = Some(applier);
        self.config_denylist_checker = Some(denylist);
        self.config_secret_redactor = Some(redactor);
        self.config_approval_correlator = Some(correlator);
        self.config_reload_trigger = Some(reload);
        self.config_tool_policy = Some(policy);
        self.config_proposals_dir = Some(proposals_dir);
        self
    }

    pub fn build(self) -> McpServerBootContext {
        McpServerBootContext {
            agent_id: self.agent_id,
            broker: self.broker,
            agent_context: self.agent_context,
            cron_store: self.cron_store,
            mcp_runtime: self.mcp_runtime,
            config_changes_store: self.config_changes_store,
            web_search_router: self.web_search_router,
            link_extractor: self.link_extractor,
            long_term_memory: self.long_term_memory,
            memory_git: self.memory_git,
            taskflow_manager: self.taskflow_manager,
            lsp_manager: self.lsp_manager,
            team_store: self.team_store,
            #[cfg(feature = "config-self-edit")]
            config_yaml_applier: self.config_yaml_applier,
            #[cfg(feature = "config-self-edit")]
            config_denylist_checker: self.config_denylist_checker,
            #[cfg(feature = "config-self-edit")]
            config_secret_redactor: self.config_secret_redactor,
            #[cfg(feature = "config-self-edit")]
            config_approval_correlator: self.config_approval_correlator,
            #[cfg(feature = "config-self-edit")]
            config_reload_trigger: self.config_reload_trigger,
            #[cfg(feature = "config-self-edit")]
            config_tool_policy: self.config_tool_policy,
            #[cfg(feature = "config-self-edit")]
            config_proposals_dir: self.config_proposals_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::context::AgentContext;
    use nexo_broker::AnyBroker;

    fn fixture_ctx() -> Arc<AgentContext> {
        use nexo_config::types::agents::{
            AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
            OutboundAllowlistConfig, WorkspaceGitConfig,
        };
        let cfg = AgentConfig {
            id: "a".into(),
            model: ModelConfig {
                provider: "x".into(),
                model: "y".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
        repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
            event_subscribers: Vec::new(),
        };
        Arc::new(AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(crate::session::SessionManager::new(
                std::time::Duration::from_secs(60),
                8,
            )),
        ))
    }

    #[tokio::test]
    async fn builder_defaults_all_optional_handles_to_none() {
        let ctx = fixture_ctx();
        let broker = AnyBroker::local();
        let bc = McpServerBootContext::builder("agent-1", broker, ctx).build();
        assert_eq!(bc.agent_id, "agent-1");
        assert!(bc.cron_store.is_none());
        assert!(bc.mcp_runtime.is_none());
        assert!(bc.config_changes_store.is_none());
        assert!(bc.web_search_router.is_none());
        assert!(bc.link_extractor.is_none());
        assert!(bc.memory_git.is_none());
        assert!(bc.taskflow_manager.is_none());
        assert!(bc.lsp_manager.is_none());
        assert!(bc.team_store.is_none());
    }
}
