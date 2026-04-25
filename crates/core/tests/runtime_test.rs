use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_broker::AnyBroker;
use agent_config::types::agents::{
    AgentConfig, AgentRuntimeConfig, HeartbeatConfig, InboundBinding, ModelConfig,
    SenderRateLimitConfig,
};
use agent_core::agent::{Agent, AgentBehavior, AgentContext, AgentRuntime, InboundMessage};
use async_trait::async_trait;
use serde_json::json;
use tokio::time::sleep;
use uuid::Uuid;

use agent_core::session::SessionManager;

struct CaptureBehavior {
    received: Arc<Mutex<Vec<String>>>,
    heartbeats: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentBehavior for CaptureBehavior {
    async fn on_message(&self, _ctx: &AgentContext, msg: InboundMessage) -> anyhow::Result<()> {
        self.received.lock().unwrap().push(msg.text.clone());
        Ok(())
    }

    async fn on_heartbeat(&self, _ctx: &AgentContext) -> anyhow::Result<()> {
        self.heartbeats.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn decide(&self, _ctx: &AgentContext, msg: &InboundMessage) -> anyhow::Result<String> {
        Ok(format!("handled: {}", msg.text))
    }
}

fn make_config(
    debounce_ms: u64,
    queue_cap: usize,
    heartbeat_enabled: bool,
    heartbeat_interval: &str,
) -> AgentConfig {
    AgentConfig {
        id: "test-agent".to_string(),
        model: ModelConfig {
            provider: "minimax".to_string(),
            model: "m2.5".to_string(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig {
            enabled: heartbeat_enabled,
            interval: heartbeat_interval.to_string(),
        },
        config: AgentRuntimeConfig {
            debounce_ms,
            queue_cap,
        },
        system_prompt: String::new(),
        workspace: String::new(),
        skills: vec![],
        skills_dir: "./skills".into(),
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        inbound_bindings: Vec::new(),
        allowed_tools: Vec::new(),
        sender_rate_limit: None,
        allowed_delegates: Vec::new(),
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: Default::default(),
        google_auth: None,
        credentials: Default::default(),
            language: None,
    }
}

fn make_runtime(
    debounce_ms: u64,
    queue_cap: usize,
    received: Arc<Mutex<Vec<String>>>,
    heartbeats: Arc<AtomicUsize>,
    broker: AnyBroker,
) -> AgentRuntime {
    let config = make_config(debounce_ms, queue_cap, false, "5m");
    let behavior = CaptureBehavior {
        received,
        heartbeats,
    };
    let agent = Arc::new(Agent::new(config, behavior));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    AgentRuntime::new(agent, broker, sessions)
}

// ── helpers ──────────────────────────────────────────────────────────────────

use agent_broker::types::Event;
use agent_broker::BrokerHandle;

async fn publish_text(broker: &AnyBroker, session_id: Uuid, text: &str) {
    let mut event = Event::new("plugin.inbound.test", "test", json!({ "text": text }));
    event.session_id = Some(session_id);
    broker.publish("plugin.inbound.test", event).await.unwrap();
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn runtime_dispatches_single_message() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let heartbeats = Arc::new(AtomicUsize::new(0));
    let runtime = make_runtime(
        0,
        32,
        Arc::clone(&received),
        Arc::clone(&heartbeats),
        broker.clone(),
    );
    runtime.start().await.unwrap();

    let session = Uuid::new_v4();
    publish_text(&broker, session, "hello").await;

    sleep(Duration::from_millis(50)).await;
    runtime.stop().await;

    let msgs = received.lock().unwrap();
    assert_eq!(msgs.as_slice(), &["hello"]);
}

#[tokio::test]
async fn runtime_debounce_batches_rapid_messages() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let heartbeats = Arc::new(AtomicUsize::new(0));
    // 100ms debounce so we can test within a reasonable wall-clock window
    let runtime = make_runtime(
        100,
        32,
        Arc::clone(&received),
        Arc::clone(&heartbeats),
        broker.clone(),
    );
    runtime.start().await.unwrap();

    let session = Uuid::new_v4();
    publish_text(&broker, session, "a").await;
    publish_text(&broker, session, "b").await;
    publish_text(&broker, session, "c").await;

    // wait for debounce timer to fire (100ms) + dispatch margin
    sleep(Duration::from_millis(250)).await;
    runtime.stop().await;

    let msgs = received.lock().unwrap();
    // all three arrive because debounce is reset each time,
    // then flushed together after silence
    assert_eq!(msgs.as_slice(), &["a", "b", "c"]);
}

#[tokio::test]
async fn runtime_two_sessions_independent() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let heartbeats = Arc::new(AtomicUsize::new(0));
    let runtime = make_runtime(
        0,
        32,
        Arc::clone(&received),
        Arc::clone(&heartbeats),
        broker.clone(),
    );
    runtime.start().await.unwrap();

    let s1 = Uuid::new_v4();
    let s2 = Uuid::new_v4();
    publish_text(&broker, s1, "from-s1").await;
    publish_text(&broker, s2, "from-s2").await;

    sleep(Duration::from_millis(50)).await;
    runtime.stop().await;

    let msgs = received.lock().unwrap();
    assert_eq!(msgs.len(), 2);
    assert!(msgs.contains(&"from-s1".to_string()));
    assert!(msgs.contains(&"from-s2".to_string()));
}

#[tokio::test]
async fn runtime_stop_flushes_remaining() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let heartbeats = Arc::new(AtomicUsize::new(0));
    // Long debounce — message would normally wait 10s
    let runtime = make_runtime(
        10_000,
        32,
        Arc::clone(&received),
        Arc::clone(&heartbeats),
        broker.clone(),
    );
    runtime.start().await.unwrap();

    let session = Uuid::new_v4();
    publish_text(&broker, session, "pending").await;

    // Give subscriber task time to route the message into the session task
    sleep(Duration::from_millis(80)).await;

    // stop() cancels the token; debounce task should flush the buffer
    runtime.stop().await;

    let msgs = received.lock().unwrap();
    assert_eq!(msgs.as_slice(), &["pending"]);
}

#[tokio::test]
async fn runtime_heartbeat_calls_on_heartbeat() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let heartbeats = Arc::new(AtomicUsize::new(0));
    let config = make_config(0, 32, true, "50ms");
    let behavior = CaptureBehavior {
        received: Arc::clone(&received),
        heartbeats: Arc::clone(&heartbeats),
    };
    let agent = Arc::new(Agent::new(config, behavior));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent, broker, sessions);

    runtime.start().await.unwrap();
    sleep(Duration::from_millis(180)).await;
    runtime.stop().await;

    assert!(heartbeats.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn runtime_routes_delegate_and_returns_result() {
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));

    let behavior_a = CaptureBehavior {
        received: Arc::new(Mutex::new(Vec::new())),
        heartbeats: Arc::new(AtomicUsize::new(0)),
    };
    let agent_a = Arc::new(Agent::new(
        AgentConfig {
            id: "agent-a".to_string(),
            model: ModelConfig {
                provider: "minimax".to_string(),
                model: "m2.5".to_string(),
            },
            plugins: vec![],
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig {
                debounce_ms: 0,
                queue_cap: 32,
            },
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            transcripts_dir: String::new(),
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            outbound_allowlist: Default::default(),
            google_auth: None,
            credentials: Default::default(),
            language: None,
        },
        behavior_a,
    ));
    let runtime_a = AgentRuntime::new(Arc::clone(&agent_a), broker.clone(), Arc::clone(&sessions));

    let behavior_b = CaptureBehavior {
        received: Arc::new(Mutex::new(Vec::new())),
        heartbeats: Arc::new(AtomicUsize::new(0)),
    };
    let agent_b = Arc::new(Agent::new(
        AgentConfig {
            id: "agent-b".to_string(),
            model: ModelConfig {
                provider: "minimax".to_string(),
                model: "m2.5".to_string(),
            },
            plugins: vec![],
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig {
                debounce_ms: 0,
                queue_cap: 32,
            },
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            transcripts_dir: String::new(),
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            outbound_allowlist: Default::default(),
            google_auth: None,
            credentials: Default::default(),
            language: None,
        },
        behavior_b,
    ));
    let runtime_b = AgentRuntime::new(Arc::clone(&agent_b), broker.clone(), Arc::clone(&sessions));

    runtime_a.start().await.unwrap();
    runtime_b.start().await.unwrap();

    let output = runtime_a
        .router()
        .delegate(
            &broker,
            "agent-a",
            "agent-b",
            "collect latest status",
            json!({ "session_id": Uuid::new_v4().to_string() }),
            1_000,
        )
        .await
        .unwrap();

    assert_eq!(output["text"], "handled: collect latest status");

    runtime_a.stop().await;
    runtime_b.stop().await;
}

// ── Multi-agent routing with inbound_bindings ────────────────────────────

/// Build an AgentRuntime pinned to a specific `inbound_bindings` set.
/// Used by the multi-agent routing tests below to assert each runtime
/// only processes events matching its bindings.
fn make_runtime_with_bindings(
    id: &str,
    bindings: Vec<InboundBinding>,
    received: Arc<Mutex<Vec<String>>>,
    broker: AnyBroker,
) -> AgentRuntime {
    let mut config = make_config(0, 32, false, "5m");
    config.id = id.to_string();
    config.inbound_bindings = bindings;
    let behavior = CaptureBehavior {
        received,
        heartbeats: Arc::new(AtomicUsize::new(0)),
    };
    let agent = Arc::new(Agent::new(config, behavior));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    AgentRuntime::new(agent, broker, sessions)
}

async fn publish_on(broker: &AnyBroker, topic: &str, session_id: Uuid, text: &str) {
    let mut event = Event::new(topic, "test", json!({ "text": text }));
    event.session_id = Some(session_id);
    broker.publish(topic, event).await.unwrap();
}

#[tokio::test]
async fn two_agents_receive_only_their_bound_plugin_instances() {
    let broker = AnyBroker::local();

    // Boss: pinned to telegram.boss only.
    let boss_received = Arc::new(Mutex::new(Vec::new()));
    let rt_boss = make_runtime_with_bindings(
        "boss",
        vec![InboundBinding {
            plugin: "telegram".into(),
            instance: Some("boss".into()),
            ..Default::default()
        }],
        Arc::clone(&boss_received),
        broker.clone(),
    );

    // Ventas: pinned to telegram.sales only.
    let ventas_received = Arc::new(Mutex::new(Vec::new()));
    let rt_ventas = make_runtime_with_bindings(
        "ventas",
        vec![InboundBinding {
            plugin: "telegram".into(),
            instance: Some("sales".into()),
            ..Default::default()
        }],
        Arc::clone(&ventas_received),
        broker.clone(),
    );

    // Legacy: no bindings → accepts every inbound (back-compat).
    let legacy_received = Arc::new(Mutex::new(Vec::new()));
    let rt_legacy = make_runtime_with_bindings(
        "legacy",
        Vec::new(),
        Arc::clone(&legacy_received),
        broker.clone(),
    );

    rt_boss.start().await.unwrap();
    rt_ventas.start().await.unwrap();
    rt_legacy.start().await.unwrap();

    // Emit one message on each instance topic + one unrelated plugin.
    let s1 = Uuid::new_v4();
    let s2 = Uuid::new_v4();
    let s3 = Uuid::new_v4();
    let s4 = Uuid::new_v4();
    publish_on(&broker, "plugin.inbound.telegram.boss", s1, "to-boss").await;
    publish_on(&broker, "plugin.inbound.telegram.sales", s2, "to-sales").await;
    publish_on(
        &broker,
        "plugin.inbound.whatsapp",
        s3,
        "to-whatsapp-unbound",
    )
    .await;
    publish_on(&broker, "plugin.inbound.telegram.other", s4, "to-other-bot").await;

    // Debounce is 0 but the select loop + session task are async; give
    // them a tick to drain. 100ms is plenty on local broker.
    sleep(Duration::from_millis(100)).await;

    let boss = boss_received.lock().unwrap().clone();
    let ventas = ventas_received.lock().unwrap().clone();
    let legacy = legacy_received.lock().unwrap().clone();

    assert_eq!(
        boss,
        vec!["to-boss".to_string()],
        "boss should only see its own bot"
    );
    assert_eq!(
        ventas,
        vec!["to-sales".to_string()],
        "ventas should only see its own bot"
    );
    // Legacy wildcard sees every inbound regardless of instance.
    assert_eq!(legacy.len(), 4, "legacy wildcard should receive all 4");
    assert!(legacy.contains(&"to-boss".to_string()));
    assert!(legacy.contains(&"to-sales".to_string()));
    assert!(legacy.contains(&"to-whatsapp-unbound".to_string()));
    assert!(legacy.contains(&"to-other-bot".to_string()));

    rt_boss.stop().await;
    rt_ventas.stop().await;
    rt_legacy.stop().await;
}

#[tokio::test]
async fn plugin_wide_binding_accepts_any_instance() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    // Plugin-wide binding: `telegram` with no instance → matches every
    // telegram bot (legacy single-bot topic AND any instance suffix).
    let rt = make_runtime_with_bindings(
        "any_telegram",
        vec![InboundBinding {
            plugin: "telegram".into(),
            instance: None,
            ..Default::default()
        }],
        Arc::clone(&received),
        broker.clone(),
    );
    rt.start().await.unwrap();

    publish_on(
        &broker,
        "plugin.inbound.telegram",
        Uuid::new_v4(),
        "no-instance",
    )
    .await;
    publish_on(
        &broker,
        "plugin.inbound.telegram.sales",
        Uuid::new_v4(),
        "from-sales",
    )
    .await;
    publish_on(
        &broker,
        "plugin.inbound.whatsapp",
        Uuid::new_v4(),
        "from-whatsapp",
    )
    .await;

    sleep(Duration::from_millis(100)).await;
    let got = received.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        2,
        "plugin-wide binding accepts both telegram topics but not whatsapp"
    );
    assert!(got.contains(&"no-instance".to_string()));
    assert!(got.contains(&"from-sales".to_string()));
    assert!(!got.contains(&"from-whatsapp".to_string()));

    rt.stop().await;
}

async fn publish_from(broker: &AnyBroker, topic: &str, sender: &str, text: &str) {
    let mut event = Event::new(topic, "test", json!({ "text": text, "from": sender }));
    event.session_id = Some(Uuid::new_v4());
    broker.publish(topic, event).await.unwrap();
}

#[tokio::test]
async fn sender_rate_limit_drops_excess_messages_from_same_sender() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    // burst=2, rps=0 → after 2 messages from the same sender, further
    // inbounds are dropped. Isolated per sender, so a different user
    // still gets through.
    let mut config = make_config(0, 32, false, "5m");
    config.id = "throttled".into();
    config.sender_rate_limit = Some(SenderRateLimitConfig { rps: 0.0, burst: 2 });
    let behavior = CaptureBehavior {
        received: Arc::clone(&received),
        heartbeats: Arc::new(AtomicUsize::new(0)),
    };
    let agent = Arc::new(Agent::new(config, behavior));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let rt = AgentRuntime::new(agent, broker.clone(), sessions);
    rt.start().await.unwrap();

    // u1 sends 4 messages — only 2 should make it through.
    for i in 0..4 {
        publish_from(
            &broker,
            "plugin.inbound.telegram",
            "u1",
            &format!("msg-{i}"),
        )
        .await;
    }
    // u2 on different sender bucket — both get through.
    publish_from(&broker, "plugin.inbound.telegram", "u2", "from-u2-a").await;
    publish_from(&broker, "plugin.inbound.telegram", "u2", "from-u2-b").await;

    sleep(Duration::from_millis(150)).await;
    rt.stop().await;

    let got = received.lock().unwrap().clone();
    let u1_count = got.iter().filter(|t| t.starts_with("msg-")).count();
    let u2_count = got.iter().filter(|t| t.starts_with("from-u2-")).count();
    assert_eq!(
        u1_count, 2,
        "u1 should be limited to burst=2 (got {u1_count}): {got:?}"
    );
    assert_eq!(u2_count, 2, "u2 has its own bucket, both pass: {got:?}");
}
