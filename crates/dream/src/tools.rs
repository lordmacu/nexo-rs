//! `dream_now` LLM tool — manual force-bypass for autoDream gates.
//! Phase 80.1.c.
//!
//! Verbatim semantics from leak `autoDream.ts:102-179` (`isForced()`
//! gate-bypass + `priorMtime = lastAt` rollback no-op). Nexo
//! extension over leak — leak's `isForced()` is build-time test
//! override (always `false`); nexo exposes via LLM tool for
//! self-driven consolidation.
//!
//! # Lifecycle
//!
//! 1. Operator at boot calls
//!    [`register_dream_now_tool`]`(&registry, runner, transcript_dir)`
//!    if `auto_dream.enabled` AND binding policy permits (Phase 16
//!    capability gate — operator's responsibility to gate).
//! 2. LLM invokes `dream_now { reason?: "..." }` mid-turn.
//! 3. Tool calls `runner.run_forced(&dream_ctx)` — bypasses
//!    time/scan/session gates; respects lock + memory-dir precondition.
//! 4. Returns structured JSON outcome — 6 variants matching
//!    [`crate::auto_dream::RunOutcome`].
//!
//! # Capability gate
//!
//! Default off — operators opt-in per binding via Phase 16 binding
//! policy. The boot wiring caller (e.g.
//! `nexo_dream::boot::build_runner`'s sibling) checks the policy
//! before invoking [`register_dream_now_tool`].
//!
//! # Provider-agnostic
//!
//! Tool name + JSON args + plain-text description — works under any
//! [`nexo_llm::LlmClient`] impl per memory rule
//! `feedback_provider_agnostic.md`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_core::agent::tool_registry::{ToolHandler, ToolRegistry};
use nexo_core::agent::AgentContext;
use nexo_driver_types::GoalId;
use nexo_llm::ToolDef;
use serde_json::{json, Value};

use crate::auto_dream::{AutoDreamRunner, DreamContext, RunOutcome};

/// Canonical tool name. Capability gate / config references use this
/// constant to avoid string drift.
pub const DREAM_NOW_TOOL_NAME: &str = "dream_now";

/// Force-trigger memory consolidation. See module docs for lifecycle
/// + gate semantics.
pub struct DreamNowTool {
    runner: Arc<AutoDreamRunner>,
    /// Transcript dir for building [`DreamContext`]. Operator
    /// supplies — same path used in driver-loop's per-turn hook.
    transcript_dir: PathBuf,
}

impl DreamNowTool {
    pub fn new(runner: Arc<AutoDreamRunner>, transcript_dir: PathBuf) -> Self {
        Self {
            runner,
            transcript_dir,
        }
    }

    /// Static tool definition. Mirrors Phase 77.20 Sleep tool shape.
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: DREAM_NOW_TOOL_NAME.into(),
            description:
                "Force a memory consolidation pass now, bypassing time/session gates. \
                 Use when you've just learned a lot and want it consolidated into long-term memory \
                 immediately, instead of waiting for the next scheduled pass."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Why are you forcing the pass? Used for observability logs."
                    }
                }
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for DreamNowTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> Result<Value> {
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("no reason given")
            .to_string();

        let session_uuid = ctx
            .session_id
            .ok_or_else(|| anyhow!("dream_now: AgentContext.session_id is required"))?;

        let dream_ctx = DreamContext {
            goal_id: GoalId(session_uuid),
            session_id: session_uuid.to_string(),
            transcript_dir: self.transcript_dir.clone(),
            // dream_now hardcodes false — operator-driven force runs
            // independent of these flags. The runner still respects
            // lock + memory-dir precondition.
            kairos_active: false,
            remote_mode: false,
        };
        let outcome = self.runner.run_forced(&dream_ctx).await;

        Ok(outcome_to_json(&outcome, &reason))
    }
}

fn outcome_to_json(outcome: &RunOutcome, reason: &str) -> Value {
    match outcome {
        RunOutcome::Completed {
            run_id,
            files_touched,
            duration_ms,
        } => json!({
            "outcome": "completed",
            "run_id": run_id.to_string(),
            "files_touched_count": files_touched.len(),
            "duration_ms": duration_ms,
            "reason": reason,
        }),
        RunOutcome::Skipped { gate } => json!({
            "outcome": "skipped",
            "gate": format!("{gate:?}"),
            "reason": reason,
        }),
        RunOutcome::LockBlocked {
            holder_pid,
            mtime_secs,
        } => json!({
            "outcome": "lock_blocked",
            "holder_pid": holder_pid,
            "mtime_secs": mtime_secs,
            "reason": reason,
        }),
        RunOutcome::Errored {
            run_id,
            error,
            prior_mtime,
        } => json!({
            "outcome": "errored",
            "run_id": run_id.to_string(),
            "error": error,
            "prior_mtime": prior_mtime,
            "reason": reason,
        }),
        RunOutcome::TimedOut {
            run_id,
            timeout_secs,
            prior_mtime,
        } => json!({
            "outcome": "timed_out",
            "run_id": run_id.to_string(),
            "timeout_secs": timeout_secs,
            "prior_mtime": prior_mtime,
            "reason": reason,
        }),
        RunOutcome::EscapeAudit {
            run_id,
            escapes,
            prior_mtime,
        } => json!({
            "outcome": "escape_audit",
            "run_id": run_id.to_string(),
            "escape_count": escapes.len(),
            "prior_mtime": prior_mtime,
            "reason": reason,
        }),
    }
}

/// Phase 80.1.c.b — host-level capability gate env var for the
/// `dream_now` tool. Mirror of `nexo-setup::capabilities::INVENTORY`
/// entry `extension: "dream"`. When the env var resolves to a truthy
/// value the tool is registered; otherwise [`register_dream_now_tool`]
/// is a no-op. Per-binding granularity (Phase 16 `allowed_tools`) layers
/// on top.
pub const DREAM_NOW_ENV_VAR: &str = "NEXO_DREAM_NOW_ENABLED";

/// Boolean coercion for `NEXO_DREAM_NOW_ENABLED`. Keep in sync with
/// `nexo-setup::capabilities::evaluate_one` Boolean arm — same accepted
/// truthy set (`true`/`1`/`yes`, case-insensitive, trimmed). Anything
/// else (including unset, empty, garbage) is `false`. Lives here to
/// avoid pulling `nexo-setup` (with its plugin/auth/google/whatsapp
/// transitive deps) into `nexo-dream` for a single 7-line helper.
fn is_dream_now_env_enabled() -> bool {
    std::env::var(DREAM_NOW_ENV_VAR)
        .ok()
        .map(|s| {
            let lower = s.trim().to_ascii_lowercase();
            matches!(lower.as_str(), "true" | "1" | "yes")
        })
        .unwrap_or(false)
}

/// Register `dream_now` on the operator's [`ToolRegistry`]. Mirror
/// Phase 77.5 / 77.20 tool registration pattern.
///
/// Caller wires this AT BOOT after constructing the runner via
/// `nexo_dream::boot::build_runner`. Two-layer gate:
/// 1. **Host-level** — `NEXO_DREAM_NOW_ENABLED` env var (this fn).
///    When unset / falsy, the tool is NOT registered and the function
///    is a tracing-logged no-op.
/// 2. **Per-binding** — Phase 16 `allowed_tools` decides whether the
///    registered tool reaches a given binding's surface.
pub fn register_dream_now_tool(
    registry: &ToolRegistry,
    runner: Arc<AutoDreamRunner>,
    transcript_dir: PathBuf,
) {
    if !is_dream_now_env_enabled() {
        tracing::info!(
            target: "nexo_dream::tools",
            env_var = DREAM_NOW_ENV_VAR,
            "dream_now: host-level capability gate closed; tool not registered"
        );
        return;
    }
    let tool = DreamNowTool::new(runner, transcript_dir);
    registry.register_arc(DreamNowTool::tool_def(), Arc::new(tool));
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_swap::ArcSwap;
    use async_trait::async_trait;
    use futures::future::{ready, BoxFuture};
    use nexo_agent_registry::{DreamRunStore, SqliteDreamRunStore};
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use nexo_config::types::dream::AutoDreamConfig;
    use nexo_core::session::SessionManager;
    use nexo_fork::{ForkError, ForkHandle, ForkParams, ForkResult, ForkSubagent, ToolDispatcher};
    use nexo_llm::stream::StreamChunk;
    use nexo_llm::types::{ChatRequest, ChatResponse, FinishReason, ResponseContent, TokenUsage};
    use nexo_llm::LlmClient;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use crate::auto_dream::SkipReason;
    use crate::consolidation_lock::ConsolidationLock;

    /// Phase 80.1.c.b — env-var tests touch process-wide state; serialize them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Set the env var inside an `ENV_LOCK` guard, return the guard so the
    /// caller drops it after test work; cleanup runs at drop time of `Cleanup`.
    struct EnvGuard<'a> {
        _lock: std::sync::MutexGuard<'a, ()>,
    }
    impl<'a> EnvGuard<'a> {
        fn set(value: &str) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::set_var(DREAM_NOW_ENV_VAR, value);
            Self { _lock: lock }
        }
        fn unset() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var(DREAM_NOW_ENV_VAR);
            Self { _lock: lock }
        }
    }
    impl<'a> Drop for EnvGuard<'a> {
        fn drop(&mut self) {
            std::env::remove_var(DREAM_NOW_ENV_VAR);
        }
    }

    // ── Mocks (copied minimal — option B per spec) ──

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
        ) -> anyhow::Result<futures::stream::BoxStream<'a, anyhow::Result<StreamChunk>>>
        {
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

    /// Mock fork that returns a scripted `ForkResult` on first call.
    /// Used to drive the runner's `run_forced` to specific outcomes.
    struct MockFork {
        result: tokio::sync::Mutex<Option<Result<ForkResult, ForkError>>>,
    }

    impl MockFork {
        fn ok() -> Arc<Self> {
            Arc::new(Self {
                result: tokio::sync::Mutex::new(Some(Ok(ForkResult {
                    messages: vec![],
                    total_usage: TokenUsage::default(),
                    total_cache_usage: nexo_llm::types::CacheUsage::default(),
                    final_text: Some("done".into()),
                    turns_executed: 1,
                }))),
            })
        }
    }

    #[async_trait]
    impl ForkSubagent for MockFork {
        async fn fork(&self, _params: ForkParams) -> Result<ForkHandle, ForkError> {
            let mut g = self.result.lock().await;
            let r = g
                .take()
                .unwrap_or_else(|| Err(ForkError::Internal("exhausted".into())));
            let abort = CancellationToken::new();
            let fut: BoxFuture<'static, Result<ForkResult, ForkError>> = Box::pin(ready(r));
            Ok(ForkHandle::new(Uuid::new_v4(), None, fut, abort))
        }
    }

    fn mk_agent_config() -> AgentConfig {
        AgentConfig {
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
            extensions_config: std::collections::BTreeMap::new(),
        }
    }

    fn mk_agent_ctx_with_session() -> AgentContext {
        let ctx = AgentContext::new(
            "test_agent",
            Arc::new(mk_agent_config()),
            AnyBroker::local(),
            Arc::new(SessionManager::new(Duration::from_secs(60), 8)),
        );
        ctx.with_session_id(Uuid::new_v4())
    }

    fn mk_agent_ctx_no_session() -> AgentContext {
        // session_id field defaults to None in `AgentContext::new`.
        AgentContext::new(
            "test_agent",
            Arc::new(mk_agent_config()),
            AnyBroker::local(),
            Arc::new(SessionManager::new(Duration::from_secs(60), 8)),
        )
    }

    fn enabled_cfg() -> AutoDreamConfig {
        AutoDreamConfig {
            enabled: true,
            ..AutoDreamConfig::default()
        }
    }

    async fn mk_runner(memory_dir: PathBuf, fork: Arc<dyn ForkSubagent>) -> AutoDreamRunner {
        let cfg = enabled_cfg();
        let lock = Arc::new(ConsolidationLock::new(&memory_dir, cfg.holder_stale).unwrap());
        let audit: Arc<dyn DreamRunStore> =
            Arc::new(SqliteDreamRunStore::open_memory().await.unwrap());
        AutoDreamRunner::new(
            Arc::new(ArcSwap::from_pointee(cfg)),
            lock,
            fork,
            audit,
            Arc::new(MockLlm),
            Arc::new(NoopDispatcher),
            memory_dir,
            mk_agent_ctx_no_session(),
            "system".into(),
            Vec::new(),
            "test-model".into(),
        )
        .unwrap()
    }

    async fn mk_tool() -> DreamNowTool {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        // Leak tmpdir: tests are independent; OS reclaims.
        std::mem::forget(tmp);
        let runner = Arc::new(mk_runner(dir.clone(), MockFork::ok()).await);
        DreamNowTool::new(runner, dir)
    }

    // ── tool_def shape ──

    #[test]
    fn tool_def_shape() {
        let def = DreamNowTool::tool_def();
        assert_eq!(def.name, DREAM_NOW_TOOL_NAME);
        assert!(def.description.contains("Force a memory consolidation"));
        let params = &def.parameters;
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["reason"].is_object());
    }

    // ── reason extraction defaults ──

    #[tokio::test]
    async fn call_with_reason_returns_completed() {
        let tool = mk_tool().await;
        let ctx = mk_agent_ctx_with_session();
        let result = tool
            .call(&ctx, json!({"reason": "user said so"}))
            .await
            .unwrap();
        assert_eq!(result["outcome"], "completed");
        assert_eq!(result["reason"], "user said so");
    }

    #[tokio::test]
    async fn call_without_reason_uses_default() {
        let tool = mk_tool().await;
        let ctx = mk_agent_ctx_with_session();
        let result = tool.call(&ctx, json!({})).await.unwrap();
        assert_eq!(result["reason"], "no reason given");
    }

    #[tokio::test]
    async fn call_with_empty_reason_uses_default() {
        let tool = mk_tool().await;
        let ctx = mk_agent_ctx_with_session();
        let result = tool.call(&ctx, json!({"reason": ""})).await.unwrap();
        assert_eq!(result["reason"], "no reason given");
    }

    #[tokio::test]
    async fn call_with_non_string_reason_uses_default() {
        let tool = mk_tool().await;
        let ctx = mk_agent_ctx_with_session();
        let result = tool.call(&ctx, json!({"reason": 42})).await.unwrap();
        assert_eq!(result["reason"], "no reason given");
    }

    // ── session_id required ──

    #[tokio::test]
    async fn call_without_session_id_errors() {
        let tool = mk_tool().await;
        let ctx = mk_agent_ctx_no_session();
        let err = tool.call(&ctx, json!({})).await.unwrap_err();
        assert!(err.to_string().contains("session_id"));
    }

    // ── outcome_to_json ──

    #[test]
    fn outcome_to_json_skipped_renders_gate() {
        let outcome = RunOutcome::Skipped {
            gate: SkipReason::KairosActive,
        };
        let json = outcome_to_json(&outcome, "test");
        assert_eq!(json["outcome"], "skipped");
        assert_eq!(json["gate"], "KairosActive");
        assert_eq!(json["reason"], "test");
    }

    // ── register helper ──

    #[tokio::test]
    async fn register_dream_now_tool_adds_to_registry() {
        let _env = EnvGuard::set("true");
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let runner = Arc::new(mk_runner(dir.clone(), MockFork::ok()).await);
        let registry = ToolRegistry::new();
        register_dream_now_tool(&registry, runner, dir);
        assert!(registry.contains(DREAM_NOW_TOOL_NAME));
        let (def, _handler) = registry.get(DREAM_NOW_TOOL_NAME).unwrap();
        assert_eq!(def.name, DREAM_NOW_TOOL_NAME);
    }

    // ── Phase 80.1.c.b — host-level env gate ──

    #[tokio::test]
    async fn register_dream_now_skips_when_env_disabled() {
        let _env = EnvGuard::unset();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let runner = Arc::new(mk_runner(dir.clone(), MockFork::ok()).await);
        let registry = ToolRegistry::new();
        register_dream_now_tool(&registry, runner, dir);
        assert!(
            !registry.contains(DREAM_NOW_TOOL_NAME),
            "dream_now must NOT register when NEXO_DREAM_NOW_ENABLED is unset"
        );
    }

    #[tokio::test]
    async fn register_dream_now_skips_when_env_garbage() {
        let _env = EnvGuard::set("maybe");
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let runner = Arc::new(mk_runner(dir.clone(), MockFork::ok()).await);
        let registry = ToolRegistry::new();
        register_dream_now_tool(&registry, runner, dir);
        assert!(
            !registry.contains(DREAM_NOW_TOOL_NAME),
            "dream_now must NOT register for non-truthy env value `maybe`"
        );
    }

    #[tokio::test]
    async fn register_dream_now_registers_for_truthy_variants() {
        for truthy in ["true", "TRUE", "True", "1", "yes", "YES"] {
            let _env = EnvGuard::set(truthy);
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().canonicalize().unwrap();
            let runner = Arc::new(mk_runner(dir.clone(), MockFork::ok()).await);
            let registry = ToolRegistry::new();
            register_dream_now_tool(&registry, runner, dir);
            assert!(
                registry.contains(DREAM_NOW_TOOL_NAME),
                "dream_now must register for truthy env value `{truthy}`"
            );
        }
    }

    #[tokio::test]
    async fn register_dream_now_skips_for_falsy_variants() {
        for falsy in ["false", "FALSE", "0", "no", "", "garbage"] {
            let _env = EnvGuard::set(falsy);
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().canonicalize().unwrap();
            let runner = Arc::new(mk_runner(dir.clone(), MockFork::ok()).await);
            let registry = ToolRegistry::new();
            register_dream_now_tool(&registry, runner, dir);
            assert!(
                !registry.contains(DREAM_NOW_TOOL_NAME),
                "dream_now must NOT register for falsy env value `{falsy}`"
            );
        }
    }
}
