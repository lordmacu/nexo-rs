//! Boot-time helpers for wiring [`AutoDreamRunner`] into the daemon.
//! Phase 80.1.b.b.b.
//!
//! Operator code calls [`build_runner`] once at startup. Mirrors the
//! leak's `initAutoDream()` + `backgroundHousekeeping` setup pattern
//! (`claude-code-leak/src/services/autoDream/autoDream.ts:111-122`).
//!
//! # Provider-agnostic
//!
//! All inputs in [`BootDeps`] are trait objects (`Arc<dyn LlmClient>`,
//! `Arc<dyn ToolDispatcher>`) or plain data — no Anthropic-specific
//! types leak in. Works under any provider impl per memory rule
//! `feedback_provider_agnostic.md`.
//!
//! # main.rs hookup
//!
//! ```ignore
//! if let Some(ad_cfg) = &agent_cfg.auto_dream {
//!     let deps = nexo_dream::boot::BootDeps {
//!         config: ad_cfg.clone(),
//!         agent_id: agent_cfg.id.clone(),
//!         workspace_root: driver_cfg.workspace.root.clone(),
//!         state_root: state_root.clone(),
//!         parent_ctx_template: build_agent_ctx_template(
//!             agent_cfg, broker.clone(), sessions.clone(),
//!         ),
//!         llm: llm.clone(),
//!         tool_dispatcher: tool_dispatcher.clone(),
//!         fork_system_prompt: agent_cfg.system_prompt.clone(),
//!         fork_tools: Vec::new(), // MVP empty; AutoMemFilter restricts
//!         fork_model: agent_cfg.model.model.clone(),
//!     };
//!     if let Some(runner) = nexo_dream::boot::build_runner(deps).await? {
//!         orchestrator_builder = orchestrator_builder.auto_dream(runner);
//!     }
//! }
//! ```
//!
//! # MVP scope
//!
//! Single global runner per orchestrator. Multi-binding routing to
//! per-binding runners is a follow-up — current design ships ONE
//! runner that the orchestrator invokes per turn regardless of which
//! binding the goal belongs to.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use nexo_agent_registry::{DreamRunStore, SqliteDreamRunStore};
use nexo_fork::ToolDispatcher;
use nexo_llm::types::ToolDef;
use nexo_llm::LlmClient;

use crate::auto_dream::AutoDreamRunner;
use crate::config::{validate, AutoDreamConfig};
use crate::consolidation_lock::ConsolidationLock;
use crate::error::AutoDreamError;

/// All inputs needed to wire an [`AutoDreamRunner`] at boot.
///
/// Operator constructs this from agent config + existing per-binding
/// runtime (LlmClient, ToolDispatcher, AgentContext template).
pub struct BootDeps {
    /// Per-binding [`AutoDreamConfig`]. Validation runs at boot time.
    pub config: AutoDreamConfig,
    /// Logical agent identifier — stamped into telemetry events and
    /// used for the default `memory_dir` path segment.
    pub agent_id: String,
    /// Workspace root (e.g., `driver_cfg.workspace.root`). Used to
    /// build the default `memory_dir` when the operator did not
    /// explicitly set `config.memory_dir`.
    pub workspace_root: PathBuf,
    /// Daemon state root (e.g., `~/.local/share/nexo`). Used for the
    /// shared `dream_runs.db`.
    pub state_root: PathBuf,
    /// Parent [`AgentContext`] template, cloned per fork.
    /// Operator builds this with their existing builder (the same
    /// one used for normal turn ctx).
    pub parent_ctx_template: nexo_core::agent::AgentContext,
    /// LLM client for the fork's loop.
    pub llm: Arc<dyn LlmClient>,
    /// Tool dispatcher for the fork's tool calls.
    pub tool_dispatcher: Arc<dyn ToolDispatcher>,
    /// Fork-side system prompt. Per Phase 77.5 pattern: no parent
    /// prompt-cache share — fresh ChatRequest per fork.
    pub fork_system_prompt: String,
    /// Fork-side tool catalog. Empty acceptable for MVP — the
    /// `AutoMemFilter` (Phase 80.20) restricts what the fork can
    /// actually invoke regardless.
    pub fork_tools: Vec<ToolDef>,
    /// Fork-side model id (provider-agnostic — works for any
    /// `LlmClient` impl).
    pub fork_model: String,
    /// Phase 80.1.g — optional memory-checkpoint sink. When `Some`,
    /// the runner records each successful Completed fork-pass via
    /// the trait method (typically
    /// `nexo_core::agent::MemoryGitCheckpointer` wrapping the
    /// per-binding `MemoryGitRepo`). `None` disables. Skipped on
    /// empty `files_touched` regardless. Failure logs warn and does
    /// NOT downgrade the outcome.
    pub git_checkpointer:
        Option<Arc<dyn nexo_driver_types::MemoryCheckpointer>>,
    /// Phase 36.2 — optional pre-dream snapshot sink. When `Some`,
    /// the runner captures a `LocalFsSnapshotter` bundle (label
    /// `auto:pre-dream-<run_id>`) immediately before the fork-pass,
    /// so a corrupt dream can be reverted via `nexo memory restore`.
    /// Failure logs `tracing::warn!` and the dream proceeds without
    /// the rollback anchor — operators who want a hard gate enforce
    /// it at the boot wire (omit this hook until the snapshotter is
    /// healthy).
    pub pre_dream_snapshot:
        Option<Arc<dyn nexo_driver_types::PreDreamSnapshotHook>>,
    /// Tenant string passed to the pre-dream snapshot hook. Defaults
    /// to `"default"` for single-tenant deployments. Multi-tenant
    /// SaaS wires the per-binding tenant at boot.
    pub pre_dream_tenant: String,
}

/// Build an [`AutoDreamRunner`] ready to register on the
/// [`nexo_driver_loop::DriverOrchestratorBuilder`] via
/// `.auto_dream(...)`.
///
/// # Returns
///
/// - `Ok(None)` when `config.enabled == false`. Caller skips
///   registration so orchestrator stays without the runner — no
///   per-turn cost.
/// - `Ok(Some(runner))` when the runner is ready to register.
/// - `Err(AutoDreamError::Config)` when validation fails.
/// - `Err(AutoDreamError::Io)` when filesystem setup fails.
/// - `Err(AutoDreamError::Audit)` when SQLite open fails.
pub async fn build_runner(
    deps: BootDeps,
) -> Result<Option<Arc<AutoDreamRunner>>, AutoDreamError> {
    // Step 1 — validate config (fail-fast at boot).
    validate(&deps.config)?;

    // Step 2 — short-circuit when disabled.
    if !deps.config.enabled {
        tracing::debug!(
            target: "boot.auto_dream",
            agent = %deps.agent_id,
            "auto_dream config present but disabled"
        );
        return Ok(None);
    }

    // Step 3 — resolve memory_dir + mkdir.
    let memory_dir = deps
        .config
        .memory_dir
        .clone()
        .unwrap_or_else(|| default_memory_dir(&deps.workspace_root, &deps.agent_id));
    std::fs::create_dir_all(&memory_dir)?;

    // Step 4 — construct lock.
    let lock = Arc::new(ConsolidationLock::new(&memory_dir, deps.config.holder_stale)?);

    // Step 5 — open dream_runs DB (mkdir parent first).
    let dream_db = default_dream_db_path(&deps.state_root);
    if let Some(parent) = dream_db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dream_store: Arc<dyn DreamRunStore> = Arc::new(
        SqliteDreamRunStore::open(&dream_db.to_string_lossy())
            .await
            .map_err(|e| AutoDreamError::Audit(e.to_string()))?,
    );

    // Step 6 — construct runner.
    let runner_inner = AutoDreamRunner::with_default_fork(
        Arc::new(ArcSwap::from_pointee(deps.config)),
        lock,
        dream_store,
        deps.llm,
        deps.tool_dispatcher,
        memory_dir,
        deps.parent_ctx_template,
        deps.fork_system_prompt,
        deps.fork_tools,
        deps.fork_model,
    )?;
    // Phase 80.1.g — wire optional git checkpoint sink if operator
    // provided one. Per-binding decision; default is None (no commit).
    let runner_inner = match deps.git_checkpointer {
        Some(ckpt) => runner_inner.with_git_checkpointer(ckpt),
        None => runner_inner,
    };
    // Phase 36.2 — wire optional pre-dream snapshot sink.
    let runner_inner = match deps.pre_dream_snapshot {
        Some(hook) => runner_inner
            .with_pre_dream_snapshot(hook)
            .with_pre_dream_tenant(deps.pre_dream_tenant),
        None => runner_inner,
    };
    let runner = Arc::new(runner_inner);

    tracing::info!(
        target: "boot.auto_dream",
        agent = %deps.agent_id,
        git_checkpoint_wired = runner.has_git_checkpointer(),
        "auto_dream runner registered"
    );
    Ok(Some(runner))
}

/// Default memory dir per Phase 10.6 convention:
/// `<workspace_root>/.nexo-memory/<agent_id>`.
pub fn default_memory_dir(workspace_root: &Path, agent_id: &str) -> PathBuf {
    workspace_root.join(".nexo-memory").join(agent_id)
}

/// Default shared dream_runs DB:
/// `<state_root>/dream_runs.db`. Single open serves all bindings;
/// `goal_id` partitions naturally per Phase 80.18 schema.
pub fn default_dream_db_path(state_root: &Path) -> PathBuf {
    state_root.join("dream_runs.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use nexo_core::agent::AgentContext;
    use nexo_core::session::SessionManager;
    use nexo_llm::stream::StreamChunk;
    use nexo_llm::types::{ChatRequest, ChatResponse, FinishReason, ResponseContent, TokenUsage};
    use std::time::Duration;

    // ── Mock helpers (mirror crate-level auto_dream tests) ──

    fn mk_agent_ctx() -> AgentContext {
        let cfg = AgentConfig {
            id: "test_agent".into(),
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
            tenant_id: None,
            extensions_config: std::collections::BTreeMap::new(),
        };
        AgentContext::new(
            "test_agent",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(Duration::from_secs(60), 8)),
        )
    }

    struct MockLlm;
    #[async_trait]
    impl LlmClient for MockLlm {
        async fn chat(&self, _: ChatRequest) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                content: ResponseContent::Text("ok".into()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
        fn model_id(&self) -> &str {
            "test-model"
        }
        fn provider(&self) -> &str {
            "mock"
        }
        async fn stream<'a>(
            &'a self,
            _: ChatRequest,
        ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
            anyhow::bail!("stream not used")
        }
    }

    struct NoopDispatcher;
    #[async_trait]
    impl ToolDispatcher for NoopDispatcher {
        async fn dispatch(
            &self,
            _name: &str,
            _args: serde_json::Value,
        ) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn enabled_cfg() -> AutoDreamConfig {
        AutoDreamConfig {
            enabled: true,
            ..AutoDreamConfig::default()
        }
    }

    fn mk_deps(workspace_root: PathBuf, state_root: PathBuf, cfg: AutoDreamConfig) -> BootDeps {
        BootDeps {
            config: cfg,
            agent_id: "test_agent".into(),
            workspace_root,
            state_root,
            parent_ctx_template: mk_agent_ctx(),
            llm: Arc::new(MockLlm),
            tool_dispatcher: Arc::new(NoopDispatcher),
            fork_system_prompt: "system".into(),
            fork_tools: Vec::new(),
            fork_model: "test-model".into(),
            git_checkpointer: None,
            pre_dream_snapshot: None,
            pre_dream_tenant: "default".into(),
        }
    }

    // ── Path helpers (1 sync test) ──

    #[test]
    fn default_paths_compose_correctly() {
        let ws = Path::new("/var/nexo/workspace");
        assert_eq!(
            default_memory_dir(ws, "alice"),
            PathBuf::from("/var/nexo/workspace/.nexo-memory/alice")
        );
        let state = Path::new("/var/nexo/state");
        assert_eq!(
            default_dream_db_path(state),
            PathBuf::from("/var/nexo/state/dream_runs.db")
        );
    }

    // ── build_runner integration tests (6) ──

    #[tokio::test]
    async fn build_runner_returns_none_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = AutoDreamConfig::default(); // enabled = false
        let result = build_runner(mk_deps(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            cfg,
        ))
        .await
        .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn build_runner_validates_config() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = enabled_cfg();
        cfg.min_hours = Duration::from_secs(60); // too low
        let result = build_runner(mk_deps(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            cfg,
        ))
        .await;
        assert!(matches!(result, Err(AutoDreamError::Config(_))));
    }

    #[tokio::test]
    async fn build_runner_creates_default_memory_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = build_runner(mk_deps(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            enabled_cfg(),
        ))
        .await
        .unwrap();
        assert!(runner.is_some());
        let expected = default_memory_dir(tmp.path(), "test_agent");
        assert!(expected.exists(), "default memory_dir not created");
    }

    #[tokio::test]
    async fn build_runner_uses_explicit_memory_dir_override() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("custom_mem");
        std::fs::create_dir_all(&custom).unwrap();
        let mut cfg = enabled_cfg();
        cfg.memory_dir = Some(custom.clone());

        let _runner = build_runner(mk_deps(
            tmp.path().join("ws_unused"),
            tmp.path().to_path_buf(),
            cfg,
        ))
        .await
        .unwrap()
        .unwrap();

        // Default path should NOT have been created.
        let default = default_memory_dir(&tmp.path().join("ws_unused"), "test_agent");
        assert!(!default.exists(), "default memory_dir created despite override");
        assert!(custom.exists());
    }

    #[tokio::test]
    async fn build_runner_creates_dream_db_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let nested_state = tmp.path().join("nested").join("state");
        let _runner = build_runner(mk_deps(
            tmp.path().to_path_buf(),
            nested_state.clone(),
            enabled_cfg(),
        ))
        .await
        .unwrap()
        .unwrap();
        assert!(nested_state.exists());
        assert!(nested_state.join("dream_runs.db").exists());
    }

    #[tokio::test]
    async fn build_runner_returns_some_runner_when_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = build_runner(mk_deps(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            enabled_cfg(),
        ))
        .await
        .unwrap();
        assert!(runner.is_some());
    }

    /// Phase 80.1.b.b.b consumer / MS-3 close — when `BootDeps`
    /// carries a `pre_dream_snapshot` adapter, the constructed
    /// runner reports `has_pre_dream_snapshot() == true`. Without
    /// the field, the runner stays without an anchor.
    #[tokio::test]
    async fn build_runner_attaches_pre_dream_snapshot_when_provided() {
        use async_trait::async_trait;

        struct StubHook;
        #[async_trait]
        impl nexo_driver_types::PreDreamSnapshotHook for StubHook {
            async fn snapshot_before_dream(
                &self,
                _agent_id: &str,
                _tenant: &str,
                _run_id: &str,
            ) -> Result<(), String> {
                Ok(())
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let mut deps = mk_deps(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            enabled_cfg(),
        );
        deps.pre_dream_snapshot = Some(Arc::new(StubHook));
        deps.pre_dream_tenant = "acme".into();

        let runner = build_runner(deps).await.unwrap().expect("runner present");
        assert!(
            runner.has_pre_dream_snapshot(),
            "MS-3: runner must report the pre-dream snapshot adapter is wired"
        );
    }

    #[tokio::test]
    async fn build_runner_skips_pre_dream_snapshot_when_none() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = build_runner(mk_deps(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            enabled_cfg(),
        ))
        .await
        .unwrap()
        .expect("runner present");
        assert!(
            !runner.has_pre_dream_snapshot(),
            "default `pre_dream_snapshot: None` must leave the runner without an adapter"
        );
    }
}
