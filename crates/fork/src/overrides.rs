//! `ForkOverrides` — selective overrides applied to the parent
//! [`AgentContext`] clone for a fork. Step 80.19 / 4.
//!
//! Most state stays shared via `Arc<...>` — Rust's ownership model
//! already isolates by construction, unlike KAIROS's TypeScript path
//! that needed deep clones for everything (leak `forkedAgent.ts:345-462`).
//!
//! The fields below are the ones whose isolation actually matters in
//! nexo's architecture. Abort signal + tool filter live in
//! [`crate::ForkParams`] directly because they are *fork-loop* concerns,
//! not [`AgentContext`] concerns (the context doesn't carry either).

use nexo_core::agent::AgentContext;

/// Optional overrides for [`create_fork_context`].
#[derive(Default, Clone)]
pub struct ForkOverrides {
    /// Override the agent_id stamped into events emitted by tools the
    /// fork invokes. Default: parent's agent_id.
    pub agent_id: Option<String>,

    /// Critical system reminder injected on every turn. Mirrors
    /// `criticalSystemReminder_EXPERIMENTAL` (leak `:316-318`).
    /// Read by [`crate::run_turn_loop`] from [`crate::ForkParams`] —
    /// stored here so callers can pass it via overrides for symmetry.
    pub critical_system_reminder: Option<String>,
}

/// Build an isolated clone of the parent's [`AgentContext`] with
/// overrides applied. All `Arc` fields keep their refcount — the fork
/// shares the parent's memory store, MCP runtime, broker, etc.
///
/// This is the Rust analogue of `createSubagentContext` (leak `:345-462`)
/// minus the 17 fields whose isolation TypeScript needed but Rust doesn't:
/// no [`tokio::sync::RwLock`] readers leak between threads, no shared
/// mutable closures, no `setAppState` callback hand-off — Rust enforces
/// these invariants at compile time.
pub fn create_fork_context(parent: &AgentContext, overrides: ForkOverrides) -> AgentContext {
    let mut ctx = parent.clone();
    if let Some(id) = overrides.agent_id {
        ctx.agent_id = id;
    }
    // critical_system_reminder is consumed by `run_turn_loop`, not by
    // AgentContext itself — it has no field for it. We accept the value
    // here for ergonomic parity with KAIROS's overrides struct; ForkParams
    // also carries it so consumers can decide where to thread it.
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use nexo_core::session::SessionManager;
    use std::sync::Arc;

    fn mk_parent() -> AgentContext {
        // Mirror crates/core/src/agent/context.rs::plan_mode_tests::ctx —
        // building AgentConfig manually because there is no Default impl
        // (each field is meaningful and tests need explicit values).
        let cfg = AgentConfig {
            id: "parent_agent".into(),
            model: ModelConfig {
                provider: "test".into(),
                model: "test-model".into(),
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
            extensions_config: std::collections::BTreeMap::new(),
        };
        AgentContext::new(
            "parent_agent",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    #[tokio::test]
    async fn create_clone_preserves_agent_id_when_no_override() {
        let parent = mk_parent();
        let fork = create_fork_context(&parent, ForkOverrides::default());
        assert_eq!(fork.agent_id, "parent_agent");
    }

    #[tokio::test]
    async fn create_overrides_agent_id() {
        let parent = mk_parent();
        let fork = create_fork_context(
            &parent,
            ForkOverrides {
                agent_id: Some("forked_dream".to_string()),
                critical_system_reminder: None,
            },
        );
        assert_eq!(fork.agent_id, "forked_dream");
        // Parent untouched
        assert_eq!(parent.agent_id, "parent_agent");
    }

    #[tokio::test]
    async fn create_clone_preserves_config_arc_reference() {
        let parent = mk_parent();
        let parent_cfg_ptr = Arc::as_ptr(&parent.config) as usize;
        let fork = create_fork_context(&parent, ForkOverrides::default());
        let fork_cfg_ptr = Arc::as_ptr(&fork.config) as usize;
        assert_eq!(parent_cfg_ptr, fork_cfg_ptr, "config must be Arc-shared");
    }

    #[tokio::test]
    async fn create_clone_preserves_session_manager_arc_reference() {
        let parent = mk_parent();
        let parent_sm_ptr = Arc::as_ptr(&parent.sessions) as usize;
        let fork = create_fork_context(&parent, ForkOverrides::default());
        let fork_sm_ptr = Arc::as_ptr(&fork.sessions) as usize;
        assert_eq!(parent_sm_ptr, fork_sm_ptr, "sessions must be Arc-shared");
    }
}
