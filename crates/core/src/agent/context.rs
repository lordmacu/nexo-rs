use super::effective::EffectiveBindingPolicy;
use super::peer_directory::PeerDirectory;
use super::redaction::Redactor;
use super::routing::AgentRouter;
use super::tool_registry::ToolRegistry;
use super::transcripts_index::TranscriptsIndex;
use crate::plan_mode::PlanModeState;
use crate::session::SessionManager;
use crate::todo::TodoList;
use nexo_broker::AnyBroker;
use nexo_config::types::agents::AgentConfig;
use nexo_mcp::SessionMcpRuntime;
use nexo_memory::LongTermMemory;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;
#[derive(Clone)]
pub struct AgentContext {
    pub agent_id: String,
    pub config: Arc<AgentConfig>,
    pub broker: AnyBroker,
    pub sessions: Arc<SessionManager>,
    pub memory: Option<Arc<LongTermMemory>>,
    pub router: Option<Arc<AgentRouter>>,
    /// Snapshot of peer agents running in this process. Feeds the
    /// auto-generated `# PEERS` system-prompt block so the LLM knows
    /// which ids to pass to `delegate(...)`. `None` in test/bootstrap
    /// contexts where peer discovery doesn't apply.
    pub peers: Option<Arc<PeerDirectory>>,
    /// Phase 12.4 — MCP runtime scoped to this session (if MCP is enabled).
    pub mcp: Option<Arc<SessionMcpRuntime>>,
    /// Phase 11.5 follow-up — active session id when the context is built
    /// inside an LLM turn. None for contexts built outside the loop
    /// (heartbeat bootstrap, tests). Used by tool handlers that opt into
    /// context passthrough.
    pub session_id: Option<Uuid>,
    /// Per-binding capability snapshot resolved at intake. `Some` when the
    /// runtime matched the inbound event to an `InboundBinding` for this
    /// agent; `None` for paths without a binding match (delegation
    /// receive, heartbeat, tests). Use [`AgentContext::effective_policy`]
    /// to access a policy that always has a value — it synthesises one
    /// from the agent-level config when `effective` is `None`.
    pub effective: Option<Arc<EffectiveBindingPolicy>>,
    /// Per-binding tool registry — shares handlers with the agent's base
    /// registry but only exposes tools that survive the binding's
    /// `allowed_tools` filter. `None` on code paths without a binding
    /// match (delegation receive, heartbeat, tests); consumers fall
    /// back to the behavior's base registry in that case.
    pub effective_tools: Option<Arc<ToolRegistry>>,
    /// Phase 17 — resolver that maps this agent's id to the opaque
    /// credential handles it is allowed to use for outbound traffic.
    /// `None` in early-boot / test contexts; consumers must treat that
    /// as "no credentials configured" (tools return an unbound error
    /// rather than publishing from an arbitrary account).
    pub credentials: Option<Arc<nexo_auth::AgentCredentialResolver>>,
    /// Phase 17 — per-(channel, instance) breaker registry shared by
    /// plugin outbound tools. `None` for runtimes without credentials.
    pub breakers: Option<Arc<nexo_auth::BreakerRegistry>>,
    /// Pre-persistence redactor for transcript content. `None` in
    /// test/bootstrap contexts → behavior keeps content untouched.
    pub redactor: Option<Arc<Redactor>>,
    /// FTS5 index over transcript content. `None` when the subsystem
    /// is disabled or initialization failed; consumers fall back to
    /// JSONL-only persistence + substring scan.
    pub transcripts_index: Option<Arc<TranscriptsIndex>>,
    /// Phase 21 — shared link extractor (HTTP client + LRU cache).
    /// `None` in early-boot / test contexts; llm_behavior treats
    /// that as "link understanding disabled regardless of config".
    pub link_extractor: Option<Arc<crate::link_understanding::LinkExtractor>>,
    /// Phase 25 — shared multi-provider web-search router. `None`
    /// when no provider is configured for this process; the
    /// `web_search` tool errors out cleanly in that case.
    pub web_search_router: Option<Arc<nexo_web_search::WebSearchRouter>>,
    /// Phase F follow-up (hot-reload) — current effective enables for
    /// the four context-optimization mechanisms. Set per-event by
    /// `AgentRuntime` from `RuntimeSnapshot::context_optimization`, so
    /// a config reload that flips a flag is observed on the *next*
    /// turn without restarting the behavior. `None` for legacy /
    /// test contexts that haven't been wired through the snapshot —
    /// in that case `llm_behavior` falls back to the boot-time
    /// `prompt_cache_enabled` / `compaction_runtime.enabled` flags.
    pub context_optimization: Option<nexo_config::types::llm::ResolvedContextOptimization>,
    /// PT-1 — bundle of services consumed by the dispatch tool
    /// handlers (program_phase, list_agents, etc.). Populated at
    /// boot when the project tracker is enabled. `None` keeps the
    /// dispatch tools off — handlers return a friendly error so
    /// the LLM doesn't pretend they worked.
    pub dispatch: Option<Arc<super::dispatch_handlers::DispatchToolContext>>,
    /// B3 — sender's pairing-trust bit, set by intake after the
    /// pairing gate runs (Phase 26). Defaults to `false` so any
    /// path that forgets to thread it through fails closed under
    /// `require_trusted=true`. Read-only tools bypass this gate.
    pub sender_trusted: bool,
    /// B3 — `(plugin, instance, sender_id)` of the inbound event
    /// that produced this turn, when the runtime matched a binding.
    /// Lets the dispatch handler synthesise an `OriginChannel` for
    /// `program_phase` so `notify_origin` lands back in the chat.
    pub inbound_origin: Option<(String, String, String)>,
    /// Phase 79.1 — plan-mode state for this goal. Shared across the
    /// dispatcher (read on every tool call) and the EnterPlanMode /
    /// ExitPlanMode tools (write). SQLite is canonical (column on
    /// `agent_registry.goals.plan_mode`); this is a hot cache. New
    /// contexts default to `Off`; the runtime hydrates the value from
    /// the registry at goal spawn / reattach (Phase 71).
    pub plan_mode: Arc<RwLock<PlanModeState>>,
    /// Phase 79.1 — process-shared registry of pending plan-mode
    /// approvals. `EnterPlanMode` does not touch it; `ExitPlanMode`
    /// installs a waiter when `plan_mode.require_approval` is on; the
    /// `plan_mode_resolve` operator tool fires the matching waiter.
    /// Tests construct their own registry to avoid cross-test races.
    pub plan_approval_registry:
        Arc<crate::agent::plan_mode_tool::PlanApprovalRegistry>,
    /// Phase 79.4 — intra-turn scratch todo list. Owned by the model
    /// (mutated via `TodoWrite`). Distinct from Phase 14 TaskFlow:
    /// Todo is in-memory + per-goal + flat; TaskFlow is persistent
    /// + cross-session + DAG. Reattach does not restore todos —
    /// they die with the goal because re-deriving them mid-turn is
    /// cheap and stale items are confusing.
    pub todos: Arc<RwLock<TodoList>>,
}
impl AgentContext {
    pub fn new(
        agent_id: impl Into<String>,
        config: Arc<AgentConfig>,
        broker: AnyBroker,
        sessions: Arc<SessionManager>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            config,
            broker,
            sessions,
            memory: None,
            router: None,
            peers: None,
            mcp: None,
            session_id: None,
            effective: None,
            effective_tools: None,
            credentials: None,
            breakers: None,
            redactor: None,
            transcripts_index: None,
            link_extractor: None,
            web_search_router: None,
            context_optimization: None,
            dispatch: None,
            sender_trusted: false,
            inbound_origin: None,
            plan_mode: Arc::new(RwLock::new(PlanModeState::default())),
            plan_approval_registry: Arc::new(
                crate::agent::plan_mode_tool::PlanApprovalRegistry::default(),
            ),
            todos: Arc::new(RwLock::new(TodoList::new())),
        }
    }

    /// Phase 79.1 — install a pre-built plan-mode handle. Used at
    /// goal hydration so the runtime can share the same `Arc<RwLock>`
    /// between the dispatcher (gate) and the registry mirror (write
    /// path).
    pub fn with_plan_mode(mut self, state: Arc<RwLock<PlanModeState>>) -> Self {
        self.plan_mode = state;
        self
    }

    /// Phase 79.1 — install a process-shared plan-mode approval
    /// registry. Production wiring constructs one per process and
    /// hands it to every `AgentContext`; tests build their own to
    /// avoid cross-test races.
    pub fn with_plan_approval_registry(
        mut self,
        registry: Arc<crate::agent::plan_mode_tool::PlanApprovalRegistry>,
    ) -> Self {
        self.plan_approval_registry = registry;
        self
    }

    /// Phase 79.1 — `true` when this goal is rooted in a live channel
    /// that can deliver an operator approval message. Sub-agent goals
    /// (delegations, future TeamCreate workers), cron / poller /
    /// heartbeat-spawned goals, and bootstrap contexts all return
    /// `false` because they have no inbound channel through which an
    /// operator could approve a plan.
    ///
    /// Reference: `research/src/acp/session-interaction-mode.ts:4-15`
    /// — same intent, "interactive" vs "parent-owned-background".
    pub fn is_interactive(&self) -> bool {
        self.inbound_origin.is_some()
    }

    pub fn with_sender_trusted(mut self, v: bool) -> Self {
        self.sender_trusted = v;
        self
    }

    pub fn with_inbound_origin(
        mut self,
        plugin: impl Into<String>,
        instance: impl Into<String>,
        sender_id: impl Into<String>,
    ) -> Self {
        self.inbound_origin = Some((plugin.into(), instance.into(), sender_id.into()));
        self
    }

    pub fn with_dispatch(mut self, d: Arc<super::dispatch_handlers::DispatchToolContext>) -> Self {
        self.dispatch = Some(d);
        self
    }
    pub fn with_web_search_router(mut self, router: Arc<nexo_web_search::WebSearchRouter>) -> Self {
        self.web_search_router = Some(router);
        self
    }
    /// Set the per-turn context-optimization snapshot. Called by the
    /// agent runtime intake after loading the active `RuntimeSnapshot`,
    /// so a hot-reload that swaps the snapshot is observed without
    /// rebuilding the behavior.
    pub fn with_context_optimization(
        mut self,
        co: nexo_config::types::llm::ResolvedContextOptimization,
    ) -> Self {
        self.context_optimization = Some(co);
        self
    }
    pub fn with_redactor(mut self, redactor: Arc<Redactor>) -> Self {
        self.redactor = Some(redactor);
        self
    }
    pub fn with_transcripts_index(mut self, index: Arc<TranscriptsIndex>) -> Self {
        self.transcripts_index = Some(index);
        self
    }
    pub fn with_link_extractor(
        mut self,
        ext: Arc<crate::link_understanding::LinkExtractor>,
    ) -> Self {
        self.link_extractor = Some(ext);
        self
    }
    pub fn with_memory(mut self, memory: Arc<LongTermMemory>) -> Self {
        self.memory = Some(memory);
        self
    }
    pub fn with_router(mut self, router: Arc<AgentRouter>) -> Self {
        self.router = Some(router);
        self
    }
    pub fn with_peers(mut self, peers: Arc<PeerDirectory>) -> Self {
        self.peers = Some(peers);
        self
    }
    pub fn with_mcp(mut self, mcp: Arc<SessionMcpRuntime>) -> Self {
        self.mcp = Some(mcp);
        self
    }
    pub fn with_session_id(mut self, id: Uuid) -> Self {
        self.session_id = Some(id);
        self
    }
    pub fn with_effective(mut self, effective: Arc<EffectiveBindingPolicy>) -> Self {
        self.effective = Some(effective);
        self
    }
    pub fn with_effective_tools(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.effective_tools = Some(tools);
        self
    }
    pub fn with_credentials(
        mut self,
        credentials: Arc<nexo_auth::AgentCredentialResolver>,
    ) -> Self {
        self.credentials = Some(credentials);
        self
    }
    pub fn with_breakers(mut self, breakers: Arc<nexo_auth::BreakerRegistry>) -> Self {
        self.breakers = Some(breakers);
        self
    }
    /// Returns the active effective policy, synthesising one from the
    /// agent-level config when no binding was matched. Cheap to call in
    /// hot paths: returns an existing `Arc` when available and builds a
    /// fresh one only for unbound contexts.
    pub fn effective_policy(&self) -> Arc<EffectiveBindingPolicy> {
        if let Some(eff) = &self.effective {
            return Arc::clone(eff);
        }
        Arc::new(EffectiveBindingPolicy::from_agent_defaults(&self.config))
    }
}

#[cfg(test)]
mod plan_mode_tests {
    use super::*;
    use crate::plan_mode::{PlanModeReason, PlanModeState};
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };

    fn ctx() -> AgentContext {
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
        };
        AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    #[tokio::test]
    async fn plan_mode_default_off() {
        let c = ctx();
        assert!(c.plan_mode.read().await.is_off());
    }

    #[tokio::test]
    async fn plan_mode_set_then_read() {
        let c = ctx();
        {
            let mut g = c.plan_mode.write().await;
            *g = PlanModeState::on(
                42,
                PlanModeReason::ModelRequested {
                    reason: Some("rationale".into()),
                },
            );
        }
        assert!(c.plan_mode.read().await.is_on());
    }

    #[tokio::test]
    async fn is_interactive_requires_inbound_origin() {
        let c = ctx();
        assert!(!c.is_interactive());
        let c = c.with_inbound_origin("whatsapp", "default", "+1234");
        assert!(c.is_interactive());
    }

    #[tokio::test]
    async fn with_plan_mode_shares_handle() {
        let shared = Arc::new(RwLock::new(PlanModeState::on(
            7,
            PlanModeReason::OperatorRequested,
        )));
        let c = ctx().with_plan_mode(Arc::clone(&shared));
        // Mutating the shared handle is observed via the context
        // — proves the Arc was wired through, not cloned-by-value.
        {
            let mut g = shared.write().await;
            *g = PlanModeState::Off;
        }
        assert!(c.plan_mode.read().await.is_off());
    }
}
