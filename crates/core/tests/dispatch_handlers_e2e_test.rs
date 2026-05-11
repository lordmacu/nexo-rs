//! Multi-agent dispatch end-to-end.
//!
//! Wires AgentContext.dispatch with a real AgentRegistry, a real
//! DriverOrchestrator (cap=0 so admit lands as Queued without
//! actually spawning Claude), an FsProjectTracker pointing at a
//! tempdir PHASES.md, and a capturing DispatchTelemetry. Then
//! invokes `ProgramPhaseHandler::call` twice from the same agent
//! context and asserts:
//!
//! 1. Both calls return `Queued` outputs (cap=0 + queue_when_full).
//! 2. The registry holds two entries.
//! 3. The capturing telemetry observed two `dispatch_spawned`
//!    events, no `dispatch_denied`.
//!
//! This validates that the handler + registry filter + threaded
//! telemetry wiring composes correctly without requiring a real
//! Claude subprocess.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nexo_agent_registry::{AgentRegistry, LogBuffer, MemoryAgentRegistryStore};
use nexo_broker::AnyBroker;
use nexo_config::{
    types::agents::{
        AgentConfig, AgentRuntimeConfig, DispatchCapability, DispatchPolicy, DreamingYamlConfig,
        HeartbeatConfig, ModelConfig, OutboundAllowlistConfig, WorkspaceGitConfig,
    },
    InboundBinding,
};
use nexo_core::agent::{
    context::AgentContext,
    dispatch_handlers::{
        AddHookHandler, DispatchToolContext, ProgramPhaseHandler, RemoveHookHandler,
    },
    tool_registry::ToolHandler,
};
use nexo_dispatch_tools::{
    policy_gate::CapSnapshot, DispatchDeniedPayload, DispatchSpawnedPayload, DispatchTelemetry,
    HookDispatchedPayload, HookFailedPayload, HookRegistry,
};
use nexo_driver_claude::{ClaudeConfig, ClaudeDefaultArgs, MemoryBindingStore, OutputFormat};
use nexo_driver_loop::{DriverOrchestrator, NoopEventSink, WorkspaceManager};
use nexo_driver_permission::{AllowAllDecider, PermissionDecider};
use nexo_project_tracker::MutableTracker;
use serde_json::json;

// ── Capturing telemetry ──────────────────────────────────────

#[derive(Default)]
struct CapturingTelemetry {
    spawned: Mutex<Vec<DispatchSpawnedPayload>>,
    denied: Mutex<Vec<DispatchDeniedPayload>>,
}

#[async_trait]
impl DispatchTelemetry for CapturingTelemetry {
    async fn dispatch_spawned(&self, p: DispatchSpawnedPayload) {
        self.spawned.lock().unwrap().push(p);
    }
    async fn dispatch_denied(&self, p: DispatchDeniedPayload) {
        self.denied.lock().unwrap().push(p);
    }
    async fn hook_dispatched(&self, _p: HookDispatchedPayload) {}
    async fn hook_failed(&self, _p: HookFailedPayload) {}
}

fn empty_config(dispatch_full: bool) -> Arc<AgentConfig> {
    let mode = if dispatch_full {
        DispatchCapability::Full
    } else {
        DispatchCapability::None
    };
    Arc::new(AgentConfig {
        id: "tester".into(),
        model: ModelConfig {
            provider: "anthropic".into(),
            model: "x".into(),
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
        inbound_bindings: Vec::<InboundBinding>::new(),
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
        dispatch_policy: DispatchPolicy {
            mode,
            ..Default::default()
        },
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
        active: true,
    })
}

async fn build_orch(socket: PathBuf, ws: PathBuf) -> Arc<DriverOrchestrator> {
    let cfg = ClaudeConfig {
        binary: Some(PathBuf::from("bash")),
        default_args: ClaudeDefaultArgs {
            output_format: OutputFormat::StreamJson,
            permission_prompt_tool: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            model: None,
        },
        mcp_config: None,
        forced_kill_after: Duration::from_secs(1),
        turn_timeout: Duration::from_secs(10),
    };
    Arc::new(
        DriverOrchestrator::builder()
            .claude_config(cfg)
            .binding_store(Arc::new(MemoryBindingStore::new())
                as Arc<dyn nexo_driver_claude::SessionBindingStore>)
            .decider(Arc::new(AllowAllDecider) as Arc<dyn PermissionDecider>)
            .workspace_manager(Arc::new(WorkspaceManager::new(&ws)))
            .event_sink(Arc::new(NoopEventSink))
            .bin_path(PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp"))
            .socket_path(socket)
            .build()
            .await
            .unwrap(),
    )
}

#[tokio::test]
async fn two_program_phase_calls_queue_two_goals_and_emit_two_spawned_events() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("PHASES.md"),
        "## Phase 99 — Test\n\n#### 99.1 — A   ⬜\n#### 99.2 — B   ⬜\n",
    )
    .unwrap();

    let tracker: Arc<MutableTracker> = Arc::new(MutableTracker::open_fs(dir.path()).unwrap());
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        0, // forces queue → no real subprocess
    ));
    let orch = build_orch(dir.path().join("d.sock"), dir.path().join("ws")).await;
    let telemetry = Arc::new(CapturingTelemetry::default());
    let telemetry_dyn: Arc<dyn DispatchTelemetry> = telemetry.clone();

    let dispatch_ctx = Arc::new(DispatchToolContext {
        tracker,
        orchestrator: orch,
        registry: registry.clone(),
        hooks: Arc::new(HookRegistry::new()),
        hook_dispatcher: None,
        turn_log: None,
        log_buffer: Arc::new(LogBuffer::new(64)),
        default_caps: CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
        require_trusted: false,
        telemetry: telemetry_dyn,
        allow_self_modify: true,
        daemon_source_root: dir.path().to_path_buf(),
        audit_before_done: false,
        chainer: None,
        llm_registry: None,
    });

    let cfg = empty_config(true);
    let ctx = AgentContext::new(
        "tester",
        cfg,
        AnyBroker::local(),
        Arc::new(nexo_core::session::SessionManager::new(
            Duration::from_secs(60),
            64,
        )),
    )
    .with_dispatch(dispatch_ctx)
    .with_sender_trusted(true);

    let h = ProgramPhaseHandler;
    let r1 = h.call(&ctx, json!({ "phase_id": "99.1" })).await.unwrap();
    let r2 = h.call(&ctx, json!({ "phase_id": "99.2" })).await.unwrap();
    assert_eq!(r1["status"], "queued");
    assert_eq!(r2["status"], "queued");

    // Registry has both goals.
    let rows = registry.list().await.unwrap();
    assert_eq!(rows.len(), 2);

    // Telemetry observed both.
    let spawned = telemetry.spawned.lock().unwrap();
    assert_eq!(spawned.len(), 2);
    let phases: Vec<&str> = spawned.iter().map(|p| p.phase_id.as_str()).collect();
    assert!(phases.contains(&"99.1"));
    assert!(phases.contains(&"99.2"));
    assert!(telemetry.denied.lock().unwrap().is_empty());
}

#[tokio::test]
async fn capability_none_emits_dispatch_denied_telemetry_no_registry_entry() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("PHASES.md"),
        "## Phase 99 — Test\n\n#### 99.1 — A   ⬜\n",
    )
    .unwrap();

    let tracker: Arc<MutableTracker> = Arc::new(MutableTracker::open_fs(dir.path()).unwrap());
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("d2.sock"), dir.path().join("ws2")).await;
    let telemetry = Arc::new(CapturingTelemetry::default());
    let telemetry_dyn: Arc<dyn DispatchTelemetry> = telemetry.clone();

    let dispatch_ctx = Arc::new(DispatchToolContext {
        tracker,
        orchestrator: orch,
        registry: registry.clone(),
        hooks: Arc::new(HookRegistry::new()),
        hook_dispatcher: None,
        turn_log: None,
        log_buffer: Arc::new(LogBuffer::new(16)),
        default_caps: CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
        require_trusted: true,
        telemetry: telemetry_dyn,
        allow_self_modify: true,
        daemon_source_root: dir.path().to_path_buf(),
        audit_before_done: false,
        chainer: None,
        llm_registry: None,
    });

    // Capability=None on this agent's policy.
    let cfg = empty_config(false);
    let ctx = AgentContext::new(
        "tester",
        cfg,
        AnyBroker::local(),
        Arc::new(nexo_core::session::SessionManager::new(
            Duration::from_secs(60),
            64,
        )),
    )
    .with_dispatch(dispatch_ctx);

    let h = ProgramPhaseHandler;
    let r = h.call(&ctx, json!({ "phase_id": "99.1" })).await.unwrap();
    assert_eq!(r["status"], "forbidden");
    assert_eq!(registry.count_running(), 0);
    let denied = telemetry.denied.lock().unwrap();
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].phase_id, "99.1");
    assert!(telemetry.spawned.lock().unwrap().is_empty());
}

// ── add_hook + remove_hook ────

/// Helper: build a DispatchToolContext with a fresh HookRegistry
/// so the add/remove tests don't share state.
async fn build_ctx_for_hooks(
    dir: &tempfile::TempDir,
) -> (
    Arc<DispatchToolContext>,
    Arc<HookRegistry>,
    Arc<AgentContext>,
) {
    std::fs::write(
        dir.path().join("PHASES.md"),
        "## Phase 99 — Test\n\n#### 99.1 — A   ⬜\n",
    )
    .unwrap();
    let tracker: Arc<MutableTracker> = Arc::new(MutableTracker::open_fs(dir.path()).unwrap());
    let registry = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let orch = build_orch(dir.path().join("hooks.sock"), dir.path().join("hooks_ws")).await;
    let telemetry = Arc::new(CapturingTelemetry::default());
    let telemetry_dyn: Arc<dyn DispatchTelemetry> = telemetry.clone();
    let hooks = Arc::new(HookRegistry::new());
    let dispatch_ctx = Arc::new(DispatchToolContext {
        tracker,
        orchestrator: orch,
        registry: registry.clone(),
        hooks: hooks.clone(),
        hook_dispatcher: None,
        turn_log: None,
        log_buffer: Arc::new(LogBuffer::new(16)),
        default_caps: CapSnapshot {
            queue_when_full: true,
            ..Default::default()
        },
        require_trusted: false,
        telemetry: telemetry_dyn,
        allow_self_modify: true,
        daemon_source_root: dir.path().to_path_buf(),
        audit_before_done: false,
        chainer: None,
        llm_registry: None,
    });
    let cfg = empty_config(true);
    let ctx = AgentContext::new(
        "tester",
        cfg,
        AnyBroker::local(),
        Arc::new(nexo_core::session::SessionManager::new(
            Duration::from_secs(60),
            64,
        )),
    )
    .with_dispatch(dispatch_ctx.clone());
    (dispatch_ctx, hooks, Arc::new(ctx))
}

#[tokio::test]
async fn add_hook_attaches_to_goal_and_returns_position() {
    let dir = tempfile::tempdir().unwrap();
    let (_dispatch, hooks, ctx) = build_ctx_for_hooks(&dir).await;

    let goal_id = uuid::Uuid::new_v4();
    let r = AddHookHandler
        .call(
            &ctx,
            json!({
                "goal_id": goal_id.to_string(),
                "hook": {
                    "id": "my-notify",
                    "on": { "on": "done" },
                    "action": { "kind": "notify_origin" }
                }
            }),
        )
        .await
        .expect("add_hook ok");
    assert_eq!(r["added"], true);
    assert_eq!(r["position"], 0);
    assert_eq!(r["hook_id"], "my-notify");
    // Hook actually present in the registry.
    assert_eq!(
        hooks.count(nexo_driver_types::GoalId(goal_id)),
        1,
        "registry must hold the hook"
    );
}

#[tokio::test]
async fn add_hook_idempotent_on_duplicate_id() {
    let dir = tempfile::tempdir().unwrap();
    let (_dispatch, hooks, ctx) = build_ctx_for_hooks(&dir).await;
    let goal_id = uuid::Uuid::new_v4();
    let payload = json!({
        "goal_id": goal_id.to_string(),
        "hook": {
            "id": "dup-check",
            "on": { "on": "done" },
            "action": { "kind": "notify_origin" }
        }
    });
    let r1 = AddHookHandler.call(&ctx, payload.clone()).await.unwrap();
    assert_eq!(r1["added"], true);
    let r2 = AddHookHandler.call(&ctx, payload).await.unwrap();
    assert_eq!(r2["added"], false);
    assert!(r2["reason"]
        .as_str()
        .unwrap_or_default()
        .contains("already attached"));
    assert_eq!(
        hooks.count(nexo_driver_types::GoalId(goal_id)),
        1,
        "second add must be a no-op"
    );
}

#[tokio::test]
async fn add_hook_rejects_empty_id() {
    let dir = tempfile::tempdir().unwrap();
    let (_dispatch, hooks, ctx) = build_ctx_for_hooks(&dir).await;
    let goal_id = uuid::Uuid::new_v4();
    let r = AddHookHandler
        .call(
            &ctx,
            json!({
                "goal_id": goal_id.to_string(),
                "hook": {
                    "id": "",
                    "on": { "on": "done" },
                    "action": { "kind": "notify_origin" }
                }
            }),
        )
        .await
        .expect("call returns Ok with structured failure");
    assert_eq!(r["added"], false);
    assert!(r["reason"]
        .as_str()
        .unwrap_or_default()
        .contains("non-empty"));
    assert_eq!(hooks.count(nexo_driver_types::GoalId(goal_id)), 0);
}

#[tokio::test]
async fn remove_hook_removes_existing_and_handles_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (_dispatch, hooks, ctx) = build_ctx_for_hooks(&dir).await;
    let goal_id = uuid::Uuid::new_v4();
    AddHookHandler
        .call(
            &ctx,
            json!({
                "goal_id": goal_id.to_string(),
                "hook": {
                    "id": "to-remove",
                    "on": { "on": "done" },
                    "action": { "kind": "notify_origin" }
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(hooks.count(nexo_driver_types::GoalId(goal_id)), 1);

    // Existing → removed: true
    let r1 = RemoveHookHandler
        .call(
            &ctx,
            json!({"goal_id": goal_id.to_string(), "hook_id": "to-remove"}),
        )
        .await
        .unwrap();
    assert_eq!(r1["removed"], true);
    assert_eq!(hooks.count(nexo_driver_types::GoalId(goal_id)), 0);

    // Missing → removed: false (probe-then-remove pattern works).
    let r2 = RemoveHookHandler
        .call(
            &ctx,
            json!({"goal_id": goal_id.to_string(), "hook_id": "ghost"}),
        )
        .await
        .unwrap();
    assert_eq!(r2["removed"], false);
}

#[tokio::test]
async fn add_hook_rejects_invalid_goal_id() {
    let dir = tempfile::tempdir().unwrap();
    let (_dispatch, _hooks, ctx) = build_ctx_for_hooks(&dir).await;
    let err = AddHookHandler
        .call(
            &ctx,
            json!({
                "goal_id": "not-a-uuid",
                "hook": {
                    "id": "x",
                    "on": { "on": "done" },
                    "action": { "kind": "notify_origin" }
                }
            }),
        )
        .await
        .expect_err("invalid uuid must error");
    assert!(err.to_string().contains("invalid goal_id"));
}
