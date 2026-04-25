use super::effective::EffectiveBindingPolicy;
use super::peer_directory::PeerDirectory;
use super::redaction::Redactor;
use super::routing::AgentRouter;
use super::tool_registry::ToolRegistry;
use super::transcripts_index::TranscriptsIndex;
use crate::session::SessionManager;
use nexo_broker::AnyBroker;
use nexo_config::types::agents::AgentConfig;
use nexo_mcp::SessionMcpRuntime;
use nexo_memory::LongTermMemory;
use std::sync::Arc;
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
        }
    }
    pub fn with_web_search_router(
        mut self,
        router: Arc<nexo_web_search::WebSearchRouter>,
    ) -> Self {
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
