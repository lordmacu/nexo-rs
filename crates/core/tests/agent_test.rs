use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use agent_broker::AnyBroker;
use agent_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use agent_core::agent::{
    Agent, AgentBehavior, AgentContext, InboundMessage, NoOpAgent, RunTrigger,
};
use agent_core::session::SessionManager;
use async_trait::async_trait;
use uuid::Uuid;

fn test_config(id: &str) -> AgentConfig {
    AgentConfig {
        id: id.to_string(),
        model: ModelConfig {
            provider: "minimax".to_string(),
            model: "MiniMax-M2.5".to_string(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: String::new(),
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
        inbound_bindings: Vec::new(),
        allowed_tools: Vec::new(),
        sender_rate_limit: None,
        allowed_delegates: Vec::new(),
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: Default::default(),
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            language: None,
        context_optimization: None,
    }
}

fn test_ctx(agent_id: &str) -> AgentContext {
    let config = Arc::new(test_config(agent_id));
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 50));
    AgentContext::new(agent_id, config, broker, sessions)
}

#[tokio::test]
async fn inbound_message_new() {
    let session_id = Uuid::new_v4();
    let msg = InboundMessage::new(session_id, "kate", "hello");

    assert_eq!(msg.session_id, session_id);
    assert_eq!(msg.agent_id, "kate");
    assert_eq!(msg.text, "hello");
    assert_eq!(msg.trigger, RunTrigger::User);
    assert!(msg.sender_id.is_none());
}

#[tokio::test]
async fn noop_agent_on_message() {
    let agent = NoOpAgent;
    let ctx = test_ctx("kate");
    let msg = InboundMessage::new(Uuid::new_v4(), "kate", "ping");

    let result = agent.on_message(&ctx, msg).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn noop_agent_default_methods() {
    use agent_broker::Event;
    use serde_json::json;

    let agent = NoOpAgent;
    let ctx = test_ctx("kate");

    assert!(agent.on_heartbeat(&ctx).await.is_ok());
    let event = Event::new("test.topic", "test", json!({}));
    assert!(agent.on_event(&ctx, event).await.is_ok());
    let msg = InboundMessage::new(Uuid::new_v4(), "kate", "decide?");
    let result = agent.decide(&ctx, &msg).await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn agent_new() {
    let config = test_config("ventas");
    let agent = Agent::new(config, NoOpAgent);

    assert_eq!(agent.id, "ventas");
    assert_eq!(agent.config.id, "ventas");
}

#[tokio::test]
async fn custom_behavior_called() {
    struct TrackingAgent {
        called: Arc<AtomicBool>,
    }

    #[async_trait]
    impl AgentBehavior for TrackingAgent {
        async fn on_message(
            &self,
            _ctx: &AgentContext,
            _msg: InboundMessage,
        ) -> anyhow::Result<()> {
            self.called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    let called = Arc::new(AtomicBool::new(false));
    let agent = TrackingAgent {
        called: Arc::clone(&called),
    };
    let ctx = test_ctx("kate");
    let msg = InboundMessage::new(Uuid::new_v4(), "kate", "test");

    agent.on_message(&ctx, msg).await.unwrap();
    assert!(called.load(Ordering::SeqCst));
}
