//! Phase 18 — end-to-end hot-reload exercises.
//!
//! Spin up a runtime, send a message to establish the "old snapshot"
//! baseline, then swap a fresh snapshot through `ReloadCommand::Apply`
//! and verify the NEXT message sees the new policy. Uses the local
//! broker + a recorder behavior to snapshot `ctx.effective_policy()`
//! at each turn.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, path::Path};

use async_trait::async_trait;
use nexo_broker::{types::Event, AnyBroker, BrokerHandle};
use nexo_config::types::agents::{
    AgentConfig, AgentRuntimeConfig, HeartbeatConfig, InboundBinding, ModelConfig,
    OutboundAllowlistConfig, SenderRateLimitOverride,
};
use nexo_core::agent::runtime::ReloadCommand;
use nexo_core::agent::{Agent, AgentBehavior, AgentContext, AgentRuntime, InboundMessage};
use nexo_core::session::SessionManager;
use nexo_core::ConfigReloadCoordinator;
use nexo_core::RuntimeSnapshot;
use nexo_llm::LlmRegistry;
use serde_json::json;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Capture {
    allowed_tools: Vec<String>,
    version: u64,
    #[allow(dead_code)]
    text: String,
}

struct Recorder {
    out: Arc<Mutex<Vec<Capture>>>,
}

#[async_trait]
impl AgentBehavior for Recorder {
    async fn on_message(&self, ctx: &AgentContext, msg: InboundMessage) -> anyhow::Result<()> {
        let eff = ctx.effective_policy();
        // Snapshot version via the ArcSwap would require exposing the
        // runtime's handle; instead we stash it in an invisible way:
        // the RuntimeSnapshot that the intake pinned for THIS turn is
        // the one whose effective.binding_index drives tool_allowed.
        // The coordinator hook test below reads the version directly
        // from the runtime handle.
        self.out.lock().unwrap().push(Capture {
            allowed_tools: eff.allowed_tools.clone(),
            version: 0, // see comment above; filled via handle in assertions
            text: msg.text.clone(),
        });
        Ok(())
    }

    async fn on_heartbeat(&self, _ctx: &AgentContext) -> anyhow::Result<()> {
        Ok(())
    }

    async fn decide(&self, _ctx: &AgentContext, msg: &InboundMessage) -> anyhow::Result<String> {
        Ok(msg.text.clone())
    }
}

fn base_agent() -> AgentConfig {
    AgentConfig {
        id: "ana".into(),
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
        system_prompt: "You are Ana.".into(),
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
        allowed_tools: vec!["old_tool".into()],
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
        inbound_bindings: vec![InboundBinding {
            plugin: "whatsapp".into(),
            allowed_tools: Some(vec!["old_tool".into()]),
            sender_rate_limit: SenderRateLimitOverride::default(),
            ..Default::default()
        }],
        context_optimization: None,
        dispatch_policy: Default::default(),
        plan_mode: Default::default(),
        remote_triggers: Vec::new(),
        lsp: nexo_config::types::lsp::LspPolicy::default(),
        config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
        team: nexo_config::types::team::TeamPolicy::default(),
        proactive: Default::default(),
    }
}

async fn publish(broker: &AnyBroker, topic: &str, text: &str) {
    let mut ev = Event::new(topic, "ext", json!({ "text": text, "from": "user-1" }));
    ev.session_id = Some(Uuid::new_v4());
    broker.publish(topic, ev).await.unwrap();
}

#[tokio::test]
async fn reload_apply_command_is_picked_up_by_next_message() {
    let broker = AnyBroker::local();
    let out: Arc<Mutex<Vec<Capture>>> = Arc::new(Mutex::new(Vec::new()));
    let behavior = Recorder {
        out: Arc::clone(&out),
    };
    let cfg = base_agent();
    let agent = Arc::new(Agent::new(cfg, behavior));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent.clone(), broker.clone(), sessions);
    let reload_tx = runtime.reload_sender();
    let snapshot_handle = runtime.snapshot_handle();
    runtime.start().await.unwrap();

    // Baseline: send a message with the original config (allowed_tools = ["old_tool"]).
    publish(&broker, "plugin.inbound.whatsapp", "first").await;
    sleep(Duration::from_millis(50)).await;

    // Build a mutated config: flip allowed_tools on the same binding.
    let mut new_cfg = base_agent();
    new_cfg.inbound_bindings[0].allowed_tools = Some(vec!["new_tool".into()]);
    let new_snap = Arc::new(RuntimeSnapshot::bare(Arc::new(new_cfg), 42));
    reload_tx
        .send(ReloadCommand::Apply(new_snap))
        .await
        .unwrap();

    // Give the runtime a beat to process the reload command, then send
    // the next message — it must see the new policy.
    sleep(Duration::from_millis(80)).await;
    publish(&broker, "plugin.inbound.whatsapp", "second").await;
    sleep(Duration::from_millis(80)).await;

    runtime.stop().await;

    let snap = out.lock().unwrap();
    assert_eq!(snap.len(), 2, "both messages must reach the behavior");
    let first = &snap[0];
    let second = &snap[1];
    assert_eq!(
        first.allowed_tools,
        vec!["old_tool".to_string()],
        "first message uses the initial snapshot"
    );
    assert_eq!(
        second.allowed_tools,
        vec!["new_tool".to_string()],
        "second message picks up the reloaded snapshot"
    );

    // The ArcSwap handle is now pointing at version 42.
    assert_eq!(snapshot_handle.load().version, 42);
    // Silence unused warning on Capture.version — the field lives so
    // a future assertion that exposes per-turn version via ctx can
    // use it without a struct change.
    let _ = first.version;
    let _ = second.version;
}

#[tokio::test]
async fn reload_sender_send_does_not_block_when_queue_is_full() {
    // Smoke test the channel capacity: send a handful of Apply
    // commands and verify they all eventually land. Exposes starvation
    // regressions if the biased select arm ever changes.
    let broker = AnyBroker::local();
    let out: Arc<Mutex<Vec<Capture>>> = Arc::new(Mutex::new(Vec::new()));
    let cfg = base_agent();
    let agent = Arc::new(Agent::new(cfg, Recorder { out }));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent.clone(), broker.clone(), sessions);
    let reload_tx = runtime.reload_sender();
    let handle = runtime.snapshot_handle();
    runtime.start().await.unwrap();

    for v in 1..=5u64 {
        let mut c = base_agent();
        c.inbound_bindings[0].allowed_tools = Some(vec![format!("t_{v}")]);
        let snap = Arc::new(RuntimeSnapshot::bare(Arc::new(c), v));
        reload_tx.send(ReloadCommand::Apply(snap)).await.unwrap();
    }
    sleep(Duration::from_millis(100)).await;

    // The last swap is what remains visible.
    assert_eq!(handle.load().version, 5);
    runtime.stop().await;
}

fn write_file(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

#[tokio::test]
async fn reload_coordinator_auto_applies_legacy_yaml_schema_before_swap() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "agents.yaml",
        r#"
agent:
  id: "ana"
  model:
    provider: "anthropic"
    model: "claude-haiku-4-5"
  inbound_binding:
    plugin: "whatsapp"
    allowed_tool: "new_tool"
"#,
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

    let broker = AnyBroker::local();
    let out: Arc<Mutex<Vec<Capture>>> = Arc::new(Mutex::new(Vec::new()));
    let behavior = Recorder {
        out: Arc::clone(&out),
    };
    let cfg = base_agent();
    let agent = Arc::new(Agent::new(cfg, behavior));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent.clone(), broker.clone(), sessions);
    runtime.start().await.unwrap();

    publish(&broker, "plugin.inbound.whatsapp", "before").await;
    sleep(Duration::from_millis(60)).await;

    let coord = Arc::new(ConfigReloadCoordinator::new(
        dir.path().to_path_buf(),
        Arc::new(LlmRegistry::with_builtins()),
        CancellationToken::new(),
    ));
    coord.register(
        "ana",
        runtime.reload_sender(),
        Arc::new(vec!["old_tool".to_string(), "new_tool".to_string()]),
    );

    let outcome = coord.reload().await;
    assert_eq!(outcome.applied, vec!["ana".to_string()], "{:#?}", outcome);
    assert!(outcome.rejected.is_empty(), "{:#?}", outcome.rejected);

    sleep(Duration::from_millis(80)).await;
    publish(&broker, "plugin.inbound.whatsapp", "after").await;
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    let got = out.lock().unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].allowed_tools, vec!["old_tool".to_string()]);
    assert_eq!(got[1].allowed_tools, vec!["new_tool".to_string()]);

    let migrated_agents = fs::read_to_string(dir.path().join("agents.yaml")).unwrap();
    assert!(migrated_agents.contains("schema_version: 11"));
    assert!(migrated_agents.contains("agents:"));
}

#[tokio::test]
async fn reload_coordinator_without_auto_apply_rejects_legacy_yaml_and_keeps_files() {
    let dir = tempfile::tempdir().unwrap();
    let legacy_agents = r#"
agent:
  id: "ana"
  model:
    provider: "anthropic"
    model: "claude-haiku-4-5"
  inbound_binding:
    plugin: "whatsapp"
    allowed_tool: "new_tool"
"#;
    write_file(dir.path(), "agents.yaml", legacy_agents);
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
  auto_apply: false
"#,
    );

    let broker = AnyBroker::local();
    let out: Arc<Mutex<Vec<Capture>>> = Arc::new(Mutex::new(Vec::new()));
    let behavior = Recorder {
        out: Arc::clone(&out),
    };
    let cfg = base_agent();
    let agent = Arc::new(Agent::new(cfg, behavior));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent.clone(), broker.clone(), sessions);
    runtime.start().await.unwrap();

    publish(&broker, "plugin.inbound.whatsapp", "before").await;
    sleep(Duration::from_millis(60)).await;

    let coord = Arc::new(ConfigReloadCoordinator::new(
        dir.path().to_path_buf(),
        Arc::new(LlmRegistry::with_builtins()),
        CancellationToken::new(),
    ));
    coord.register(
        "ana",
        runtime.reload_sender(),
        Arc::new(vec!["old_tool".to_string(), "new_tool".to_string()]),
    );

    let outcome = coord.reload().await;
    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.rejected.len(), 1);
    assert!(outcome.rejected[0].agent_id.is_none());
    assert!(outcome.rejected[0].reason.contains("AppConfig::load"));

    sleep(Duration::from_millis(80)).await;
    publish(&broker, "plugin.inbound.whatsapp", "after").await;
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    let got = out.lock().unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].allowed_tools, vec!["old_tool".to_string()]);
    assert_eq!(got[1].allowed_tools, vec!["old_tool".to_string()]);

    let agents_after = fs::read_to_string(dir.path().join("agents.yaml")).unwrap();
    assert_eq!(agents_after, legacy_agents);
    assert!(!agents_after.contains("schema_version:"));
}
