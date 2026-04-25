//! Phase 26 — pairing gate intake test.
//!
//! Verifies that the runtime, when given a `PairingGate` and a binding
//! whose `pairing_policy.auto_challenge=true`, drops events from
//! unknown senders and admits events from senders pre-seeded into the
//! allow_from list.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nexo_broker::{types::Event, AnyBroker, BrokerHandle};
use nexo_config::types::agents::{
    AgentConfig, AgentRuntimeConfig, HeartbeatConfig, InboundBinding, ModelConfig,
    OutboundAllowlistConfig,
};
use nexo_core::agent::{
    Agent, AgentBehavior, AgentContext, AgentRuntime, InboundMessage,
};
use nexo_core::session::SessionManager;
use nexo_pairing::{PairingGate, PairingStore};
use serde_json::json;
use tokio::time::sleep;
use uuid::Uuid;

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

struct CountingBehavior(Arc<Recorder>);

#[async_trait]
impl AgentBehavior for CountingBehavior {
    async fn on_message(&self, _ctx: &AgentContext, msg: InboundMessage) -> anyhow::Result<()> {
        self.0 .0.lock().unwrap().push(msg.text.clone());
        Ok(())
    }
    async fn on_heartbeat(&self, _ctx: &AgentContext) -> anyhow::Result<()> {
        Ok(())
    }
    async fn decide(&self, _ctx: &AgentContext, msg: &InboundMessage) -> anyhow::Result<String> {
        Ok(msg.text.clone())
    }
}

fn agent_with_pairing_on() -> AgentConfig {
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
        skills: vec![],
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        allowed_tools: vec![],
        sender_rate_limit: None,
        allowed_delegates: vec![],
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: OutboundAllowlistConfig {
            whatsapp: vec![],
            telegram: vec![],
        },
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
        inbound_bindings: vec![InboundBinding {
            plugin: "whatsapp".into(),
            allowed_tools: None,
            outbound_allowlist: None,
            skills: None,
            system_prompt_extra: None,
            allowed_delegates: None,
            language: None,
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::json!({ "auto_challenge": true }),
            ..Default::default()
        }],
        context_optimization: None,
    }
}

async fn spawn(
    cfg: AgentConfig,
    gate: Arc<PairingGate>,
) -> (AgentRuntime, Arc<Recorder>, AnyBroker) {
    let broker = AnyBroker::local();
    let recorder = Arc::new(Recorder::default());
    let agent = Arc::new(Agent::new(cfg, CountingBehavior(Arc::clone(&recorder))));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent, broker.clone(), sessions).with_pairing_gate(gate);
    runtime.start().await.unwrap();
    (runtime, recorder, broker)
}

async fn publish(broker: &AnyBroker, topic: &str, from: &str, text: &str) {
    let mut ev = Event::new(topic, "ext", json!({ "text": text, "from": from }));
    ev.session_id = Some(Uuid::new_v4());
    broker.publish(topic, ev).await.unwrap();
}

#[tokio::test]
async fn unknown_sender_dropped_admitted_after_seed() {
    let store = Arc::new(PairingStore::open_memory().await.unwrap());
    let gate = Arc::new(PairingGate::new(Arc::clone(&store)));

    let (runtime, recorder, broker) = spawn(agent_with_pairing_on(), gate).await;

    // First send from an unknown sender — gate issues challenge + drops.
    publish(&broker, "plugin.inbound.whatsapp", "+57111", "hola unknown").await;
    sleep(Duration::from_millis(60)).await;

    assert!(
        recorder.0.lock().unwrap().is_empty(),
        "unknown sender must not reach behavior"
    );

    // Seed sender into allow_from then re-publish; behavior receives.
    store
        .seed("whatsapp", "default", &["+57222".into()])
        .await
        .unwrap();
    publish(&broker, "plugin.inbound.whatsapp", "+57222", "hola known").await;
    sleep(Duration::from_millis(60)).await;

    let captured = recorder.0.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0], "hola known");

    runtime.stop().await;
}
