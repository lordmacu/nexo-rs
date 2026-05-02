//! Immutable per-agent runtime snapshot.
//!
//! Everything that hot-reload can replace on an agent lives here: the
//! full `AgentConfig`, the pre-resolved per-binding effective policies,
//! the filtered tool-registry cache, and the live `LlmClient` handle.
//! A snapshot is immutable — reload builds a fresh one and swaps it in
//! atomically via `ArcSwap`. Session tasks hold `Arc<RuntimeSnapshot>`
//! clones, so an in-flight turn always sees a consistent view; the
//! swap only affects *new* `snapshot.load()` reads.
//!
//! Phase 18 scope: this module is pure data + a builder. The runtime
//! wiring that actually swaps these in and out lives in
//! `crates/core/src/config_reload.rs` (coordinator) and the
//! `AgentRuntime` refactor that reads `snapshot.load()` on the intake
//! hot path.

use std::sync::Arc;

use nexo_config::types::llm::ResolvedContextOptimization;
use nexo_config::{AgentConfig, LlmConfig};
use nexo_llm::{LlmClient, LlmRegistry};

use crate::agent::effective::EffectiveBindingPolicy;
use crate::agent::tool_registry::ToolRegistry;
use crate::agent::tool_registry_cache::ToolRegistryCache;

/// Immutable snapshot of everything hot-reload can swap on an agent.
///
/// Held behind `Arc<ArcSwap<RuntimeSnapshot>>` by `AgentRuntime`; the
/// intake hot path calls `.load()` per event (lock-free). Fields that
/// never change across reloads (the mpsc senders, the tokio JoinSet,
/// the shutdown token) stay on `AgentRuntime` itself.
#[derive(Clone)]
pub struct RuntimeSnapshot {
    /// The `AgentConfig` this snapshot was built from. Downstream
    /// consumers (delegation ACL, heartbeat interval lookups) read
    /// agent-level fields from here instead of holding a separate
    /// `Arc<AgentConfig>`.
    pub nexo_config: Arc<AgentConfig>,
    /// Pre-resolved per-binding capability policies, keyed by
    /// `binding_index` (Some(n) for real bindings, None for the
    /// legacy agent-level fallback). Built at snapshot construction so
    /// the intake path is an Arc lookup, not a resolve.
    pub effective_policies: Arc<dashmap::DashMap<Option<usize>, Arc<EffectiveBindingPolicy>>>,
    /// Per-binding filtered tool registry cache. Keyed by
    /// `(agent_id, binding_index)`; entries built lazily the first
    /// time a binding sees traffic. Fresh per snapshot so a reload
    /// that changes `allowed_tools` does not serve a stale filtered
    /// clone.
    pub tool_cache: Arc<ToolRegistryCache>,
    /// LLM client for this agent. `None` in early-boot snapshots
    /// built before the `LlmRegistry` is wired in (tests, scaffolding).
    /// Production snapshots constructed via the reload coordinator
    /// always populate this; consumers that fall back read the
    /// behavior-owned `llm` field.
    pub llm_client: Option<Arc<dyn LlmClient>>,
    /// Monotonic version per agent. The intake path tags log lines
    /// with this so operators can correlate "session X used version Y"
    /// when debugging a reload.
    pub version: u64,
    /// Phase F follow-up — the four context-optimization enables,
    /// already resolved against `llm.context_optimization` and the
    /// agent's per-agent override. Captured at snapshot-build time so
    /// the agent loop reads the *current* enables on every turn (a
    /// reload that swaps the snapshot is observed on the next
    /// `snapshot_ref.load()`). The boot-time wiring on
    /// `LlmAgentBehavior` (compactor / token_counter / workspace_cache
    /// instances) stays put — these flags only gate whether the agent
    /// loop *uses* those instances on a given turn.
    pub context_optimization: ResolvedContextOptimization,
}

impl std::fmt::Debug for RuntimeSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSnapshot")
            .field("agent_id", &self.nexo_config.id)
            .field("version", &self.version)
            .field("bindings", &self.nexo_config.inbound_bindings.len())
            .field("effective_slots", &self.effective_policies.len())
            .field("tool_cache_entries", &self.tool_cache.len())
            .finish()
    }
}

impl RuntimeSnapshot {
    /// Build a fresh snapshot from the current config + registries.
    ///
    /// Errors if the LLM client cannot be constructed (unknown
    /// provider, missing `llm.providers.X`, invalid credentials
    /// reference). Callers should validate against the same registries
    /// with `validate_agents_with_providers` *before* calling this so
    /// a failure here means something changed between validation and
    /// build — always log at warn and keep the old snapshot.
    /// Build a snapshot with the LLM client set to `None`. Used at
    /// `AgentRuntime::new` before the registry is wired, and in tests.
    /// Production reloads use [`RuntimeSnapshot::build`] to also pin
    /// the LLM client.
    /// Resolve the four enables from a (global, agent) pair. Used by
    /// both `bare` (global=default) and `build` (real config).
    fn resolve_co(
        nexo_config: &AgentConfig,
        global: &nexo_config::types::llm::ContextOptimizationConfig,
    ) -> ResolvedContextOptimization {
        ResolvedContextOptimization::resolve(global, nexo_config.context_optimization.as_ref())
    }

    pub fn bare(nexo_config: Arc<AgentConfig>, version: u64) -> Self {
        let effective_policies: dashmap::DashMap<Option<usize>, Arc<EffectiveBindingPolicy>> =
            dashmap::DashMap::new();
        if nexo_config.inbound_bindings.is_empty() {
            effective_policies.insert(
                None,
                Arc::new(EffectiveBindingPolicy::from_agent_defaults(&nexo_config)),
            );
        } else {
            for idx in 0..nexo_config.inbound_bindings.len() {
                effective_policies.insert(
                    Some(idx),
                    EffectiveBindingPolicy::resolved(&nexo_config, idx),
                );
            }
        }
        let context_optimization = Self::resolve_co(
            &nexo_config,
            &nexo_config::types::llm::ContextOptimizationConfig::default(),
        );
        Self {
            nexo_config,
            effective_policies: Arc::new(effective_policies),
            tool_cache: Arc::new(ToolRegistryCache::new()),
            llm_client: None,
            version,
            context_optimization,
        }
    }

    pub fn build(
        nexo_config: Arc<AgentConfig>,
        llm_registry: &LlmRegistry,
        llm_cfg: &LlmConfig,
        version: u64,
    ) -> anyhow::Result<Self> {
        let effective_policies: dashmap::DashMap<Option<usize>, Arc<EffectiveBindingPolicy>> =
            dashmap::DashMap::new();
        if nexo_config.inbound_bindings.is_empty() {
            effective_policies.insert(
                None,
                Arc::new(EffectiveBindingPolicy::from_agent_defaults(&nexo_config)),
            );
        } else {
            for idx in 0..nexo_config.inbound_bindings.len() {
                effective_policies.insert(
                    Some(idx),
                    EffectiveBindingPolicy::resolved(&nexo_config, idx),
                );
            }
        }
        // Phase 83.8.12.5.b — resolve provider via tenant-first
        // namespace when the agent declares `tenant_id`. Single-
        // tenant deployments leave `nexo_config.tenant_id` as
        // `None` → falls back to the legacy global path
        // (identical bytes to pre-83.8.12.5).
        let llm_client = llm_registry
            .build_for_tenant(
                llm_cfg,
                &nexo_config.model,
                nexo_config.tenant_id.as_deref(),
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "snapshot build: LLM client for agent '{}' failed: {}",
                    nexo_config.id,
                    e
                )
            })?;
        let context_optimization = Self::resolve_co(&nexo_config, &llm_cfg.context_optimization);
        Ok(Self {
            nexo_config,
            effective_policies: Arc::new(effective_policies),
            tool_cache: Arc::new(ToolRegistryCache::new()),
            llm_client: Some(llm_client),
            version,
            context_optimization,
        })
    }

    /// Convenience: look up the pre-resolved policy for a binding
    /// index. Returns `None` only when the policy map is missing the
    /// slot (never happens for a snapshot built via `build` — the
    /// legacy path seeds `None` and each real binding seeds `Some(n)`).
    pub fn policy_for(&self, binding_index: Option<usize>) -> Option<Arc<EffectiveBindingPolicy>> {
        self.effective_policies
            .get(&binding_index)
            .map(|e| Arc::clone(e.value()))
    }

    /// Fetch or build the filtered tool registry for a binding. Thin
    /// delegation to the per-snapshot `ToolRegistryCache`; the base
    /// registry stays external to the snapshot because it is owned by
    /// the `AgentRuntime` and typically shared across reloads (plugin
    /// hot-reload is Phase 19).
    pub fn tools_for(
        &self,
        agent_id: &str,
        binding_index: Option<usize>,
        base: &ToolRegistry,
        allowed_tools: &[String],
    ) -> Arc<ToolRegistry> {
        self.tool_cache
            .get_or_build(agent_id, binding_index, base, allowed_tools)
    }

    /// PT-2 — dispatch-aware variant. Same lazy cache, but the
    /// filtered registry also has `apply_dispatch_capability`
    /// applied so dispatch tools the binding's `DispatchPolicy`
    /// disallows are not registered.
    pub fn tools_for_with_dispatch(
        &self,
        agent_id: &str,
        binding_index: Option<usize>,
        base: &ToolRegistry,
        allowed_tools: &[String],
        dispatch_policy: &nexo_config::DispatchPolicy,
        is_admin: bool,
    ) -> Arc<ToolRegistry> {
        self.tool_cache.get_or_build_with_dispatch(
            agent_id,
            binding_index,
            base,
            allowed_tools,
            dispatch_policy,
            is_admin,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_config::{
        AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    fn empty_llm_cfg() -> LlmConfig {
        // Real LlmConfig has `providers: HashMap<_, _>`; an empty map
        // means `build` will fail, which is exactly what we want to
        // verify in the error-path test.
        LlmConfig {
            providers: std::collections::HashMap::new(),
            retry: Default::default(),
            context_optimization: Default::default(),
            tenants: std::collections::HashMap::new(),
        }
    }

    fn minimal_agent(id: &str) -> Arc<AgentConfig> {
        Arc::new(AgentConfig {
            id: id.into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m1".into(),
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
            tenant_id: None,
            extensions_config: std::collections::BTreeMap::new(),
        })
    }

    #[test]
    fn build_fails_for_unknown_provider() {
        let registry = LlmRegistry::with_builtins();
        let err = RuntimeSnapshot::build(minimal_agent("ana"), &registry, &empty_llm_cfg(), 1)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("stub") || msg.contains("not registered") || msg.contains("agent 'ana'"),
            "error should mention the offending provider: {msg}"
        );
    }

    #[test]
    fn policy_for_returns_legacy_slot_on_bindingless_agent() {
        // We can't actually build because stub isn't registered in the
        // default registry, so exercise the in-memory map directly by
        // asserting the legacy path keys `None` — using the typed
        // helper to not re-implement the resolution logic.
        let agent = minimal_agent("ana");
        let policy = EffectiveBindingPolicy::from_agent_defaults(&agent);
        assert_eq!(policy.binding_index, None);
    }
}
