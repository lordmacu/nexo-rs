#![allow(clippy::all)] // Phase 79 scaffolding — re-enable when 79.x fully shipped

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
    /// Phase 79.12 — REPL session registry. `Some` when `repl-tool`
    /// feature is enabled AND the binding config has `repl.enabled`.
    /// Holds persistent Python/Node/bash subprocesses.
    pub repl_registry: Option<Arc<super::repl_registry::ReplRegistry>>,
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
    pub plan_approval_registry: Arc<crate::agent::plan_mode_tool::PlanApprovalRegistry>,
    /// Phase 79.4 — intra-turn scratch todo list. Owned by the model
    /// (mutated via `TodoWrite`). Distinct from Phase 14 TaskFlow:
    /// Todo is in-memory + per-goal + flat; TaskFlow is persistent
    /// + cross-session + DAG. Reattach does not restore todos —
    /// they die with the goal because re-deriving them mid-turn is
    /// cheap and stale items are confusing.
    pub todos: Arc<RwLock<TodoList>>,
    /// Phase 79.6 — when set, this goal is running as a member
    /// of a named team. The lead's `team_id` is its own team's
    /// id; ordinary sub-agents stay `None`.
    pub team_id: Option<String>,
    /// Phase 79.6 — human-readable member name within
    /// `team_id` (e.g. `"researcher"`). `None` ⇔ `team_id.is_none()`.
    /// `Some(TEAM_LEAD_NAME)` for the lead's own goal.
    pub team_member_name: Option<String>,
    /// Phase 79.6 — DMs the team router delivered while this
    /// goal was running. Consumed at the start of each turn by
    /// the prompt-assembly path. Concurrent appends are
    /// serialised by the goal's tokio task scheduler — there is
    /// no inner lock because the consume is single-threaded
    /// per-goal.
    pub inbox: Arc<RwLock<Vec<DmMessage>>>,
    /// Phase 77.20 — whether this goal runs in proactive tick-loop mode.
    /// Set at goal spawn from `EffectiveBindingPolicy::proactive().enabled`.
    /// Read by `llm_behavior` to inject the proactive system hint.
    pub proactive_enabled: bool,
    /// Phase 77.20 — binding role tag (`"coordinator"`, `"worker"`, `"proactive"`,
    /// or `None`). Stored here so `llm_behavior` can inject the coordinator
    /// hint without re-reading the binding config on every turn.
    pub binding_role: Option<String>,
    /// Phase 80.15 — boot-resolved assistant-mode view. Read by
    /// downstream consumers (driver-loop tick generator, cron default
    /// flip, brief mode auto-on, dream-context kairos signal,
    /// remote-control auto-tier in Phase 80.17). The `enabled` flag
    /// is boot-immutable; the addendum text inside it can be
    /// hot-reloaded through the Phase 18 path. `Default::default()`
    /// is the zero-cost disabled view — fixtures and bootstrap
    /// contexts can rely on it without opting in.
    #[doc(hidden)]
    pub assistant: nexo_assistant::ResolvedAssistant,
    /// Phase 82.1 Step 3 — composed binding context propagated
    /// to tool calls via `_meta.nexo.binding`. `Some` when
    /// intake matched an `InboundBinding`; `None` for
    /// bindingless paths (delegation receive, heartbeat
    /// bootstrap, tests).
    ///
    /// Construct via `BindingContext::from_effective(&policy,
    /// agent_id, session_id)` at the intake site that matches
    /// the binding (Step 4). Tool dispatch reads this through
    /// `inject_context_meta` to populate the JSON-RPC
    /// `params._nexo_context` block (Step 5).
    pub binding: Option<BindingContext>,
}

/// One inbound team message attached to a goal's `AgentContext.inbox`.
/// Mirror of [`crate::team_message_router::DmFrame`] minus the wire
/// fields the call site already knows (`team_id`, `to`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct DmMessage {
    pub from: String,
    pub body: serde_json::Value,
    pub correlation_id: Option<String>,
    pub received_at: i64,
}

/// Phase 82.1 — binding context propagated to tool calls so
/// extensions and MCP servers can route per-(channel, account_id,
/// agent_id) tuple without re-deriving it from each tool call's
/// payload.
///
/// Serialised under `_meta.nexo.binding` in JSON-RPC `tools/call`
/// (extensions ignore unknown fields) and as the `meta` block of
/// MCP `call_tool_with_meta`.
///
/// `agent_id` is mandatory; the rest are `Option` because some
/// dispatch paths (delegation receive, heartbeat bootstrap, tests)
/// have no binding match — `None` is the correct state, not a
/// sentinel string.
///
/// `mcp_channel_source` is populated when the inbound that
/// triggered this turn arrived via a Phase 80.9 MCP channel
/// server (e.g., `"slack"`, `"telegram"`). Lets a tool
/// distinguish "telegram-binding answered via MCP slack server"
/// from "telegram-binding answered via native Telegram plugin"
/// while still seeing the same `(channel, account_id)` binding
/// tuple. Matches the `goal_turns.source = "channel:slack"`
/// audit column shipped with Phase 80.9.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BindingContext {
    /// Stable agent identifier (`agents.yaml.<id>`).
    pub agent_id: String,

    /// Active session UUID. `None` outside an LLM turn (heartbeat
    /// bootstrap, delegation receive, tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,

    /// Channel name as declared in `InboundBinding.plugin`
    /// (`"whatsapp"`, `"telegram"`, `"email"`, `"web"`, …).
    /// `None` for contexts without a binding match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,

    /// Account / instance discriminator from
    /// `InboundBinding.instance`. `None` when the binding
    /// declared no instance (single-account default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,

    /// Stable binding identifier rendered as
    /// `<channel>:<account_id|"default">`. Survives
    /// `agents.yaml` reloads (does NOT depend on the binding
    /// vector index). `None` for bindingless contexts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,

    /// Phase 80.9 — MCP channel server name when the inbound
    /// arrived via `notifications/nexo/channel`. `None` for
    /// native-channel inbounds. Examples: `"slack"`,
    /// `"telegram"`, `"imessage"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_channel_source: Option<String>,
}

impl BindingContext {
    /// Pure-fn constructor from an already-resolved Phase 16
    /// binding policy + agent / session identity.
    ///
    /// When the policy has no `binding_index` (synthesised by
    /// `EffectiveBindingPolicy::from_agent_defaults` for
    /// delegation / heartbeat / tests), the `(channel,
    /// account_id, binding_id)` tuple stays `None`. Only
    /// `agent_id` + `session_id` carry through.
    ///
    /// `mcp_channel_source` is propagated separately by the
    /// intake site that received a Phase 80.9 MCP-channel
    /// inbound. This constructor never infers it from the
    /// policy alone; callers chain `.with_mcp_channel_source(s)`
    /// when applicable.
    pub fn from_effective(
        policy: &EffectiveBindingPolicy,
        agent_id: impl Into<String>,
        session_id: Option<Uuid>,
    ) -> Self {
        let (channel, account_id, binding_id) = match policy.binding_index {
            Some(_) => (
                policy.channel.clone(),
                policy.account_id.clone(),
                policy.binding_id(),
            ),
            None => (None, None, None),
        };
        Self {
            agent_id: agent_id.into(),
            session_id,
            channel,
            account_id,
            binding_id,
            mcp_channel_source: None,
        }
    }

    /// Agent-only minimal context for paths that have neither
    /// a binding match nor a session (e.g., bootstrap
    /// validation, unit tests).
    pub fn agent_only(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: None,
            channel: None,
            account_id: None,
            binding_id: None,
            mcp_channel_source: None,
        }
    }

    /// Render the stable binding id from `(channel, account_id)`.
    /// Returns `<channel>:<account_id|"default">`. Used at the
    /// `from_effective` call site and exposed for tests /
    /// downstream consumers that need to compute the same id
    /// without holding a policy reference.
    pub fn render_binding_id(channel: &str, account_id: Option<&str>) -> String {
        format!("{}:{}", channel, account_id.unwrap_or("default"))
    }

    /// Builder for the MCP channel source (Phase 80.9). Returns
    /// `self` so it composes inline with `from_effective`.
    #[must_use]
    pub fn with_mcp_channel_source(
        mut self,
        source: impl Into<String>,
    ) -> Self {
        self.mcp_channel_source = Some(source.into());
        self
    }
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
            team_id: None,
            team_member_name: None,
            inbox: Arc::new(RwLock::new(Vec::new())),
            proactive_enabled: false,
            binding_role: None,
            assistant: nexo_assistant::ResolvedAssistant::disabled(),
            repl_registry: None,
            // Phase 82.1 Step 3 — `None` is the default for
            // `AgentContext::new`. Intake sites that match an
            // inbound to an `InboundBinding` populate this via
            // `BindingContext::from_effective(&policy, agent_id,
            // session_id)` (Step 4). Bindingless paths
            // (delegation receive, heartbeat bootstrap, tests)
            // keep `None`.
            binding: None,
        }
    }

    /// Phase 79.6 — mark this context as running as a teammate.
    /// `name` is the human-readable handle within the team
    /// (`"researcher"`, `"tester"`, or `TEAM_LEAD_NAME`).
    pub fn with_team(mut self, team_id: impl Into<String>, name: impl Into<String>) -> Self {
        self.team_id = Some(team_id.into());
        self.team_member_name = Some(name.into());
        self
    }

    /// Phase 79.6 — `true` when both `team_id` and
    /// `team_member_name` are set. The runtime's
    /// teammate-cannot-spawn-teammate guard inspects this.
    pub fn is_teammate(&self) -> bool {
        self.team_id.is_some() && self.team_member_name.is_some()
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
        self.proactive_enabled = effective.proactive.enabled;
        self.binding_role = effective.role.clone();
        // Phase 82.1 Step 4 — populate the BindingContext as a
        // side effect of installing the policy. Every intake
        // path that resolves an inbound to an `InboundBinding`
        // funnels through `with_effective`, so this single call
        // site is sufficient — no need to chase N intake-side
        // call paths individually. Bindingless paths
        // (delegation receive / heartbeat bootstrap / tests)
        // never call `with_effective` and therefore keep
        // `binding == None`. `mcp_channel_source` stays None
        // here; it is layered on top by the channel-aware
        // intake site that received the Phase 80.9 MCP-channel
        // inbound (`with_mcp_channel_source` chained after).
        self.binding = Some(BindingContext::from_effective(
            &effective,
            self.agent_id.clone(),
            self.session_id,
        ));
        self.effective = Some(effective);
        self
    }

    /// Phase 82.1 Step 4 — layer the Phase 80.9 MCP channel
    /// source on top of the BindingContext after
    /// `with_effective` has run. No-op if `binding` is `None`
    /// (paths without a binding match cannot have an
    /// MCP-channel source — the source rides alongside an
    /// already-matched binding, not as a substitute).
    pub fn with_mcp_channel_source(
        mut self,
        source: impl Into<String>,
    ) -> Self {
        if let Some(b) = self.binding.as_mut() {
            b.mcp_channel_source = Some(source.into());
        }
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

    // -----------------------------------------------------------
    // Phase 79.6 — team fields
    // -----------------------------------------------------------

    #[tokio::test]
    async fn team_fields_default_to_none() {
        let c = ctx();
        assert!(c.team_id.is_none());
        assert!(c.team_member_name.is_none());
        assert!(!c.is_teammate());
        assert!(c.inbox.read().await.is_empty());
    }

    #[tokio::test]
    async fn with_team_sets_both_fields() {
        let c = ctx().with_team("feature-x", "researcher");
        assert_eq!(c.team_id.as_deref(), Some("feature-x"));
        assert_eq!(c.team_member_name.as_deref(), Some("researcher"));
        assert!(c.is_teammate());
    }

    #[tokio::test]
    async fn dm_message_serde_roundtrip() {
        let m = DmMessage {
            from: "team-lead".into(),
            body: serde_json::json!({"hi": 1}),
            correlation_id: Some("c-1".into()),
            received_at: 100,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: DmMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[tokio::test]
    async fn inbox_appends_persist_across_clones() {
        // Inbox is `Arc<RwLock<Vec<DmMessage>>>` so two refs
        // to the same context share the queue.
        let c = ctx().with_team("feature-x", "researcher");
        c.inbox.write().await.push(DmMessage {
            from: "team-lead".into(),
            body: serde_json::json!("hi"),
            correlation_id: None,
            received_at: 1,
        });
        let same = c.clone();
        assert_eq!(same.inbox.read().await.len(), 1);
    }

    // -----------------------------------------------------------
    // Phase 82.1 Step 4 — `with_effective` populates `binding`
    // -----------------------------------------------------------

    #[tokio::test]
    async fn binding_is_none_before_with_effective() {
        let c = ctx();
        assert!(c.binding.is_none());
    }

    #[tokio::test]
    async fn with_effective_populates_binding_from_policy() {
        use nexo_config::types::agents::InboundBinding;

        let mut a = (*ctx().config).clone();
        a.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            instance: Some("personal".into()),
            ..Default::default()
        });
        let policy = Arc::new(EffectiveBindingPolicy::resolve(&a, 0));

        let c = ctx().with_effective(policy);
        let b = c.binding.expect("binding populated by with_effective");
        assert_eq!(b.agent_id, "a"); // ctx() helper uses agent id "a"
        assert_eq!(b.channel.as_deref(), Some("whatsapp"));
        assert_eq!(b.account_id.as_deref(), Some("personal"));
        assert_eq!(b.binding_id.as_deref(), Some("whatsapp:personal"));
        assert!(b.mcp_channel_source.is_none());
    }

    #[tokio::test]
    async fn with_mcp_channel_source_layers_on_top_of_with_effective() {
        use nexo_config::types::agents::InboundBinding;

        let mut a = (*ctx().config).clone();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("kate_tg".into()),
            ..Default::default()
        });
        let policy = Arc::new(EffectiveBindingPolicy::resolve(&a, 0));

        let c = ctx().with_effective(policy).with_mcp_channel_source("slack");
        let b = c.binding.expect("binding populated");
        // Native binding tuple from policy
        assert_eq!(b.channel.as_deref(), Some("telegram"));
        assert_eq!(b.account_id.as_deref(), Some("kate_tg"));
        // Phase 80.9 source layered on top
        assert_eq!(b.mcp_channel_source.as_deref(), Some("slack"));
    }

    #[tokio::test]
    async fn with_mcp_channel_source_no_op_when_no_binding_match() {
        // No `with_effective` called → binding stays None →
        // `with_mcp_channel_source` is a no-op (mcp_channel_source
        // rides alongside an already-matched binding, never as a
        // substitute).
        let c = ctx().with_mcp_channel_source("slack");
        assert!(c.binding.is_none());
    }
}

#[cfg(test)]
mod binding_context_tests {
    //! Phase 82.1 Step 1 tests — `BindingContext` struct +
    //! standalone helpers. `from_effective` (which closes the
    //! loop with `EffectiveBindingPolicy`) lands at Step 3 once
    //! Step 2 extends the policy struct.

    use super::BindingContext;
    use uuid::Uuid;

    #[test]
    fn agent_only_minimal_context_clears_binding_fields() {
        let ctx = BindingContext::agent_only("ana");
        assert_eq!(ctx.agent_id, "ana");
        assert!(ctx.session_id.is_none());
        assert!(ctx.channel.is_none());
        assert!(ctx.account_id.is_none());
        assert!(ctx.binding_id.is_none());
        assert!(ctx.mcp_channel_source.is_none());
    }

    #[test]
    fn render_binding_id_with_account_id_renders_channel_colon_account() {
        assert_eq!(
            BindingContext::render_binding_id("whatsapp", Some("personal")),
            "whatsapp:personal"
        );
        assert_eq!(
            BindingContext::render_binding_id("telegram", Some("kate_tg")),
            "telegram:kate_tg"
        );
    }

    #[test]
    fn render_binding_id_without_account_id_uses_default_sentinel() {
        assert_eq!(
            BindingContext::render_binding_id("whatsapp", None),
            "whatsapp:default"
        );
    }

    #[test]
    fn with_mcp_channel_source_sets_field_inline() {
        let ctx = BindingContext::agent_only("ana").with_mcp_channel_source("slack");
        assert_eq!(ctx.mcp_channel_source.as_deref(), Some("slack"));
        assert_eq!(ctx.agent_id, "ana");
    }

    #[test]
    fn binding_context_is_clone_eq_serializable() {
        let ctx = BindingContext {
            agent_id: "ana".into(),
            session_id: Some(Uuid::nil()),
            channel: Some("whatsapp".into()),
            account_id: Some("personal".into()),
            binding_id: Some("whatsapp:personal".into()),
            mcp_channel_source: Some("slack".into()),
        };
        let cloned = ctx.clone();
        assert_eq!(ctx, cloned);
        let json = serde_json::to_value(&ctx).unwrap();
        assert_eq!(json["agent_id"], "ana");
        assert_eq!(json["channel"], "whatsapp");
        assert_eq!(json["account_id"], "personal");
        assert_eq!(json["binding_id"], "whatsapp:personal");
        assert_eq!(json["mcp_channel_source"], "slack");
    }

    #[test]
    fn binding_context_skips_serializing_none_fields() {
        let ctx = BindingContext::agent_only("ana");
        let json = serde_json::to_value(&ctx).unwrap();
        let obj = json.as_object().expect("expected object");
        assert!(obj.contains_key("agent_id"));
        // None fields skipped per #[serde(skip_serializing_if = "Option::is_none")]
        assert!(!obj.contains_key("session_id"));
        assert!(!obj.contains_key("channel"));
        assert!(!obj.contains_key("account_id"));
        assert!(!obj.contains_key("binding_id"));
        assert!(!obj.contains_key("mcp_channel_source"));
    }

    #[test]
    fn binding_context_round_trips_through_serde() {
        let ctx = BindingContext {
            agent_id: "carlos".into(),
            session_id: Some(Uuid::from_u128(42)),
            channel: Some("whatsapp".into()),
            account_id: Some("business".into()),
            binding_id: Some("whatsapp:business".into()),
            mcp_channel_source: None,
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: BindingContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
    }

    // -- Phase 82.1 Step 3 — from_effective constructor --

    fn mini_agent() -> nexo_config::types::agents::AgentConfig {
        use nexo_config::types::agents::{
            AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
            OutboundAllowlistConfig, WorkspaceGitConfig,
        };
        AgentConfig {
            id: "ana".into(),
            model: ModelConfig {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: String::new(),
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
        }
    }

    #[test]
    fn from_effective_with_matched_binding_populates_tuple() {
        use super::EffectiveBindingPolicy;
        use nexo_config::types::agents::InboundBinding;

        let mut a = mini_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            instance: Some("personal".into()),
            ..Default::default()
        });
        let policy = EffectiveBindingPolicy::resolve(&a, 0);
        let ctx = BindingContext::from_effective(&policy, "ana", Some(Uuid::from_u128(1)));

        assert_eq!(ctx.agent_id, "ana");
        assert_eq!(ctx.session_id, Some(Uuid::from_u128(1)));
        assert_eq!(ctx.channel.as_deref(), Some("whatsapp"));
        assert_eq!(ctx.account_id.as_deref(), Some("personal"));
        assert_eq!(ctx.binding_id.as_deref(), Some("whatsapp:personal"));
        assert!(ctx.mcp_channel_source.is_none());
    }

    #[test]
    fn from_effective_with_synthesised_policy_keeps_tuple_none() {
        use super::EffectiveBindingPolicy;

        let a = mini_agent();
        let policy = EffectiveBindingPolicy::from_agent_defaults(&a);
        let ctx = BindingContext::from_effective(&policy, "delegation", None);

        assert_eq!(ctx.agent_id, "delegation");
        assert!(ctx.session_id.is_none());
        assert!(ctx.channel.is_none());
        assert!(ctx.account_id.is_none());
        assert!(ctx.binding_id.is_none());
        assert!(ctx.mcp_channel_source.is_none());
    }

    #[test]
    fn from_effective_chains_with_mcp_channel_source() {
        use super::EffectiveBindingPolicy;
        use nexo_config::types::agents::InboundBinding;

        let mut a = mini_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "telegram".into(),
            instance: Some("kate_tg".into()),
            ..Default::default()
        });
        let policy = EffectiveBindingPolicy::resolve(&a, 0);
        let ctx = BindingContext::from_effective(&policy, "ana", None)
            .with_mcp_channel_source("slack");

        // Native binding tuple stays from policy.
        assert_eq!(ctx.channel.as_deref(), Some("telegram"));
        assert_eq!(ctx.account_id.as_deref(), Some("kate_tg"));
        // MCP source layered on top.
        assert_eq!(ctx.mcp_channel_source.as_deref(), Some("slack"));
    }

    #[test]
    fn from_effective_two_personas_get_distinct_binding_ids() {
        use super::EffectiveBindingPolicy;
        use nexo_config::types::agents::InboundBinding;

        let mut a = mini_agent();
        a.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            instance: Some("personal".into()),
            ..Default::default()
        });
        a.inbound_bindings.push(InboundBinding {
            plugin: "whatsapp".into(),
            instance: Some("business".into()),
            ..Default::default()
        });
        let p0 = EffectiveBindingPolicy::resolve(&a, 0);
        let p1 = EffectiveBindingPolicy::resolve(&a, 1);
        let c0 = BindingContext::from_effective(&p0, "ana", None);
        let c1 = BindingContext::from_effective(&p1, "carlos", None);

        assert_eq!(c0.binding_id.as_deref(), Some("whatsapp:personal"));
        assert_eq!(c1.binding_id.as_deref(), Some("whatsapp:business"));
        assert_ne!(c0.binding_id, c1.binding_id);
    }
}
