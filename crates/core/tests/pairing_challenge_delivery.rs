//! Phase 26.x — pairing challenge delivery test.
//!
//! Verifies that when the pairing gate issues a `Challenge`, the
//! runtime delegates to the registered `PairingChannelAdapter` (when
//! present), and otherwise falls back to a hardcoded broker publish on
//! `plugin.outbound.{channel}`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nexo_broker::{types::Event, AnyBroker, BrokerHandle};
use nexo_config::types::agents::{
    AgentConfig, AgentRuntimeConfig, HeartbeatConfig, InboundBinding, ModelConfig,
    OutboundAllowlistConfig,
};
use nexo_core::agent::{Agent, AgentBehavior, AgentContext, AgentRuntime, InboundMessage};
use nexo_core::session::SessionManager;
use nexo_pairing::adapter::PairingChannelAdapter;
use nexo_pairing::{PairingAdapterRegistry, PairingGate, PairingStore};
use serde_json::json;
use tokio::time::sleep;
use uuid::Uuid;

#[derive(Default)]
struct Noop;

#[async_trait]
impl AgentBehavior for Noop {
    async fn on_message(&self, _ctx: &AgentContext, _msg: InboundMessage) -> anyhow::Result<()> {
        Ok(())
    }
    async fn on_heartbeat(&self, _ctx: &AgentContext) -> anyhow::Result<()> {
        Ok(())
    }
    async fn decide(&self, _ctx: &AgentContext, msg: &InboundMessage) -> anyhow::Result<String> {
        Ok(msg.text.clone())
    }
}

#[derive(Default)]
struct CapturingAdapter {
    calls: Mutex<Vec<(String, String, String)>>, // (account, to, text)
}

#[async_trait]
impl PairingChannelAdapter for CapturingAdapter {
    fn channel_id(&self) -> &'static str {
        "whatsapp"
    }
    fn normalize_sender(&self, raw: &str) -> Option<String> {
        // Strip a fictional `@c.us` suffix the test uses to assert the
        // adapter's normalisation actually ran.
        let s = raw.strip_suffix("@c.us").unwrap_or(raw);
        Some(if s.starts_with('+') {
            s.to_string()
        } else {
            format!("+{s}")
        })
    }
    fn format_challenge_text(&self, code: &str) -> String {
        format!("CUSTOM:{code}")
    }
    async fn send_reply(&self, account: &str, to: &str, text: &str) -> anyhow::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push((account.to_string(), to.to_string(), text.to_string()));
        Ok(())
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
    }
}

async fn publish(broker: &AnyBroker, topic: &str, from: &str, text: &str) {
    let mut ev = Event::new(topic, "ext", json!({ "text": text, "from": from }));
    ev.session_id = Some(Uuid::new_v4());
    broker.publish(topic, ev).await.unwrap();
}

#[tokio::test]
async fn challenge_delivers_via_registered_adapter() {
    let broker = AnyBroker::local();
    let store = Arc::new(PairingStore::open_memory().await.unwrap());
    let gate = Arc::new(PairingGate::new(Arc::clone(&store)));
    let adapter = Arc::new(CapturingAdapter::default());
    let registry = PairingAdapterRegistry::new();
    registry.register(adapter.clone() as Arc<dyn PairingChannelAdapter>);

    let agent = Arc::new(Agent::new(agent_with_pairing_on(), Noop));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent, broker.clone(), sessions)
        .with_pairing_gate(gate)
        .with_pairing_adapters(registry);
    runtime.start().await.unwrap();

    publish(
        &broker,
        "plugin.inbound.whatsapp",
        "573001112222@c.us",
        "hola",
    )
    .await;
    sleep(Duration::from_millis(80)).await;

    {
        let calls = adapter.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "adapter.send_reply should be called once");
        let (account, to, text) = &calls[0];
        assert_eq!(account, "default");
        assert_eq!(to, "+573001112222", "sender id was normalised");
        assert!(
            text.starts_with("CUSTOM:"),
            "format_challenge_text was used"
        );
    }

    runtime.stop().await;
}

#[tokio::test]
async fn challenge_falls_back_to_broker_publish_when_no_adapter() {
    let broker = AnyBroker::local();
    let store = Arc::new(PairingStore::open_memory().await.unwrap());
    let gate = Arc::new(PairingGate::new(Arc::clone(&store)));

    // Subscribe to the outbound topic before spawning so we don't miss
    // the publish.
    let mut sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();

    let agent = Arc::new(Agent::new(agent_with_pairing_on(), Noop));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent, broker.clone(), sessions).with_pairing_gate(gate);
    runtime.start().await.unwrap();

    publish(&broker, "plugin.inbound.whatsapp", "+57111", "hola").await;

    // Wait for the outbound publish.
    let evt = tokio::time::timeout(Duration::from_millis(500), sub.next())
        .await
        .expect("timed out waiting for outbound publish")
        .expect("subscriber dropped");
    let payload = &evt.payload;
    assert_eq!(
        payload.get("kind").and_then(serde_json::Value::as_str),
        Some("text"),
    );
    assert_eq!(
        payload.get("to").and_then(serde_json::Value::as_str),
        Some("+57111"),
    );
    let text = payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("nexo pair approve"),
        "fallback text contains the legacy approve hint: {text}"
    );

    runtime.stop().await;
}
