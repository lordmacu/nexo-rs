//! Phase 81.32 c10 — `ConfigReloadCoordinator` hot-spawn / hot-remove tests.
//!
//! Exercises the unknown-id (c8) and removed-id (c9) branches of
//! `ConfigReloadCoordinator::reload()` without spinning a real
//! `AgentRuntime`. The spawner closure is replaced with a stub that
//! returns a fabricated `SpawnedAgent` whose `reload_tx` is a plain
//! `mpsc::channel` so we can assert the registration + shutdown
//! dispatch without touching the broker layer.
//!
//! Coverage:
//!   - spawner not installed → unknown id rejects with a "set a
//!     spawner" diagnostic (no panic, no legacy Phase-18 message).
//!   - spawner returns Err → SpawnError text propagates verbatim
//!     into the rejection reason.
//!   - spawner returns Ok → coord registers the new id and reports
//!     it in `applied`.
//!   - hot-remove → coord sends `ReloadCommand::Shutdown` to the
//!     vanished agent's mailbox and unregisters its handle.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::{fs, path::Path};

use nexo_core::agent::runtime::ReloadCommand;
use nexo_core::agent::spawn::{AgentSpawnerFn, SpawnError, SpawnedAgent};
use nexo_core::ConfigReloadCoordinator;
use nexo_llm::LlmRegistry;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn write_minimal_config_dir(id: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "agents.yaml",
        &format!(
            r#"
schema_version: 11
agents:
  - id: "{id}"
    model:
      provider: "anthropic"
      model: "claude-haiku-4-5"
    plugins:
      - whatsapp
    system_prompt: "minimal"
    inbound_bindings:
      - plugin: "whatsapp"
        allowed_tools: ["old_tool"]
"#
        ),
    );
    write_file(
        dir.path(),
        "broker.yaml",
        r#"
broker:
  type: "nats"
  url: "nats://localhost:4222"
"#,
    );
    write_file(
        dir.path(),
        "llm.yaml",
        r#"
providers:
  anthropic:
    api_key: "dummy"
    base_url: "https://api.anthropic.com"
"#,
    );
    write_file(
        dir.path(),
        "memory.yaml",
        r#"
short_term: {}
long_term:
  backend: "sqlite"
  sqlite:
    path: "./memory.db"
vector:
  backend: "sqlite-vec"
  embedding:
    provider: "anthropic"
    model: "text-embedding-3-small"
    dimensions: 1536
"#,
    );
    write_file(
        dir.path(),
        "runtime.yaml",
        r#"
migrations:
  auto_apply: true
"#,
    );
    dir
}

fn write_file(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

fn build_coord(config_dir: PathBuf) -> Arc<ConfigReloadCoordinator> {
    Arc::new(ConfigReloadCoordinator::new(
        config_dir,
        Arc::new(LlmRegistry::with_builtins()),
        CancellationToken::new(),
    ))
}

/// Stub `SpawnedAgent` with a fresh mpsc channel. Mirrors what
/// the real spawner returns but avoids constructing a full
/// `AgentRuntime` (which would pull broker + sessions + every Arc
/// dep into the test). The `runtime` field would normally hold an
/// `AgentRuntime`; we use `Option::None` via an `unsafe` newtype
/// is not needed because `SpawnedAgent::runtime` is required —
/// instead we build a minimal `AgentRuntime` from a local broker
/// + session manager. This keeps the test honest about the type
/// the coord receives.
async fn fabricated_spawned(id: &str) -> (SpawnedAgent, mpsc::Receiver<ReloadCommand>) {
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig, OutboundAllowlistConfig,
    };
    use nexo_core::agent::{Agent, AgentBehavior, AgentContext, AgentRuntime, InboundMessage};
    use nexo_core::session::SessionManager;
    use std::time::Duration;

    // Behavior is irrelevant for these coord-level tests — the
    // runtime is never started, so its broker subscribers never
    // open. `Recorder` here only satisfies the `AgentBehavior`
    // trait bound.
    struct Noop;
    #[async_trait::async_trait]
    impl AgentBehavior for Noop {
        async fn on_message(
            &self,
            _ctx: &AgentContext,
            _msg: InboundMessage,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn on_heartbeat(&self, _ctx: &AgentContext) -> anyhow::Result<()> {
            Ok(())
        }
        async fn decide(
            &self,
            _ctx: &AgentContext,
            msg: &InboundMessage,
        ) -> anyhow::Result<String> {
            Ok(msg.text.clone())
        }
    }

    let cfg = AgentConfig {
        id: id.into(),
        model: ModelConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
        },
        plugins: vec!["whatsapp".into()],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig {
            debounce_ms: 0,
            queue_cap: 32,
        },
        system_prompt: "stub".into(),
        workspace: String::new(),
        skills: Vec::new(),
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        allowed_tools: Vec::new(),
        sender_rate_limit: None,
        allowed_delegates: Vec::new(),
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: OutboundAllowlistConfig::default(),
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
        locale_prompts: Default::default(),
        inbound_bindings: Vec::new(),
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
        active: true,
    };
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let agent = Arc::new(Agent::new(cfg, Noop));
    let runtime = AgentRuntime::new(agent, broker, sessions);
    let reload_tx_runtime = runtime.reload_sender();

    // The coord registers under `reload_tx` it receives. We
    // pass it our own channel so the test can listen for
    // `Shutdown` — `runtime` carries its own internal channel we
    // never start. The unused warning on `reload_tx_runtime` is
    // intentional documentation that the real spawner would
    // return the runtime's own sender.
    let _ = reload_tx_runtime;

    let (tx, rx) = mpsc::channel(8);
    (
        SpawnedAgent {
            agent_id: id.into(),
            reload_tx: tx,
            known_tools: Arc::new(vec!["old_tool".into()]),
            shutdown_token: CancellationToken::new(),
            runtime,
        },
        rx,
    )
}

#[tokio::test]
async fn unknown_id_without_spawner_rejects_with_actionable_message() {
    let dir = write_minimal_config_dir("brand_new");
    let coord = build_coord(dir.path().to_path_buf());
    // No `set_spawner` call — legacy fallback path.

    let outcome = coord.reload().await;
    assert!(outcome.applied.is_empty(), "{:#?}", outcome);
    assert_eq!(outcome.rejected.len(), 1);
    let rej = &outcome.rejected[0];
    assert_eq!(rej.agent_id.as_deref(), Some("brand_new"));
    assert!(
        rej.reason.contains("set a spawner"),
        "rejection must point operators at set_spawner, got: {}",
        rej.reason,
    );
}

#[tokio::test]
async fn spawner_returns_err_surfaces_full_reason_in_rejection() {
    let dir = write_minimal_config_dir("flaky");
    let coord = build_coord(dir.path().to_path_buf());
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = Arc::clone(&calls);
    let spawner: AgentSpawnerFn = AgentSpawnerFn(Box::new(move |cfg, _plugins, _llm| {
        let calls = Arc::clone(&calls_c);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(SpawnError::LlmBind(format!(
                "agent `{}` provider not configured",
                cfg.id
            )))
        })
    }));
    coord.set_spawner(Arc::new(spawner));

    let outcome = coord.reload().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "spawner must be invoked");
    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.rejected.len(), 1);
    let rej = &outcome.rejected[0];
    assert!(
        rej.reason.contains("provider not configured"),
        "{}",
        rej.reason
    );
    // The `llm bind:` prefix from SpawnError::Display proves the
    // typed variant survived round-trip, not a stringified anyhow.
    assert!(rej.reason.contains("llm bind"), "{}", rej.reason);
}

#[tokio::test]
async fn spawner_success_registers_id_and_reports_applied() {
    let dir = write_minimal_config_dir("hot");
    let coord = build_coord(dir.path().to_path_buf());

    let (spawned, _rx) = fabricated_spawned("hot").await;
    let cell = Arc::new(tokio::sync::Mutex::new(Some(spawned)));
    let cell_c = Arc::clone(&cell);
    let spawner: AgentSpawnerFn = AgentSpawnerFn(Box::new(move |_cfg, _plugins, _llm| {
        let cell = Arc::clone(&cell_c);
        Box::pin(async move {
            // Hand out the prebuilt SpawnedAgent on first call.
            // Subsequent calls return Internal so a test author
            // who forgets to set up a fresh cell sees a clear
            // error.
            cell.lock()
                .await
                .take()
                .ok_or_else(|| SpawnError::Internal("test cell already drained".into()))
        })
    }));
    coord.set_spawner(Arc::new(spawner));

    let outcome = coord.reload().await;
    assert!(outcome.rejected.is_empty(), "{:#?}", outcome.rejected);
    assert_eq!(outcome.applied, vec!["hot".to_string()]);
}

#[tokio::test]
async fn removed_id_triggers_shutdown_dispatch_and_unregisters() {
    // Boot with one registered agent, then reload against an empty
    // agents.yaml so coord detects the removal.
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "agents.yaml",
        r#"
schema_version: 11
agents: []
"#,
    );
    write_file(
        dir.path(),
        "broker.yaml",
        r#"broker:
  type: "nats"
  url: "nats://localhost:4222"
"#,
    );
    write_file(
        dir.path(),
        "llm.yaml",
        r#"providers:
  anthropic:
    api_key: "dummy"
    base_url: "https://api.anthropic.com"
"#,
    );
    write_file(
        dir.path(),
        "memory.yaml",
        r#"short_term: {}
long_term:
  backend: "sqlite"
  sqlite:
    path: "./memory.db"
vector:
  backend: "sqlite-vec"
  embedding:
    provider: "anthropic"
    model: "text-embedding-3-small"
    dimensions: 1536
"#,
    );
    write_file(
        dir.path(),
        "runtime.yaml",
        r#"migrations:
  auto_apply: true
"#,
    );

    let coord = build_coord(dir.path().to_path_buf());
    let (tx, mut rx) = mpsc::channel(8);
    coord.register("orphan", tx, Arc::new(vec!["old_tool".into()]));

    let outcome = coord.reload().await;
    // `orphan` is now in `applied` (hot-removed), and the
    // runtime entry vanishes from coord's runtimes map.
    assert!(
        outcome.applied.iter().any(|id| id == "orphan"),
        "{:#?}",
        outcome
    );
    // Coord must have unregistered the handle — re-running
    // reload would otherwise re-emit the removal forever.
    assert!(
        coord.unregister("orphan").is_none(),
        "handle must already be unregistered"
    );
    // `Shutdown` was dispatched. Drain channel once with a short
    // timeout — the send is best-effort but should succeed since
    // the receiver is alive.
    let cmd = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .expect("coord must send Shutdown within timeout")
        .expect("channel closed without sending");
    assert!(
        matches!(cmd, ReloadCommand::Shutdown),
        "expected ReloadCommand::Shutdown",
    );
}
