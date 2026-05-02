use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use nexo_broker::{AnyBroker, BrokerHandle};
use nexo_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use nexo_core::agent::tool_policy::{CacheConfig, ParallelConfig, ToolPolicy, ToolPolicyConfig};
use nexo_core::agent::{
    Agent, AgentBehavior, AgentContext, AgentMessage, AgentPayload, AgentRouter, AgentRuntime,
    DelegationTool, InboundMessage, LlmAgentBehavior, ToolHandler, ToolRegistry,
};
use nexo_core::session::SessionManager;
use nexo_llm::{
    ChatRequest, ChatResponse, FinishReason, LlmClient, ResponseContent, TokenUsage, ToolCall,
    ToolDef,
};
use nexo_memory::LongTermMemory;
use serde_json::Value;
use uuid::Uuid;

// ── Stub LLM: always returns a fixed text ────────────────────────────────────

struct StubLlm(String);

#[async_trait]
impl LlmClient for StubLlm {
    async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            content: ResponseContent::Text(self.0.clone()),
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Stop,
            cache_usage: None,
        })
    }
    fn model_id(&self) -> &str {
        "stub"
    }
}

// ── Stub LLM: first call returns tool use, second returns text ───────────────

use std::sync::Mutex;

struct ToolThenTextLlm {
    calls: Mutex<u32>,
}

#[async_trait]
impl LlmClient for ToolThenTextLlm {
    async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let mut c = self.calls.lock().unwrap();
        *c += 1;
        if *c == 1 {
            Ok(ChatResponse {
                content: ResponseContent::ToolCalls(vec![ToolCall {
                    id: "tc1".into(),
                    name: "ping".into(),
                    arguments: serde_json::json!({}),
                }]),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::ToolUse,
                cache_usage: None,
            })
        } else {
            Ok(ChatResponse {
                content: ResponseContent::Text("pong".into()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
    }
    fn model_id(&self) -> &str {
        "tool-stub"
    }
}

struct ReminderThenTextLlm;

#[async_trait]
impl LlmClient for ReminderThenTextLlm {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let has_tool_result = req
            .messages
            .iter()
            .any(|m| m.role == nexo_llm::ChatRole::Tool);
        if !has_tool_result {
            Ok(ChatResponse {
                content: ResponseContent::ToolCalls(vec![ToolCall {
                    id: "rem1".into(),
                    name: "schedule_reminder".into(),
                    arguments: serde_json::json!({
                        "at": "10m",
                        "message": "drink water"
                    }),
                }]),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::ToolUse,
                cache_usage: None,
            })
        } else {
            Ok(ChatResponse {
                content: ResponseContent::Text("ok, te lo recordaré".into()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
    }

    fn model_id(&self) -> &str {
        "reminder-stub"
    }
}

struct DelegateThenTextLlm;

#[async_trait]
impl LlmClient for DelegateThenTextLlm {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let has_tool_result = req
            .messages
            .iter()
            .any(|m| m.role == nexo_llm::ChatRole::Tool);
        if !has_tool_result {
            Ok(ChatResponse {
                content: ResponseContent::ToolCalls(vec![ToolCall {
                    id: "d1".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "agent_id": "agent-b",
                        "task": "analyze this"
                    }),
                }]),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::ToolUse,
                cache_usage: None,
            })
        } else {
            Ok(ChatResponse {
                content: ResponseContent::Text("delegated and done".into()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
    }

    fn model_id(&self) -> &str {
        "delegate-stub"
    }
}

// ── Helper ────────────────────────────────────────────────────────────────────

fn make_context(broker: AnyBroker) -> AgentContext {
    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    AgentContext::new("test-agent", cfg, broker, sessions)
}

fn make_msg(session_id: Uuid, plugin: &str) -> InboundMessage {
    let mut m = InboundMessage::new(session_id, "test-agent", "hello");
    m.source_plugin = plugin.to_string();
    m.sender_id = Some("user-1".into());
    m
}

/// `make_msg` variant without a `sender_id`. The behavior injects a
/// `# CONTEXTO DEL CANAL` System block whenever a sender id is present
/// — tests focused on system_prompt assembly use this builder so the
/// channel-context block doesn't show up in their assertions.
fn make_msg_no_sender(session_id: Uuid, plugin: &str) -> InboundMessage {
    let mut m = InboundMessage::new(session_id, "test-agent", "hello");
    m.source_plugin = plugin.to_string();
    m.sender_id = None;
    m
}

// ── Tests ─────────────────────────────────────────────────────────────────────

// Spy that captures every ChatRequest sent to it. Used to assert prompt shape.
struct CapturingLlm {
    captured: Arc<Mutex<Vec<ChatRequest>>>,
    reply: String,
}

#[async_trait]
impl LlmClient for CapturingLlm {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        self.captured.lock().unwrap().push(req);
        Ok(ChatResponse {
            content: ResponseContent::Text(self.reply.clone()),
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Stop,
            cache_usage: None,
        })
    }
    fn model_id(&self) -> &str {
        "capturing-stub"
    }
}

#[tokio::test]
async fn system_prompt_prepended_to_llm_request() {
    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();

    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: "You are Kate, a caring assistant.".into(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("test-agent", cfg, broker, sessions);

    let captured: Arc<Mutex<Vec<ChatRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingLlm {
        captured: Arc::clone(&captured),
        reply: "ok".into(),
    };
    let behavior = LlmAgentBehavior::new(
        Arc::new(llm) as Arc<dyn LlmClient>,
        Arc::new(ToolRegistry::new()),
    );

    // No sender_id so the behavior's '# CONTEXTO DEL CANAL' block
    // doesn't append; this test is specifically asserting the
    // system_prompt assembly path.
    behavior
        .on_message(&ctx, make_msg_no_sender(Uuid::new_v4(), "whatsapp"))
        .await
        .unwrap();

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let first = &reqs[0].messages[0];
    assert_eq!(first.role, nexo_llm::ChatRole::System);
    assert_eq!(first.content, "You are Kate, a caring assistant.");
}

#[tokio::test]
async fn output_language_directive_renders_when_configured() {
    // Agent with language="es" should produce a System message that
    // contains both the persona prompt AND a `# OUTPUT LANGUAGE`
    // block telling the model to reply in Spanish.
    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();

    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: "You are Kate.".into(),
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
        pairing_policy: serde_json::Value::Null,
        language: Some("es".into()),
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("test-agent", cfg, broker, sessions);

    let captured: Arc<Mutex<Vec<ChatRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingLlm {
        captured: Arc::clone(&captured),
        reply: "ok".into(),
    };
    let behavior = LlmAgentBehavior::new(
        Arc::new(llm) as Arc<dyn LlmClient>,
        Arc::new(ToolRegistry::new()),
    );

    behavior
        .on_message(&ctx, make_msg_no_sender(Uuid::new_v4(), "whatsapp"))
        .await
        .unwrap();

    let reqs = captured.lock().unwrap();
    let system = reqs[0]
        .messages
        .iter()
        .find(|m| m.role == nexo_llm::ChatRole::System)
        .expect("at least one System message");
    assert!(
        system.content.contains("You are Kate."),
        "persona block present"
    );
    assert!(
        system.content.contains("# OUTPUT LANGUAGE"),
        "language block header present, got:\n{}",
        system.content
    );
    assert!(
        system.content.contains("Respond to the user in es"),
        "directive mentions the configured language"
    );
}

#[tokio::test]
async fn empty_system_prompt_emits_no_system_message() {
    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();

    let ctx = make_context(broker); // system_prompt = ""
    let captured: Arc<Mutex<Vec<ChatRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingLlm {
        captured: Arc::clone(&captured),
        reply: "ok".into(),
    };
    let behavior = LlmAgentBehavior::new(
        Arc::new(llm) as Arc<dyn LlmClient>,
        Arc::new(ToolRegistry::new()),
    );

    // Same rationale as the test above: skip the channel-context
    // block by feeding a message with no sender_id, so this test is
    // strictly about the empty system_prompt path.
    behavior
        .on_message(&ctx, make_msg_no_sender(Uuid::new_v4(), "whatsapp"))
        .await
        .unwrap();

    let reqs = captured.lock().unwrap();
    assert!(
        reqs[0]
            .messages
            .iter()
            .all(|m| m.role != nexo_llm::ChatRole::System),
        "no System message expected when system_prompt is empty"
    );
}

#[tokio::test]
async fn workspace_bundle_prepended_to_system_message() -> anyhow::Result<()> {
    // Create a workspace with IDENTITY.md + SOUL.md + MEMORY.md and verify
    // the persona blocks land in the first System message, and that
    // system_prompt (if any) is concatenated after them.
    let tmp = std::env::temp_dir().join(format!("llm-behavior-ws-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&tmp).await?;
    tokio::fs::write(
        tmp.join("IDENTITY.md"),
        "- **Name:** Kate\n- **Emoji:** 🐙\n",
    )
    .await?;
    tokio::fs::write(tmp.join("SOUL.md"), "have opinions, skip filler").await?;
    tokio::fs::write(
        tmp.join("MEMORY.md"),
        "Cristian prefers Spanish conversations.",
    )
    .await?;

    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();
    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: "Always reply in the user's language.".into(),
        workspace: tmp.to_string_lossy().into_owned(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("test-agent", cfg, broker, sessions);

    let captured: Arc<Mutex<Vec<ChatRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingLlm {
        captured: Arc::clone(&captured),
        reply: "ok".into(),
    };
    let behavior = LlmAgentBehavior::new(
        Arc::new(llm) as Arc<dyn LlmClient>,
        Arc::new(ToolRegistry::new()),
    );

    behavior
        .on_message(&ctx, make_msg(Uuid::new_v4(), "whatsapp"))
        .await
        .unwrap();

    {
        let reqs = captured.lock().unwrap();
        let system = &reqs[0].messages[0];
        assert_eq!(system.role, nexo_llm::ChatRole::System);
        let body = &system.content;
        assert!(
            body.contains("# IDENTITY"),
            "IDENTITY block missing: {body}"
        );
        assert!(body.contains("name=Kate"));
        assert!(body.contains("# SOUL"));
        assert!(body.contains("have opinions"));
        assert!(body.contains("# MEMORY"));
        assert!(body.contains("Cristian prefers Spanish"));
        // system_prompt must come after workspace blocks.
        let soul_idx = body.find("# SOUL").unwrap();
        let sp_idx = body.find("Always reply in the user's language").unwrap();
        assert!(
            sp_idx > soul_idx,
            "system_prompt must come after workspace blocks"
        );
    }

    tokio::fs::remove_dir_all(&tmp).await.ok();
    Ok(())
}

#[tokio::test]
async fn skills_loaded_between_workspace_and_system_prompt() -> anyhow::Result<()> {
    let workspace = std::env::temp_dir().join(format!("llm-behavior-skills-ws-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&workspace).await?;
    tokio::fs::write(workspace.join("SOUL.md"), "be concise").await?;

    let skills_root =
        std::env::temp_dir().join(format!("llm-behavior-skills-root-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(skills_root.join("weather")).await?;
    tokio::fs::write(
        skills_root.join("weather").join("SKILL.md"),
        "Use this skill for weather requests only.",
    )
    .await?;

    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();
    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: "Always answer in Spanish.".into(),
        workspace: workspace.to_string_lossy().into_owned(),
        skills: vec!["weather".into()],
        skills_dir: skills_root.to_string_lossy().into_owned(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("test-agent", cfg, broker, sessions);

    let captured: Arc<Mutex<Vec<ChatRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingLlm {
        captured: Arc::clone(&captured),
        reply: "ok".into(),
    };
    let behavior = LlmAgentBehavior::new(
        Arc::new(llm) as Arc<dyn LlmClient>,
        Arc::new(ToolRegistry::new()),
    );

    behavior
        .on_message(&ctx, make_msg(Uuid::new_v4(), "whatsapp"))
        .await
        .unwrap();

    {
        let reqs = captured.lock().unwrap();
        let body = &reqs[0].messages[0].content;
        assert!(body.contains("# SOUL"));
        assert!(body.contains("# SKILLS"));
        assert!(body.contains("## weather"));
        assert!(body.contains("Use this skill for weather requests only."));
        let soul_idx = body.find("# SOUL").unwrap();
        let skills_idx = body.find("# SKILLS").unwrap();
        let prompt_idx = body.find("Always answer in Spanish.").unwrap();
        assert!(
            skills_idx > soul_idx,
            "skills block must come after workspace"
        );
        assert!(
            prompt_idx > skills_idx,
            "system_prompt must come after skills"
        );
    }

    tokio::fs::remove_dir_all(&workspace).await.ok();
    tokio::fs::remove_dir_all(&skills_root).await.ok();
    Ok(())
}

#[tokio::test]
async fn workspace_memory_skipped_when_source_is_peer_agent() -> anyhow::Result<()> {
    // When another agent delegates (source_plugin = "agent"), MEMORY.md must
    // not appear in the system prompt — privacy boundary.
    let tmp = std::env::temp_dir().join(format!("llm-behavior-peer-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&tmp).await?;
    tokio::fs::write(tmp.join("SOUL.md"), "be helpful").await?;
    tokio::fs::write(tmp.join("MEMORY.md"), "PRIVATE: user's bank info").await?;

    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();
    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: String::new(),
        workspace: tmp.to_string_lossy().into_owned(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("test-agent", cfg, broker, sessions);

    let captured: Arc<Mutex<Vec<ChatRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingLlm {
        captured: Arc::clone(&captured),
        reply: "ok".into(),
    };
    let behavior = LlmAgentBehavior::new(
        Arc::new(llm) as Arc<dyn LlmClient>,
        Arc::new(ToolRegistry::new()),
    );

    // Simulate delegation from another agent.
    let mut msg = InboundMessage::new(Uuid::new_v4(), "test-agent", "hola");
    msg.source_plugin = "agent".into();
    msg.sender_id = Some("peer-agent".into());
    behavior.on_message(&ctx, msg).await.unwrap();

    {
        let reqs = captured.lock().unwrap();
        let body = &reqs[0].messages[0].content;
        assert!(body.contains("# SOUL"), "SOUL.md should still be present");
        assert!(
            !body.contains("PRIVATE: user's bank info"),
            "MEMORY.md must not leak via peer-agent delegation"
        );
    }

    tokio::fs::remove_dir_all(&tmp).await.ok();
    Ok(())
}

#[tokio::test]
async fn transcript_written_when_dir_configured() -> anyhow::Result<()> {
    use nexo_core::agent::{TranscriptLine, TranscriptRole, TranscriptWriter};

    let transcripts =
        std::env::temp_dir().join(format!("llm-behavior-transcripts-{}", Uuid::new_v4()));

    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();
    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: String::new(),
        workspace: String::new(),
        skills: vec![],
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        transcripts_dir: transcripts.to_string_lossy().into_owned(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("test-agent", cfg, broker, sessions);
    let behavior = LlmAgentBehavior::new(
        Arc::new(StubLlm("hola, cómo estás?".into())) as Arc<dyn LlmClient>,
        Arc::new(ToolRegistry::new()),
    );

    let session_id = Uuid::new_v4();
    let msg = make_msg(session_id, "whatsapp");
    behavior.on_message(&ctx, msg).await.unwrap();

    let writer = TranscriptWriter::new(&transcripts, "test-agent");
    let lines = writer.read_session(session_id).await?;
    assert_eq!(lines.len(), 3, "header + user + assistant expected");

    let roles: Vec<TranscriptRole> = lines
        .iter()
        .filter_map(|l| match l {
            TranscriptLine::Entry(e) => Some(e.role),
            _ => None,
        })
        .collect();
    assert_eq!(roles, vec![TranscriptRole::User, TranscriptRole::Assistant]);

    tokio::fs::remove_dir_all(&transcripts).await.ok();
    Ok(())
}

#[tokio::test]
async fn text_reply_published_to_outbound_topic() {
    let broker = AnyBroker::local();
    let mut sub = broker.subscribe("plugin.outbound.whatsapp").await.unwrap();

    let ctx = make_context(broker);
    let tools = Arc::new(ToolRegistry::new());
    let behavior = LlmAgentBehavior::new(
        Arc::new(StubLlm("hello back".into())) as Arc<dyn LlmClient>,
        tools,
    );

    let msg = make_msg(Uuid::new_v4(), "whatsapp");
    behavior.on_message(&ctx, msg).await.unwrap();

    let event = tokio::time::timeout(Duration::from_millis(200), sub.next())
        .await
        .expect("timed out")
        .expect("no event");

    assert_eq!(event.topic, "plugin.outbound.whatsapp");
    assert_eq!(event.payload["text"], "hello back");
    assert_eq!(event.payload["to"], "user-1");
}

#[tokio::test]
async fn tool_call_then_text_reply() {
    let broker = AnyBroker::local();
    let mut sub = broker.subscribe("plugin.outbound.telegram").await.unwrap();

    let ctx = make_context(broker);
    let tools = Arc::new(ToolRegistry::new());

    struct PingHandler;
    #[async_trait]
    impl ToolHandler for PingHandler {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            Ok(serde_json::json!("pong"))
        }
    }
    tools.register(
        ToolDef {
            name: "ping".into(),
            description: "ping".into(),
            parameters: serde_json::json!({}),
        },
        PingHandler,
    );

    let behavior = LlmAgentBehavior::new(
        Arc::new(ToolThenTextLlm {
            calls: Mutex::new(0),
        }) as Arc<dyn LlmClient>,
        tools,
    );

    let mut msg = InboundMessage::new(Uuid::new_v4(), "test-agent", "ping please");
    msg.source_plugin = "telegram".into();
    msg.sender_id = Some("u2".into());

    behavior.on_message(&ctx, msg).await.unwrap();

    let event = tokio::time::timeout(Duration::from_millis(200), sub.next())
        .await
        .expect("timed out")
        .expect("no event");

    assert_eq!(event.payload["text"], "pong");
}

#[tokio::test]
async fn session_history_persists_across_messages() {
    let broker = AnyBroker::local();
    let mut _sub = broker.subscribe("plugin.outbound.test").await.unwrap();

    let ctx = make_context(broker);
    let tools = Arc::new(ToolRegistry::new());
    let behavior =
        LlmAgentBehavior::new(Arc::new(StubLlm("ok".into())) as Arc<dyn LlmClient>, tools);

    let session_id = Uuid::new_v4();
    for _ in 0..3 {
        let msg = make_msg(session_id, "test");
        behavior.on_message(&ctx, msg).await.unwrap();
    }

    let session = ctx.sessions.get(session_id).unwrap();
    // 3 user + 3 assistant turns = 6
    assert_eq!(session.history.len(), 6);
}

#[tokio::test]
async fn heartbeat_delivers_due_reminders_once() {
    let broker = AnyBroker::local();
    let mut sub = broker.subscribe("plugin.outbound.telegram").await.unwrap();

    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let memory = Arc::new(LongTermMemory::open(":memory:").await.unwrap());
    let ctx =
        AgentContext::new("test-agent", cfg, broker, sessions).with_memory(Arc::clone(&memory));

    let session_id = Uuid::new_v4();
    let reminder_id = memory
        .schedule_reminder(
            "test-agent",
            session_id,
            "telegram",
            "u-heartbeat",
            "drink water",
            Utc::now() - ChronoDuration::seconds(1),
        )
        .await
        .unwrap();

    let tools = Arc::new(ToolRegistry::new());
    let behavior = LlmAgentBehavior::new(
        Arc::new(StubLlm("unused".into())) as Arc<dyn LlmClient>,
        tools,
    );

    behavior.on_heartbeat(&ctx).await.unwrap();

    let event = tokio::time::timeout(Duration::from_millis(200), sub.next())
        .await
        .expect("timed out")
        .expect("no event");
    assert_eq!(event.payload["text"], "drink water");
    assert_eq!(event.payload["to"], "u-heartbeat");

    let due = memory
        .list_due_reminders("test-agent", Utc::now(), 10)
        .await
        .unwrap();
    assert!(due.is_empty());
    assert!(!memory.mark_reminder_delivered(reminder_id).await.unwrap());

    behavior.on_heartbeat(&ctx).await.unwrap();
    let second = tokio::time::timeout(Duration::from_millis(50), sub.next()).await;
    assert!(second.is_err(), "reminder was delivered twice");
}

#[tokio::test]
async fn schedule_reminder_tool_uses_current_conversation_context() {
    let broker = AnyBroker::local();
    let cfg = Arc::new(AgentConfig {
        id: "test-agent".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let memory = Arc::new(LongTermMemory::open(":memory:").await.unwrap());
    let ctx =
        AgentContext::new("test-agent", cfg, broker, sessions).with_memory(Arc::clone(&memory));

    let tools = Arc::new(ToolRegistry::new());
    tools.register(
        nexo_core::agent::HeartbeatTool::tool_def(),
        nexo_core::agent::HeartbeatTool::new(Arc::clone(&memory)),
    );
    let behavior =
        LlmAgentBehavior::new(Arc::new(ReminderThenTextLlm) as Arc<dyn LlmClient>, tools);

    let mut msg = InboundMessage::new(
        Uuid::new_v4(),
        "test-agent",
        "recuerdame beber agua en 10 minutos",
    );
    msg.source_plugin = "telegram".into();
    msg.sender_id = Some("u-reminder".into());

    behavior.on_message(&ctx, msg.clone()).await.unwrap();

    let due = memory
        .list_due_reminders("test-agent", Utc::now() + ChronoDuration::minutes(11), 10)
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].plugin, "telegram");
    assert_eq!(due[0].recipient, "u-reminder");
    assert_eq!(due[0].session_id, msg.session_id);
    assert_eq!(due[0].message, "drink water");
}

#[tokio::test]
async fn llm_can_call_delegate_tool_and_receive_result() {
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));

    struct ResponderBehavior;
    #[async_trait]
    impl AgentBehavior for ResponderBehavior {
        async fn decide(
            &self,
            _ctx: &AgentContext,
            msg: &InboundMessage,
        ) -> anyhow::Result<String> {
            Ok(format!("agent-b handled: {}", msg.text))
        }
    }

    let cfg_b = AgentConfig {
        id: "agent-b".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    let runtime_b = AgentRuntime::new(
        Arc::new(Agent::new(cfg_b, ResponderBehavior)),
        broker.clone(),
        Arc::clone(&sessions),
    );
    runtime_b.start().await.unwrap();

    let cfg_a = Arc::new(AgentConfig {
        id: "agent-a".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
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
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let mut sub = broker.subscribe("plugin.outbound.telegram").await.unwrap();
    let router = Arc::new(AgentRouter::new());
    let ctx_a = AgentContext::new("agent-a", cfg_a, broker.clone(), Arc::clone(&sessions))
        .with_router(Arc::clone(&router));
    let mut route_sub = broker.subscribe("agent.route.agent-a").await.unwrap();
    let router_resolver = Arc::clone(&router);
    tokio::spawn(async move {
        if let Some(event) = route_sub.next().await {
            if let Ok(msg) = serde_json::from_value::<AgentMessage>(event.payload) {
                if let AgentPayload::Result { output, .. } = msg.payload {
                    let _ = router_resolver.resolve(msg.correlation_id, output);
                }
            }
        }
    });

    let tools = Arc::new(ToolRegistry::new());
    tools.register(DelegationTool::tool_def(), DelegationTool);
    let behavior_a =
        LlmAgentBehavior::new(Arc::new(DelegateThenTextLlm) as Arc<dyn LlmClient>, tools);

    let mut msg = InboundMessage::new(Uuid::new_v4(), "agent-a", "delegate this");
    msg.source_plugin = "telegram".into();
    msg.sender_id = Some("u-delegate".into());
    behavior_a.on_message(&ctx_a, msg).await.unwrap();

    let event = tokio::time::timeout(Duration::from_millis(400), sub.next())
        .await
        .expect("timed out")
        .expect("no event");
    assert_eq!(event.payload["text"], "delegated and done");
    assert_eq!(event.payload["to"], "u-delegate");

    runtime_b.stop().await;
}

// ── ToolPolicy integration tests ────────────────────────────────────────

/// Emits the same tool call twice (once per turn), then an assistant
/// text response. Used to verify that the second invocation hits the
/// cache instead of invoking the handler.
struct ToolCallTwiceThenText {
    calls: Mutex<u32>,
}

#[async_trait]
impl LlmClient for ToolCallTwiceThenText {
    async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let mut c = self.calls.lock().unwrap();
        *c += 1;
        if *c <= 2 {
            Ok(ChatResponse {
                content: ResponseContent::ToolCalls(vec![ToolCall {
                    id: format!("tc{}", *c),
                    name: "ext_cached_echo".into(),
                    arguments: serde_json::json!({"q": "same"}),
                }]),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::ToolUse,
                cache_usage: None,
            })
        } else {
            Ok(ChatResponse {
                content: ResponseContent::Text("done".into()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
    }
    fn model_id(&self) -> &str {
        "cache-stub"
    }
}

/// Emits two parallel tool calls in a single response, then a text
/// reply. Used to verify that `parallel_safe` tools actually run
/// concurrently — the total time must be below 2× the per-call sleep.
struct TwoToolsInOneCall {
    calls: Mutex<u32>,
}

#[async_trait]
impl LlmClient for TwoToolsInOneCall {
    async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let mut c = self.calls.lock().unwrap();
        *c += 1;
        if *c == 1 {
            Ok(ChatResponse {
                content: ResponseContent::ToolCalls(vec![
                    ToolCall {
                        id: "p1".into(),
                        name: "ext_slow_a".into(),
                        arguments: serde_json::json!({}),
                    },
                    ToolCall {
                        id: "p2".into(),
                        name: "ext_slow_b".into(),
                        arguments: serde_json::json!({}),
                    },
                ]),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::ToolUse,
                cache_usage: None,
            })
        } else {
            Ok(ChatResponse {
                content: ResponseContent::Text("both done".into()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
    }
    fn model_id(&self) -> &str {
        "parallel-stub"
    }
}

#[tokio::test]
async fn cached_tool_call_short_circuits_handler() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let broker = AnyBroker::local();
    let mut sub = broker.subscribe("plugin.outbound.telegram").await.unwrap();
    let ctx = make_context(broker);
    let tools = Arc::new(ToolRegistry::new());

    struct CountingEcho {
        calls: Arc<AtomicU32>,
    }
    #[async_trait]
    impl ToolHandler for CountingEcho {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!("echoed"))
        }
    }
    let counter = Arc::new(AtomicU32::new(0));
    tools.register(
        ToolDef {
            name: "ext_cached_echo".into(),
            description: "Echo".into(),
            parameters: serde_json::json!({}),
        },
        CountingEcho {
            calls: Arc::clone(&counter),
        },
    );

    let policy_cfg = ToolPolicyConfig {
        cache: CacheConfig {
            ttl_secs: 60,
            tools: vec!["ext_cached_*".into()],
            max_entries: 10,
            max_value_bytes: 0,
        },
        parallel_safe: vec![],
        parallel: Default::default(),
        relevance: Default::default(),
        per_agent: Default::default(),
    };
    let behavior = LlmAgentBehavior::new(
        Arc::new(ToolCallTwiceThenText {
            calls: Mutex::new(0),
        }) as Arc<dyn LlmClient>,
        tools,
    )
    .with_tool_policy(ToolPolicy::from_config(&policy_cfg));

    let mut msg = InboundMessage::new(Uuid::new_v4(), "test-agent", "echo please");
    msg.source_plugin = "telegram".into();
    msg.sender_id = Some("u-cache".into());

    behavior.on_message(&ctx, msg).await.unwrap();

    // Drain the final text response.
    tokio::time::timeout(Duration::from_millis(500), sub.next())
        .await
        .expect("timed out")
        .expect("no event");

    // Despite 2 identical tool calls, the handler fired only once —
    // the second hit was served from the policy cache.
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "expected one handler invocation (second was cached)"
    );
}

#[tokio::test]
async fn parallel_safe_tools_run_concurrently() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let broker = AnyBroker::local();
    let mut sub = broker.subscribe("plugin.outbound.telegram").await.unwrap();
    let ctx = make_context(broker);
    let tools = Arc::new(ToolRegistry::new());

    struct Sleepy {
        sleep_ms: u64,
        fired: Arc<AtomicU32>,
    }
    #[async_trait]
    impl ToolHandler for Sleepy {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            self.fired.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!("slow-done"))
        }
    }
    let counter = Arc::new(AtomicU32::new(0));
    tools.register(
        ToolDef {
            name: "ext_slow_a".into(),
            description: "slow a".into(),
            parameters: serde_json::json!({}),
        },
        Sleepy {
            sleep_ms: 120,
            fired: Arc::clone(&counter),
        },
    );
    tools.register(
        ToolDef {
            name: "ext_slow_b".into(),
            description: "slow b".into(),
            parameters: serde_json::json!({}),
        },
        Sleepy {
            sleep_ms: 120,
            fired: Arc::clone(&counter),
        },
    );

    let policy_cfg = ToolPolicyConfig {
        cache: CacheConfig::default(),
        parallel_safe: vec!["ext_slow_*".into()],
        parallel: ParallelConfig {
            max_in_flight: 4,
            call_timeout_secs: 5,
        },
        relevance: Default::default(),
        per_agent: Default::default(),
    };
    let behavior = LlmAgentBehavior::new(
        Arc::new(TwoToolsInOneCall {
            calls: Mutex::new(0),
        }) as Arc<dyn LlmClient>,
        tools,
    )
    .with_tool_policy(ToolPolicy::from_config(&policy_cfg));

    let mut msg = InboundMessage::new(Uuid::new_v4(), "test-agent", "run both");
    msg.source_plugin = "telegram".into();
    msg.sender_id = Some("u-par".into());

    let start = std::time::Instant::now();
    behavior.on_message(&ctx, msg).await.unwrap();
    let event = tokio::time::timeout(Duration::from_millis(800), sub.next())
        .await
        .expect("timed out")
        .expect("no event");
    let elapsed = start.elapsed();

    assert_eq!(event.payload["text"], "both done");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    // Both tools sleep 120ms. Sequential would be ≥240ms. Parallel
    // should finish close to 120ms; allow generous slack for CI.
    assert!(
        elapsed < Duration::from_millis(230),
        "parallel execution took {:?} — looks sequential",
        elapsed,
    );
}

// ── Multi-instance outbound routing ─────────────────────────────────────

#[tokio::test]
async fn reply_routes_to_instance_specific_outbound_topic() {
    // When the inbound carries `source_instance = Some("sales")`, the
    // agent's text reply must land on `plugin.outbound.telegram.sales`
    // — NOT the legacy `plugin.outbound.telegram` where a different
    // bot's dispatcher would pick it up.
    let broker = AnyBroker::local();
    let mut sub_sales = broker
        .subscribe("plugin.outbound.telegram.sales")
        .await
        .unwrap();
    let mut sub_legacy = broker.subscribe("plugin.outbound.telegram").await.unwrap();

    let ctx = make_context(broker);
    let tools = Arc::new(ToolRegistry::new());
    let behavior = LlmAgentBehavior::new(
        Arc::new(StubLlm("hola desde ventas".into())) as Arc<dyn LlmClient>,
        tools,
    );

    let mut msg = InboundMessage::new(Uuid::new_v4(), "test-agent", "hi sales");
    msg.source_plugin = "telegram".into();
    msg.source_instance = Some("sales".into());
    msg.sender_id = Some("u1".into());
    behavior.on_message(&ctx, msg).await.unwrap();

    // Reply lands on the sales-specific topic.
    let event = tokio::time::timeout(Duration::from_millis(300), sub_sales.next())
        .await
        .expect("timed out waiting for sales reply")
        .expect("no event on sales topic");
    assert_eq!(event.payload["text"], "hola desde ventas");

    // Legacy topic must NOT receive it — the boss bot would otherwise
    // steal the reply. 50ms is enough to catch any stray publish.
    let race = tokio::time::timeout(Duration::from_millis(50), sub_legacy.next()).await;
    assert!(
        race.is_err(),
        "reply leaked to legacy outbound topic; multi-bot routing broken",
    );
}

#[tokio::test]
async fn reply_falls_back_to_legacy_outbound_when_no_instance() {
    // No `source_instance` = legacy single-bot path. Reply must use
    // `plugin.outbound.telegram` so pre-multi-bot configs keep working.
    let broker = AnyBroker::local();
    let mut sub_legacy = broker.subscribe("plugin.outbound.telegram").await.unwrap();

    let ctx = make_context(broker);
    let tools = Arc::new(ToolRegistry::new());
    let behavior = LlmAgentBehavior::new(
        Arc::new(StubLlm("legacy reply".into())) as Arc<dyn LlmClient>,
        tools,
    );

    let mut msg = InboundMessage::new(Uuid::new_v4(), "test-agent", "hi");
    msg.source_plugin = "telegram".into();
    msg.source_instance = None;
    msg.sender_id = Some("u1".into());
    behavior.on_message(&ctx, msg).await.unwrap();

    let event = tokio::time::timeout(Duration::from_millis(300), sub_legacy.next())
        .await
        .expect("timed out")
        .expect("no event");
    assert_eq!(event.payload["text"], "legacy reply");
}

#[tokio::test]
async fn delegation_rejects_target_outside_allowed_delegates() {
    // agent-a is configured to only delegate to `soporte_*`. Attempt
    // to delegate to `ventas` must fail before any NATS roundtrip.
    use nexo_core::agent::AgentRouter;
    use nexo_core::agent::DelegationTool;

    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let cfg = Arc::new(AgentConfig {
        id: "agent-a".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
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
        allowed_delegates: vec!["soporte_*".into()],
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: Default::default(),
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let router = Arc::new(AgentRouter::new());
    let ctx = AgentContext::new("agent-a", cfg, broker, sessions).with_router(router);

    let tool = DelegationTool;
    let err = tool
        .call(
            &ctx,
            serde_json::json!({
                "agent_id": "ventas",
                "task": "sell something",
            }),
        )
        .await
        .expect_err("delegation should have been rejected by allowlist");
    let err_msg = format!("{err}");
    assert!(
        err_msg.contains("not allowed to delegate"),
        "got: {err_msg}"
    );
    assert!(err_msg.contains("ventas"), "got: {err_msg}");

    // Positive case: delegating to a matching pattern is NOT rejected
    // by the allowlist. (Will still fail due to no peer present, but
    // with a different error — router timeout, not permission denied.)
    let err2 = tool
        .call(
            &ctx,
            serde_json::json!({
                "agent_id": "soporte_nivel1",
                "task": "help this user",
                "timeout_ms": 50,
            }),
        )
        .await
        .expect_err("no peer agent-b running → should still fail");
    let err2_msg = format!("{err2}");
    assert!(
        !err2_msg.contains("not allowed to delegate"),
        "allowlist-matching target should reach router, got permission error: {err2_msg}",
    );
}

#[tokio::test]
async fn peer_directory_renders_into_system_prompt() {
    use nexo_core::agent::{PeerDirectory, PeerSummary};

    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.telegram").await.unwrap();

    let peers = PeerDirectory::new(vec![
        PeerSummary {
            id: "boss".into(),
            description: "takes decisions".into(),
        },
        PeerSummary {
            id: "ventas".into(),
            description: "closes deals".into(),
        },
        PeerSummary {
            id: "soporte_lvl1".into(),
            description: "first-line".into(),
        },
    ]);

    let cfg = Arc::new(AgentConfig {
        id: "ventas".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: "Be helpful.".into(),
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
        allowed_delegates: vec!["soporte_*".into()],
        accept_delegates_from: Vec::new(),
        description: "sales desk".into(),
        outbound_allowlist: Default::default(),
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
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
    });
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("ventas", cfg, broker, sessions).with_peers(peers);

    let captured = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingLlm {
        captured: Arc::clone(&captured),
        reply: "ok".into(),
    };
    let tools = Arc::new(ToolRegistry::new());
    let behavior = LlmAgentBehavior::new(Arc::new(llm) as Arc<dyn LlmClient>, tools);

    let mut msg = InboundMessage::new(Uuid::new_v4(), "ventas", "hi");
    msg.source_plugin = "telegram".into();
    msg.sender_id = Some("u1".into());
    behavior.on_message(&ctx, msg).await.unwrap();

    let req = captured.lock().unwrap().remove(0);
    let sys = req
        .messages
        .iter()
        .find(|m| m.role == nexo_llm::ChatRole::System)
        .expect("system message present");
    let sys_text = sys.content.as_str();
    assert!(
        sys_text.contains("# PEERS"),
        "peers block missing: {sys_text}"
    );
    // Self (`ventas`) filtered out.
    assert!(!sys_text.contains("`ventas`"));
    // Unreachable peer (`boss`) still listed but marked ✗.
    assert!(sys_text.contains("✗ `boss`"));
    // Reachable peer (`soporte_lvl1`) marked ✓ with description.
    assert!(sys_text.contains("✓ `soporte_lvl1`"));
    assert!(sys_text.contains("first-line"));
    // Inline system_prompt still present below peers.
    assert!(sys_text.contains("Be helpful."));
}

#[tokio::test]
async fn retain_matching_prunes_tools_from_llm_request() {
    // Registry-level allowlist (applied by `main.rs` after all tools
    // register) must actually reach the LLM — verify that pruned tools
    // don't appear in the outbound ChatRequest.tools field.
    let broker = AnyBroker::local();
    let _sub = broker.subscribe("plugin.outbound.telegram").await.unwrap();
    let ctx = make_context(broker);

    struct Noop;
    #[async_trait]
    impl ToolHandler for Noop {
        async fn call(&self, _: &AgentContext, _: Value) -> anyhow::Result<Value> {
            Ok(Value::Null)
        }
    }
    fn td(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: "stub".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    let tools = Arc::new(ToolRegistry::new());
    tools.register(td("memory_recall"), Noop);
    tools.register(td("ext_github_comment"), Noop);
    tools.register(td("ext_weather_forecast"), Noop);
    tools.register(td("delegate"), Noop);
    assert_eq!(tools.to_tool_defs().len(), 4);

    // Simulate the main.rs allowlist step — keep memory_* and delegate.
    let removed = tools.retain_matching(&["memory_*".into(), "delegate".into()]);
    assert_eq!(removed, 2, "github and weather should be pruned");
    assert_eq!(tools.to_tool_defs().len(), 2);

    let captured = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingLlm {
        captured: Arc::clone(&captured),
        reply: "ok".into(),
    };
    let behavior = LlmAgentBehavior::new(Arc::new(llm) as Arc<dyn LlmClient>, tools);

    let mut msg = InboundMessage::new(Uuid::new_v4(), "test-agent", "hi");
    msg.source_plugin = "telegram".into();
    msg.sender_id = Some("u1".into());
    behavior.on_message(&ctx, msg).await.unwrap();

    let req = captured.lock().unwrap().remove(0);
    let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        names.contains(&"memory_recall"),
        "allowlisted tool missing: {names:?}"
    );
    assert!(
        names.contains(&"delegate"),
        "allowlisted tool missing: {names:?}"
    );
    assert!(
        !names.contains(&"ext_github_comment"),
        "pruned tool leaked to LLM: {names:?}"
    );
    assert!(
        !names.contains(&"ext_weather_forecast"),
        "pruned tool leaked to LLM: {names:?}"
    );
}
