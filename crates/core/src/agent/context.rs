use super::peer_directory::PeerDirectory;
use super::routing::AgentRouter;
use crate::session::SessionManager;
use agent_broker::AnyBroker;
use agent_config::types::agents::AgentConfig;
use agent_mcp::SessionMcpRuntime;
use agent_memory::LongTermMemory;
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
        }
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
}
