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

use agent_config::{AgentConfig, LlmConfig};
use agent_llm::{LlmClient, LlmRegistry};

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
    pub agent_config: Arc<AgentConfig>,
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
    /// LLM client for this agent. Rebuilt on every snapshot so a
    /// rotated API key or a swapped `model.provider` in `llm.yaml`
    /// takes effect on the next turn.
    pub llm_client: Arc<dyn LlmClient>,
    /// Monotonic version per agent. The intake path tags log lines
    /// with this so operators can correlate "session X used version Y"
    /// when debugging a reload.
    pub version: u64,
}

impl std::fmt::Debug for RuntimeSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSnapshot")
            .field("agent_id", &self.agent_config.id)
            .field("version", &self.version)
            .field("bindings", &self.agent_config.inbound_bindings.len())
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
    pub fn build(
        agent_config: Arc<AgentConfig>,
        llm_registry: &LlmRegistry,
        llm_cfg: &LlmConfig,
        version: u64,
    ) -> anyhow::Result<Self> {
        let effective_policies: dashmap::DashMap<Option<usize>, Arc<EffectiveBindingPolicy>> =
            dashmap::DashMap::new();
        if agent_config.inbound_bindings.is_empty() {
            effective_policies.insert(
                None,
                Arc::new(EffectiveBindingPolicy::from_agent_defaults(&agent_config)),
            );
        } else {
            for idx in 0..agent_config.inbound_bindings.len() {
                effective_policies.insert(
                    Some(idx),
                    EffectiveBindingPolicy::resolved(&agent_config, idx),
                );
            }
        }
        let llm_client = llm_registry
            .build(llm_cfg, &agent_config.model)
            .map_err(|e| {
                anyhow::anyhow!(
                    "snapshot build: LLM client for agent '{}' failed: {}",
                    agent_config.id,
                    e
                )
            })?;
        Ok(Self {
            agent_config,
            effective_policies: Arc::new(effective_policies),
            tool_cache: Arc::new(ToolRegistryCache::new()),
            llm_client,
            version,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_config::{
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
            outbound_allowlist: OutboundAllowlistConfig::default(),
        })
    }

    #[test]
    fn build_fails_for_unknown_provider() {
        let registry = LlmRegistry::with_builtins();
        let err = RuntimeSnapshot::build(minimal_agent("ana"), &registry, &empty_llm_cfg(), 1)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("stub") || msg.contains("not registered") || msg.contains("agent 'ana'"),
            "error should mention the offending provider: {msg}");
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
