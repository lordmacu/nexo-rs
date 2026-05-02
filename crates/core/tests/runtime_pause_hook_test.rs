//! Phase 82.13.c — runtime intake pause hook integration tests.
//!
//! Verifies that the inbound dispatcher buffers messages onto the
//! per-scope pending queue when a scope is `PausedByOperator`, and
//! falls through to the legacy fire-turn path otherwise. Also
//! exercises the fail-open paths (broken store, unwired store) and
//! the firehose drop event when the cap evicts an oldest entry.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nexo_broker::AnyBroker;
use nexo_config::types::agents::{
    AgentConfig, AgentRuntimeConfig, HeartbeatConfig, InboundBinding, ModelConfig,
};
use nexo_core::agent::admin_rpc::domains::processing::ProcessingControlStore;
use nexo_core::agent::agent_events::AgentEventEmitter;
use nexo_core::agent::redaction::Redactor;
use nexo_core::agent::{
    Agent, AgentBehavior, AgentContext, AgentRuntime, InboundMessage,
};
use nexo_core::session::SessionManager;
use nexo_tool_meta::admin::agent_events::AgentEventKind;
use nexo_tool_meta::admin::processing::{
    PendingInbound, ProcessingControlState, ProcessingScope,
};
use serde_json::json;
use tokio::time::sleep;
use uuid::Uuid;

// ── shared mocks ─────────────────────────────────────────────────

#[derive(Debug, Default)]
struct MockProcessingStore {
    rows: Mutex<std::collections::HashMap<ProcessingScope, ProcessingControlState>>,
    pending: Mutex<
        std::collections::HashMap<
            ProcessingScope,
            std::collections::VecDeque<PendingInbound>,
        >,
    >,
    cap: usize,
    /// When `Some`, every `get` returns this Err — drives the
    /// fail-open test.
    fail_get: Option<String>,
}

impl MockProcessingStore {
    fn new(cap: usize) -> Self {
        Self {
            rows: Mutex::new(Default::default()),
            pending: Mutex::new(Default::default()),
            cap,
            fail_get: None,
        }
    }

    fn with_fail_get(mut self, msg: impl Into<String>) -> Self {
        self.fail_get = Some(msg.into());
        self
    }

    fn pause(&self, scope: ProcessingScope) {
        self.rows.lock().unwrap().insert(
            scope.clone(),
            ProcessingControlState::PausedByOperator {
                scope,
                paused_at_ms: 0,
                operator_token_hash: "h".into(),
                reason: None,
            },
        );
    }

    fn pending_depth(&self, scope: &ProcessingScope) -> usize {
        self.pending
            .lock()
            .unwrap()
            .get(scope)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    fn take_pending(&self, scope: &ProcessingScope) -> Vec<PendingInbound> {
        self.pending
            .lock()
            .unwrap()
            .remove(scope)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
    }
}

#[async_trait]
impl ProcessingControlStore for MockProcessingStore {
    async fn get(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<ProcessingControlState> {
        if let Some(msg) = &self.fail_get {
            return Err(anyhow::anyhow!(msg.clone()));
        }
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(scope)
            .cloned()
            .unwrap_or(ProcessingControlState::AgentActive))
    }

    async fn set(
        &self,
        scope: ProcessingScope,
        state: ProcessingControlState,
    ) -> anyhow::Result<bool> {
        self.rows.lock().unwrap().insert(scope, state);
        Ok(true)
    }

    async fn clear(&self, scope: &ProcessingScope) -> anyhow::Result<bool> {
        Ok(self.rows.lock().unwrap().remove(scope).is_some())
    }

    async fn push_pending(
        &self,
        scope: &ProcessingScope,
        inbound: PendingInbound,
    ) -> anyhow::Result<(usize, u32)> {
        let mut p = self.pending.lock().unwrap();
        let q = p
            .entry(scope.clone())
            .or_insert_with(std::collections::VecDeque::new);
        let mut dropped = 0u32;
        if self.cap == 0 {
            dropped = 1;
        } else {
            q.push_back(inbound);
            while q.len() > self.cap {
                if q.pop_front().is_some() {
                    dropped = dropped.saturating_add(1);
                }
            }
        }
        Ok((q.len(), dropped))
    }
}

#[derive(Debug, Default)]
struct CapturingEventEmitter {
    captured: Mutex<Vec<AgentEventKind>>,
}

#[async_trait]
impl AgentEventEmitter for CapturingEventEmitter {
    async fn emit(&self, event: AgentEventKind) {
        self.captured.lock().unwrap().push(event);
    }
}

struct CaptureBehavior {
    received: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl AgentBehavior for CaptureBehavior {
    async fn on_message(
        &self,
        _ctx: &AgentContext,
        msg: InboundMessage,
    ) -> anyhow::Result<()> {
        self.received.lock().unwrap().push(msg.text.clone());
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

fn make_config() -> AgentConfig {
    AgentConfig {
        id: "ana".to_string(),
        model: ModelConfig {
            provider: "minimax".to_string(),
            model: "m2.5".to_string(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig {
            enabled: false,
            interval: "5m".to_string(),
        },
        config: AgentRuntimeConfig {
            debounce_ms: 0,
            queue_cap: 32,
        },
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
        inbound_bindings: vec![InboundBinding {
            plugin: "test".into(),
            instance: None,
            ..Default::default()
        }],
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
    }
}

fn make_runtime(
    received: Arc<Mutex<Vec<String>>>,
    broker: AnyBroker,
    store: Option<Arc<dyn ProcessingControlStore>>,
    emitter: Option<Arc<dyn AgentEventEmitter>>,
    redactor: Option<Arc<Redactor>>,
) -> AgentRuntime {
    let agent = Arc::new(Agent::new(make_config(), CaptureBehavior { received }));
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(3600), 100));
    let mut runtime = AgentRuntime::new(agent, broker, sessions);
    if let Some(s) = store {
        runtime = runtime.with_processing_store(s);
    }
    if let Some(e) = emitter {
        runtime = runtime.with_event_emitter(e);
    }
    if let Some(r) = redactor {
        runtime = runtime.with_redactor(r);
    }
    runtime
}

async fn publish_text(
    broker: &AnyBroker,
    session_id: Uuid,
    text: &str,
    from: &str,
) {
    let mut event = nexo_broker::types::Event::new(
        "plugin.inbound.test",
        "test",
        json!({ "text": text, "from": from }),
    );
    event.session_id = Some(session_id);
    nexo_broker::BrokerHandle::publish(broker, "plugin.inbound.test", event)
        .await
        .unwrap();
}

fn convo() -> ProcessingScope {
    ProcessingScope::Conversation {
        agent_id: "ana".into(),
        channel: "test".into(),
        account_id: "default".into(),
        contact_id: "wa.55".into(),
        mcp_channel_source: None,
    }
}

// ── tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn inbound_during_pause_buffers_instead_of_firing() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(MockProcessingStore::new(50));
    store.pause(convo());

    let runtime = make_runtime(
        Arc::clone(&received),
        broker.clone(),
        Some(store.clone() as Arc<dyn ProcessingControlStore>),
        None,
        None,
    );
    runtime.start().await.unwrap();

    publish_text(&broker, Uuid::new_v4(), "hola durante pausa", "wa.55").await;
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    // Agent NOT invoked.
    assert!(
        received.lock().unwrap().is_empty(),
        "behavior received message during pause: {:?}",
        received.lock().unwrap()
    );
    // Buffer has 1 entry.
    assert_eq!(store.pending_depth(&convo()), 1);
    let pending = store.take_pending(&convo());
    assert_eq!(pending[0].body, "hola durante pausa");
    assert_eq!(pending[0].from_contact_id, "wa.55");
    assert_eq!(pending[0].source_plugin, "test");
}

#[tokio::test]
async fn inbound_active_scope_passes_through_to_agent() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    // Store wired but scope NEVER paused → AgentActive default.
    let store = Arc::new(MockProcessingStore::new(50));

    let runtime = make_runtime(
        Arc::clone(&received),
        broker.clone(),
        Some(store.clone() as Arc<dyn ProcessingControlStore>),
        None,
        None,
    );
    runtime.start().await.unwrap();

    publish_text(&broker, Uuid::new_v4(), "hola normal", "wa.55").await;
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    // Agent invoked normally.
    assert_eq!(
        received.lock().unwrap().as_slice(),
        &["hola normal".to_string()]
    );
    // Buffer untouched.
    assert_eq!(store.pending_depth(&convo()), 0);
}

#[tokio::test]
async fn inbound_when_store_unwired_fires_legacy_path() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));

    // No store wired at all — runtime.processing_store stays None.
    let runtime = make_runtime(Arc::clone(&received), broker.clone(), None, None, None);
    runtime.start().await.unwrap();

    publish_text(&broker, Uuid::new_v4(), "legacy", "wa.55").await;
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    assert_eq!(received.lock().unwrap().as_slice(), &["legacy".to_string()]);
}

#[tokio::test]
async fn inbound_when_store_get_fails_fails_open_to_agent() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(MockProcessingStore::new(50).with_fail_get("synthetic disk error"));

    let runtime = make_runtime(
        Arc::clone(&received),
        broker.clone(),
        Some(store.clone() as Arc<dyn ProcessingControlStore>),
        None,
        None,
    );
    runtime.start().await.unwrap();

    publish_text(&broker, Uuid::new_v4(), "fail-open", "wa.55").await;
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    // Fail-open: agent IS invoked even though store returned Err.
    assert_eq!(
        received.lock().unwrap().as_slice(),
        &["fail-open".to_string()]
    );
    // Buffer untouched (push never attempted on fail-open).
    assert_eq!(store.pending_depth(&convo()), 0);
}

#[tokio::test]
async fn body_is_redacted_before_push() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(MockProcessingStore::new(50));
    store.pause(convo());
    // Redactor masks anything matching the custom `phone` regex.
    let redaction_cfg = nexo_config::types::transcripts::RedactionConfig {
        enabled: true,
        use_builtins: false,
        extra_patterns: vec![nexo_config::types::transcripts::RedactionPattern {
            label: "phone".into(),
            regex: r"\d{10,}".into(),
        }],
    };
    let redactor = Arc::new(Redactor::from_config(&redaction_cfg).unwrap());

    let runtime = make_runtime(
        Arc::clone(&received),
        broker.clone(),
        Some(store.clone() as Arc<dyn ProcessingControlStore>),
        None,
        Some(redactor),
    );
    runtime.start().await.unwrap();

    publish_text(&broker, Uuid::new_v4(), "mi numero es 5555551234", "wa.55").await;
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    let pending = store.take_pending(&convo());
    assert_eq!(pending.len(), 1);
    assert!(
        !pending[0].body.contains("5555551234"),
        "raw PII leaked into pending queue: {}",
        pending[0].body
    );
    assert!(pending[0].body.contains("[REDACTED:phone]"));
}

#[tokio::test]
async fn cap_exceeded_emits_drop_event_on_firehose() {
    let broker = AnyBroker::local();
    let received = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(MockProcessingStore::new(2)); // tiny cap → easy overflow
    store.pause(convo());
    let emitter = Arc::new(CapturingEventEmitter::default());

    let runtime = make_runtime(
        Arc::clone(&received),
        broker.clone(),
        Some(store.clone() as Arc<dyn ProcessingControlStore>),
        Some(emitter.clone() as Arc<dyn AgentEventEmitter>),
        None,
    );
    runtime.start().await.unwrap();

    // 3 inbounds → cap=2 → 1 eviction → 1 firehose drop event.
    for i in 0..3 {
        publish_text(&broker, Uuid::new_v4(), &format!("msg {i}"), "wa.55").await;
        sleep(Duration::from_millis(20)).await;
    }
    sleep(Duration::from_millis(80)).await;
    runtime.stop().await;

    // Buffer at cap.
    assert_eq!(store.pending_depth(&convo()), 2);
    // Firehose received exactly 1 drop event.
    let captured = emitter.captured.lock().unwrap();
    let drops: Vec<_> = captured
        .iter()
        .filter(|e| matches!(e, AgentEventKind::PendingInboundsDropped { .. }))
        .collect();
    assert_eq!(drops.len(), 1, "expected exactly 1 drop event");
    if let AgentEventKind::PendingInboundsDropped {
        agent_id,
        scope,
        dropped,
        ..
    } = drops[0]
    {
        assert_eq!(agent_id, "ana");
        assert_eq!(*dropped, 1);
        assert_eq!(scope, &convo());
    } else {
        unreachable!();
    }
}
