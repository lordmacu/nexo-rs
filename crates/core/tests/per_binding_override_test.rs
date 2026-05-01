//! End-to-end tests for per-binding capability override.
//!
//! Verifies that the runtime intake resolves the matched binding's
//! effective policy and attaches it to the session `AgentContext` so
//! downstream consumers (tools, LLM turn, rate limiter, delegation)
//! see the right capability surface.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nexo_broker::{types::Event, AnyBroker, BrokerHandle};
use nexo_config::types::agents::{
    AgentConfig, AgentRuntimeConfig, HeartbeatConfig, InboundBinding, ModelConfig,
    OutboundAllowlistConfig, SenderRateLimitConfig, SenderRateLimitKeyword,
    SenderRateLimitOverride,
};
use nexo_core::agent::{
    Agent, AgentBehavior, AgentContext, AgentRuntime, EffectiveBindingPolicy, InboundMessage,
};
use nexo_core::session::SessionManager;
use serde_json::json;
use tokio::time::sleep;
use uuid::Uuid;

#[derive(Default)]
struct CapturedEffective {
    binding_index: Option<usize>,
    allowed_tools: Vec<String>,
    outbound_whatsapp: Vec<String>,
    outbound_telegram: Vec<i64>,
    skills: Vec<String>,
    model: String,
    system_prompt: String,
    sender_rate_limit_rps: Option<f64>,
    allowed_delegates: Vec<String>,
    text: String,
}

struct RecorderBehavior {
    captures: Arc<Mutex<Vec<CapturedEffective>>>,
}

#[async_trait]
impl AgentBehavior for RecorderBehavior {
    async fn on_message(&self, ctx: &AgentContext, msg: InboundMessage) -> anyhow::Result<()> {
        let eff = ctx.effective_policy();
        self.captures.lock().unwrap().push(CapturedEffective {
            binding_index: eff.binding_index,
            allowed_tools: eff.allowed_tools.clone(),
            outbound_whatsapp: eff.outbound_allowlist.whatsapp.clone(),
            outbound_telegram: eff.outbound_allowlist.telegram.clone(),
            skills: eff.skills.clone(),
            model: eff.model.model.clone(),
            system_prompt: eff.system_prompt.clone(),
            sender_rate_limit_rps: eff.sender_rate_limit.as_ref().map(|c| c.rps),
            allowed_delegates: eff.allowed_delegates.clone(),
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

fn two_binding_agent() -> AgentConfig {
    AgentConfig {
        id: "ana_two_binding".into(),
        model: ModelConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
        },
        plugins: vec!["whatsapp".into(), "telegram".into()],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig {
            debounce_ms: 0,
            queue_cap: 32,
        },
        system_prompt: "You are Ana.".into(),
        workspace: String::new(),
        skills: vec!["weather".into()],
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        allowed_tools: vec!["weather".into()],
        sender_rate_limit: Some(SenderRateLimitConfig { rps: 2.0, burst: 5 }),
        allowed_delegates: vec!["peer_a".into()],
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: OutboundAllowlistConfig {
            whatsapp: vec!["500".into()],
            telegram: vec![500],
        },
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
        inbound_bindings: vec![
            // Sales WhatsApp — narrow.
            InboundBinding {
                plugin: "whatsapp".into(),
                allowed_tools: Some(vec!["whatsapp_send_message".into()]),
                outbound_allowlist: Some(OutboundAllowlistConfig {
                    whatsapp: vec!["573115728852".into()],
                    telegram: Vec::new(),
                }),
                skills: Some(Vec::new()),
                sender_rate_limit: SenderRateLimitOverride::Config(SenderRateLimitConfig {
                    rps: 0.5,
                    burst: 3,
                }),
                system_prompt_extra: Some("WhatsApp sales channel.".into()),
                allowed_delegates: Some(Vec::new()),
                ..Default::default()
            },
            // Private Telegram — full power, same provider.
            InboundBinding {
                plugin: "telegram".into(),
                instance: Some("ana_tg".into()),
                allowed_tools: Some(vec!["*".into()]),
                outbound_allowlist: Some(OutboundAllowlistConfig {
                    whatsapp: Vec::new(),
                    telegram: vec![1_194_292_426],
                }),
                skills: Some(vec!["browser".into(), "github".into()]),
                model: Some(ModelConfig {
                    provider: "anthropic".into(),
                    model: "claude-sonnet-4-5".into(),
                }),
                sender_rate_limit: SenderRateLimitOverride::Keyword(
                    SenderRateLimitKeyword::Disable,
                ),
                system_prompt_extra: Some("Private Telegram.".into()),
                allowed_delegates: Some(vec!["*".into()]),
                language: None,
                link_understanding: serde_json::Value::Null,
                web_search: serde_json::Value::Null,
                pairing_policy: serde_json::Value::Null,
                dispatch_policy: Default::default(),
                plan_mode: Default::default(),
                role: None,
                proactive: None,
                remote_triggers: None,
                repl: None,
                auto_approve: None,
                allowed_channel_servers: Vec::new(),
                lsp: None,
                team: None,
                config_tool: None,
            },
        ],
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
    }
}

async fn publish_on(broker: &AnyBroker, topic: &str, session_id: Uuid, text: &str) {
    let mut ev = Event::new(topic, "ext", json!({ "text": text, "from": "user-1" }));
    ev.session_id = Some(session_id);
    broker.publish(topic, ev).await.unwrap();
}

async fn spawn_runtime(
    cfg: AgentConfig,
) -> (AgentRuntime, Arc<Mutex<Vec<CapturedEffective>>>, AnyBroker) {
    let broker = AnyBroker::local();
    let captures = Arc::new(Mutex::new(Vec::new()));
    let behavior = RecorderBehavior {
        captures: Arc::clone(&captures),
    };
    let agent = Arc::new(Agent::new(cfg, behavior));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let runtime = AgentRuntime::new(agent, broker.clone(), sessions);
    runtime.start().await.unwrap();
    (runtime, captures, broker)
}

#[tokio::test]
async fn whatsapp_binding_receives_narrow_policy_telegram_binding_receives_full_policy() {
    let (runtime, captures, broker) = spawn_runtime(two_binding_agent()).await;

    // Fire a WhatsApp message.
    publish_on(
        &broker,
        "plugin.inbound.whatsapp",
        Uuid::new_v4(),
        "hola WA",
    )
    .await;
    sleep(Duration::from_millis(50)).await;
    // Fire a Telegram message targeting the ana_tg bot.
    publish_on(
        &broker,
        "plugin.inbound.telegram.ana_tg",
        Uuid::new_v4(),
        "hola TG",
    )
    .await;
    sleep(Duration::from_millis(50)).await;

    runtime.stop().await;

    let snap = captures.lock().unwrap();
    assert_eq!(
        snap.len(),
        2,
        "both bindings should have delivered one event each"
    );

    let wa = snap
        .iter()
        .find(|c| c.text == "hola WA")
        .expect("WA capture");
    assert_eq!(wa.binding_index, Some(0), "WA binding is declared first");
    assert_eq!(wa.allowed_tools, vec!["whatsapp_send_message".to_string()]);
    assert_eq!(wa.outbound_whatsapp, vec!["573115728852".to_string()]);
    assert!(
        wa.outbound_telegram.is_empty(),
        "WA binding clears TG outbound"
    );
    assert!(wa.skills.is_empty(), "WA binding loads no skills");
    assert_eq!(wa.model, "claude-haiku-4-5", "WA uses agent-level model");
    assert!(
        wa.system_prompt.contains("You are Ana.")
            && wa.system_prompt.contains("# CHANNEL ADDENDUM")
            && wa.system_prompt.contains("WhatsApp sales channel."),
        "WA prompt composes base + addendum, got: {}",
        wa.system_prompt
    );
    assert_eq!(wa.sender_rate_limit_rps, Some(0.5), "WA uses binding RPS");
    assert!(wa.allowed_delegates.is_empty(), "WA bans delegation");

    let tg = snap
        .iter()
        .find(|c| c.text == "hola TG")
        .expect("TG capture");
    assert_eq!(tg.binding_index, Some(1), "TG binding is declared second");
    assert_eq!(tg.allowed_tools, vec!["*".to_string()]);
    assert_eq!(tg.outbound_telegram, vec![1_194_292_426]);
    assert!(tg.outbound_whatsapp.is_empty());
    assert_eq!(tg.skills, vec!["browser".to_string(), "github".to_string()]);
    assert_eq!(
        tg.model, "claude-sonnet-4-5",
        "TG swaps in a stronger model within the same provider"
    );
    assert!(tg.system_prompt.contains("Private Telegram."));
    assert_eq!(
        tg.sender_rate_limit_rps, None,
        "TG disables the agent-level rate limit explicitly"
    );
    assert_eq!(tg.allowed_delegates, vec!["*".to_string()]);
}

#[tokio::test]
async fn unmatched_binding_drops_event_without_spawning_session() {
    // Agent only binds WhatsApp; Telegram event should be filtered out.
    let mut cfg = two_binding_agent();
    cfg.inbound_bindings.truncate(1); // keep only the WA binding
    let (runtime, captures, broker) = spawn_runtime(cfg).await;

    publish_on(
        &broker,
        "plugin.inbound.telegram.ana_tg",
        Uuid::new_v4(),
        "stray TG",
    )
    .await;
    publish_on(
        &broker,
        "plugin.inbound.whatsapp",
        Uuid::new_v4(),
        "hola WA",
    )
    .await;
    sleep(Duration::from_millis(50)).await;

    runtime.stop().await;

    let snap = captures.lock().unwrap();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].text, "hola WA");
}

#[tokio::test]
async fn agent_without_bindings_drops_inbound_events() {
    // Strict binding rule: an agent with no `inbound_bindings`
    // declares no plugin subscription, so plugin events are dropped
    // outright (instead of falling into the previous "legacy
    // wildcard" bucket that swallowed every event). Heartbeat and
    // agent-to-agent paths still see the synthesised agent-level
    // policy because those topics don't go through the binding
    // filter.
    let cfg = AgentConfig {
        id: "legacy".into(),
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
        system_prompt: "Legacy Ana.".into(),
        workspace: String::new(),
        skills: vec!["weather".into()],
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        inbound_bindings: Vec::new(),
        allowed_tools: vec!["weather".into()],
        sender_rate_limit: None,
        allowed_delegates: vec!["peer_a".into()],
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: OutboundAllowlistConfig {
            whatsapp: vec!["573000000000".into()],
            telegram: Vec::new(),
        },
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
    };
    let (runtime, captures, broker) = spawn_runtime(cfg).await;

    publish_on(
        &broker,
        "plugin.inbound.whatsapp",
        Uuid::new_v4(),
        "legacy hi",
    )
    .await;
    sleep(Duration::from_millis(50)).await;
    runtime.stop().await;

    let snap = captures.lock().unwrap();
    assert!(
        snap.is_empty(),
        "agent with no bindings must drop plugin events (no legacy wildcard)"
    );
}

#[tokio::test]
async fn binding_rate_limit_is_enforced_per_binding() {
    // WA binding has rps=0.5 / burst=3; after 3 rapid sends the 4th
    // must be dropped. TG binding has rate-limit disabled, so traffic
    // on it is unaffected even while WA is throttled.
    let cfg = two_binding_agent();
    let (runtime, captures, broker) = spawn_runtime(cfg).await;

    for i in 0..6 {
        publish_on(
            &broker,
            "plugin.inbound.whatsapp",
            Uuid::new_v4(),
            &format!("wa-{i}"),
        )
        .await;
    }
    for i in 0..3 {
        publish_on(
            &broker,
            "plugin.inbound.telegram.ana_tg",
            Uuid::new_v4(),
            &format!("tg-{i}"),
        )
        .await;
    }
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    let snap = captures.lock().unwrap();
    let wa_count = snap.iter().filter(|c| c.text.starts_with("wa-")).count();
    let tg_count = snap.iter().filter(|c| c.text.starts_with("tg-")).count();
    assert_eq!(
        wa_count, 3,
        "burst=3 on WA binding caps it at three accepted events"
    );
    assert_eq!(tg_count, 3, "TG binding has no limit and accepts all three");
}

#[tokio::test]
async fn effective_policy_defense_in_depth_blocks_hallucinated_tools() {
    // Unit-level guard (no broker): the execution guard inside
    // llm_behavior consults effective.tool_allowed. Exercise the
    // helper directly to make sure a tool name outside the allowlist
    // is rejected even when the base registry contains it.
    let mut agent = two_binding_agent();
    // Resolve the WA binding (narrow).
    let wa = EffectiveBindingPolicy::resolve(&agent, 0);
    assert!(wa.tool_allowed("whatsapp_send_message"));
    assert!(!wa.tool_allowed("browser_open"));
    assert!(!wa.tool_allowed("memory_write"));

    let tg = EffectiveBindingPolicy::resolve(&agent, 1);
    assert!(tg.tool_allowed("anything_goes"));
    assert!(tg.tool_allowed("memory_write"));

    // Sanity: if we wipe the binding overrides the effective policy
    // falls back to the agent-level allowed_tools (['weather'] in the
    // fixture) — so the runtime guard would reject 'browser_open'
    // even for a binding that inherited.
    agent.inbound_bindings[0].allowed_tools = None;
    let inherited = EffectiveBindingPolicy::resolve(&agent, 0);
    assert_eq!(inherited.allowed_tools, vec!["weather".to_string()]);
    assert!(!inherited.tool_allowed("browser_open"));
    assert!(inherited.tool_allowed("weather"));
}

#[tokio::test]
async fn worker_role_uses_curated_default_tools_in_runtime() {
    let mut cfg = two_binding_agent();
    cfg.allowed_tools = vec!["*".into()];
    cfg.inbound_bindings = vec![InboundBinding {
        plugin: "telegram".into(),
        instance: Some("worker_bot".into()),
        role: Some("worker".into()),
        allowed_tools: None,
        ..Default::default()
    }];

    let (runtime, captures, broker) = spawn_runtime(cfg).await;
    publish_on(
        &broker,
        "plugin.inbound.telegram.worker_bot",
        Uuid::new_v4(),
        "worker ping",
    )
    .await;
    sleep(Duration::from_millis(60)).await;
    runtime.stop().await;

    let snap = captures.lock().unwrap();
    let worker = snap
        .iter()
        .find(|c| c.text == "worker ping")
        .expect("worker capture");
    assert_eq!(
        worker.allowed_tools,
        vec![
            "bash".to_string(),
            "file_read".to_string(),
            "file_edit".to_string(),
            "agent_turns_tail".to_string()
        ]
    );
}
