//! Phase 79.7 runtime — `LlmCronDispatcher`.
//!
//! Real LLM-call cron dispatcher: builds a `ChatRequest` from the
//! entry's prompt, calls `LlmClient::chat`, and logs the response
//! body. When `channel` + `recipient` are present, it can also
//! publish the response through the broker using the same outbound
//! event shape as user-facing channel tools.
//!
//! Reference (PRIMARY):
//!   * Phase 20 `agent_turn` poller (`crates/poller/src/builtins/agent_turn.rs:157-260`)
//!     — same pattern: build messages, call `chat`, take response.
//!   * `claude-code-leak/src/utils/cronTasks.ts` — sequential
//!     fire loop the leak uses.
//!
//! Design choices:
//!
//! - The dispatcher supports two modes:
//!   1) fixed client (`new`) for tests/legacy wiring
//!   2) registry-routed (`from_registry`) where each cron entry can
//!      pin `(model_provider, model_name)` and the dispatcher lazily
//!      builds + caches clients per pair.
//! - Optional `system_prompt` prepended to every fire — operators
//!   can pin behaviour (e.g. "Reply terse. One bullet per
//!   finding.").
//! - The dispatcher returns `Ok(())` on success regardless of
//!   what the LLM said. Empty responses still count as fired —
//!   the runner advances state either way (per
//!   [`crate::cron_runner::CronRunner`] semantics).

use crate::cron_runner::CronDispatcher;
use crate::cron_schedule::CronEntry;
use async_trait::async_trait;
use dashmap::DashMap;
use nexo_broker::{BrokerHandle, Event};
use nexo_config::types::agents::ModelConfig;
use nexo_config::types::llm::LlmConfig;
use nexo_llm::{
    ChatMessage, ChatRequest, ChatRole, LlmClient, LlmRegistry, ResponseContent, ToolCall, ToolDef,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Publishes the LLM response to a user-facing channel. The cron
/// dispatcher delegates to this trait so production can route through
/// the NATS broker while tests use an in-memory capture.
#[async_trait]
pub trait ChannelPublisher: Send + Sync {
    /// `channel_hint` is the `<plugin>:<instance>` string from
    /// [`CronEntry::channel`]. `recipient` is the JID/chat-id/email
    /// from [`CronEntry::recipient`]. `body` is the raw LLM text.
    async fn publish(&self, channel_hint: &str, recipient: &str, body: &str) -> anyhow::Result<()>;
}

/// Optional tool-call executor for cron LLM responses. When configured,
/// the dispatcher can advertise a filtered tool catalog and execute
/// `ResponseContent::ToolCalls` in-process.
#[async_trait]
pub trait CronToolExecutor: Send + Sync {
    /// Tool catalog visible for this cron entry.
    fn list_tools(&self, entry: &CronEntry) -> Vec<ToolDef>;
    /// Execute one tool call emitted by the model.
    async fn call_tool(
        &self,
        entry: &CronEntry,
        tool_name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value>;
}

/// Split a `<plugin>:<instance>` hint into its two parts. Refuses
/// any string that does not contain exactly one `:` separator with
/// non-empty halves — keeps malformed config from publishing to a
/// surprising topic like `plugin.outbound.whatsapp.` (trailing dot).
pub fn parse_channel_hint(hint: &str) -> Option<(String, String)> {
    let mut parts = hint.splitn(2, ':');
    let plugin = parts.next()?.trim();
    let instance = parts.next()?.trim();
    if plugin.is_empty() || instance.is_empty() || instance.contains(':') {
        return None;
    }
    Some((plugin.to_string(), instance.to_string()))
}

/// Production publisher that emits an outbound event on
/// `plugin.outbound.<plugin>.<instance>` carrying
/// `{"kind": "text", "to": <recipient>, "text": <body>}`. The shape
/// matches what the WhatsApp / Telegram / Email plugins already
/// consume from their own outbound tools (see
/// `crates/plugins/whatsapp/src/tool.rs:209` and
/// `crates/plugins/telegram/src/tool.rs:157`).
pub struct BrokerChannelPublisher<B: BrokerHandle + ?Sized> {
    broker: Arc<B>,
}

impl<B: BrokerHandle + ?Sized> BrokerChannelPublisher<B> {
    pub fn new(broker: Arc<B>) -> Self {
        Self { broker }
    }
}

#[async_trait]
impl<B: BrokerHandle + ?Sized + Send + Sync + 'static> ChannelPublisher
    for BrokerChannelPublisher<B>
{
    async fn publish(&self, channel_hint: &str, recipient: &str, body: &str) -> anyhow::Result<()> {
        let (plugin, instance) = parse_channel_hint(channel_hint).ok_or_else(|| {
            anyhow::anyhow!("cron channel hint `{channel_hint}` is not `<plugin>:<instance>`")
        })?;
        let topic = format!("plugin.outbound.{plugin}.{instance}");
        let payload = serde_json::json!({
            "kind": "text",
            "to": recipient,
            "text": body,
        });
        let event = Event::new(topic.clone(), "cron-dispatcher", payload);
        self.broker
            .publish(&topic, event)
            .await
            .map_err(|e| anyhow::anyhow!("broker publish failed on `{topic}`: {e}"))?;
        Ok(())
    }
}

pub struct LlmCronDispatcher {
    client_mode: ClientMode,
    system_prompt: Option<String>,
    max_tokens: Option<u32>,
    publisher: Option<Arc<dyn ChannelPublisher>>,
    tool_executor: Option<Arc<dyn CronToolExecutor>>,
    max_tool_iterations: usize,
}

enum ClientMode {
    Fixed(Arc<dyn LlmClient>),
    Routed(Box<RoutedClientResolver>),
}

struct RoutedClientResolver {
    registry: Arc<LlmRegistry>,
    llm_cfg: LlmConfig,
    legacy_binding_models: HashMap<String, ModelConfig>,
    fallback_model: Option<ModelConfig>,
    cache: DashMap<String, Arc<dyn LlmClient>>,
}

impl RoutedClientResolver {
    fn model_for_entry(&self, entry: &CronEntry) -> anyhow::Result<ModelConfig> {
        let entry_provider = entry
            .model_provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let entry_model = entry
            .model_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match (entry_provider, entry_model) {
            (Some(provider), Some(model)) => Ok(ModelConfig {
                provider: provider.to_string(),
                model: model.to_string(),
            }),
            (Some(_), None) | (None, Some(_)) => anyhow::bail!(
                "cron entry has partial model override; both model_provider and model_name must be set"
            ),
            (None, None) => {
                if let Some(m) = self.legacy_binding_models.get(&entry.binding_id) {
                    return Ok(m.clone());
                }
                self.fallback_model.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "cron entry has no model override and no dispatcher fallback model"
                    )
                })
            }
        }
    }

    fn client_for_entry(
        &self,
        entry: &CronEntry,
    ) -> anyhow::Result<(Arc<dyn LlmClient>, ModelConfig)> {
        let model = self.model_for_entry(entry)?;
        let key = format!("{}\u{1f}{}", model.provider, model.model);
        if let Some(hit) = self.cache.get(&key) {
            return Ok((Arc::clone(hit.value()), model));
        }
        let built = self.registry.build(&self.llm_cfg, &model).map_err(|e| {
            anyhow::anyhow!(
                "build client {}:{} failed: {e}",
                model.provider,
                model.model
            )
        })?;
        let slot = self.cache.entry(key).or_insert_with(|| Arc::clone(&built));
        Ok((Arc::clone(slot.value()), model))
    }
}

impl LlmCronDispatcher {
    pub fn new(client: Arc<dyn LlmClient>) -> Self {
        Self {
            client_mode: ClientMode::Fixed(client),
            system_prompt: None,
            max_tokens: None,
            publisher: None,
            tool_executor: None,
            max_tool_iterations: 6,
        }
    }

    /// Model-routing mode: each cron entry can pin
    /// `model_provider/model_name`; the dispatcher lazily builds + caches
    /// clients per `(provider, model)` pair.
    ///
    /// Legacy rows without model metadata first try
    /// `legacy_binding_models[entry.binding_id]` and then
    /// `fallback_model` when provided.
    pub fn from_registry(
        registry: Arc<LlmRegistry>,
        llm_cfg: LlmConfig,
        legacy_binding_models: HashMap<String, ModelConfig>,
        fallback_model: Option<ModelConfig>,
    ) -> Self {
        Self {
            client_mode: ClientMode::Routed(Box::new(RoutedClientResolver {
                registry,
                llm_cfg,
                legacy_binding_models,
                fallback_model,
                cache: DashMap::new(),
            })),
            system_prompt: None,
            max_tokens: None,
            publisher: None,
            tool_executor: None,
            max_tool_iterations: 6,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        let s = prompt.into();
        if !s.trim().is_empty() {
            self.system_prompt = Some(s);
        }
        self
    }

    pub fn with_max_tokens(mut self, mt: u32) -> Self {
        self.max_tokens = Some(mt);
        self
    }

    /// Opt-in: when set, fired entries with both a `channel` hint and
    /// a `recipient` will have their LLM response forwarded to the
    /// channel via this publisher. When absent, the dispatcher only
    /// logs (the historical behaviour).
    pub fn with_publisher(mut self, publisher: Arc<dyn ChannelPublisher>) -> Self {
        self.publisher = Some(publisher);
        self
    }

    /// Enable cron tool-call execution with a bounded iteration loop.
    pub fn with_tool_executor(
        mut self,
        executor: Arc<dyn CronToolExecutor>,
        max_tool_iterations: usize,
    ) -> Self {
        self.tool_executor = Some(executor);
        self.max_tool_iterations = max_tool_iterations.max(1);
        self
    }

    fn select_client_for_entry(
        &self,
        entry: &CronEntry,
    ) -> anyhow::Result<(Arc<dyn LlmClient>, String, String)> {
        match &self.client_mode {
            ClientMode::Fixed(client) => Ok((
                Arc::clone(client),
                client.model_id().to_string(),
                client.provider().to_string(),
            )),
            ClientMode::Routed(router) => {
                let (client, model) = router.client_for_entry(entry)?;
                Ok((client, model.model, model.provider))
            }
        }
    }
}

#[async_trait]
impl CronDispatcher for LlmCronDispatcher {
    async fn fire(&self, entry: &CronEntry) -> anyhow::Result<()> {
        let (client, request_model, request_provider) = self
            .select_client_for_entry(entry)
            .map_err(|e| anyhow::anyhow!("cron entry `{}` LLM resolution failed: {e}", entry.id))?;
        let mut messages: Vec<ChatMessage> = Vec::with_capacity(2);
        if let Some(sp) = self.system_prompt.as_deref() {
            messages.push(ChatMessage {
                role: ChatRole::System,
                content: sp.to_string(),
                attachments: Vec::new(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
            });
        }
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: entry.prompt.clone(),
            attachments: Vec::new(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        });

        let tools = self
            .tool_executor
            .as_ref()
            .map(|e| e.list_tools(entry))
            .unwrap_or_default();
        let mut body = String::new();
        let mut tool_call_total: usize = 0;
        let mut tool_exec_total: usize = 0;
        for iteration in 0..self.max_tool_iterations {
            let mut req = ChatRequest::new(&request_model, messages.clone());
            if let Some(mt) = self.max_tokens {
                req.max_tokens = mt;
            }
            req.tools = tools.clone();

            let response = client.chat(req).await.map_err(|e| {
                anyhow::anyhow!("LLM chat failed for cron entry `{}`: {e}", entry.id)
            })?;
            match response.content {
                ResponseContent::Text(s) => {
                    body = s;
                    break;
                }
                ResponseContent::ToolCalls(calls) => {
                    tool_call_total = tool_call_total.saturating_add(calls.len());
                    if calls.is_empty() {
                        body.clear();
                        break;
                    }
                    // No executor or no exposed tools for this binding:
                    // preserve historical behavior (surface names as text).
                    if self.tool_executor.is_none() || tools.is_empty() {
                        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
                        body = format!("(tool_calls: {})", names.join(", "));
                        break;
                    }

                    if iteration + 1 >= self.max_tool_iterations {
                        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
                        body = format!(
                            "(tool_calls: {}; max tool iterations reached)",
                            names.join(", ")
                        );
                        break;
                    }

                    messages.push(ChatMessage::assistant_tool_calls(
                        calls.clone(),
                        String::new(),
                    ));
                    for call in calls {
                        let tool_result = self
                            .execute_tool_call(entry, &call)
                            .await
                            .unwrap_or_else(|e| {
                                serde_json::json!({
                                    "ok": false,
                                    "error": format!("{e}"),
                                })
                                .to_string()
                            });
                        messages.push(ChatMessage::tool_result(&call.id, &call.name, tool_result));
                        tool_exec_total = tool_exec_total.saturating_add(1);
                    }
                }
            }
        }
        if body.is_empty() && tool_call_total > 0 {
            body = format!(
                "(tool_calls processed: requested={tool_call_total}, executed={tool_exec_total})"
            );
        }

        let text_chars = body.chars().count();
        let truncated_preview: String = body.chars().take(200).collect();

        tracing::info!(
            id = %entry.id,
            binding_id = %entry.binding_id,
            cron = %entry.cron_expr,
            recurring = entry.recurring,
            channel = ?entry.channel,
            recipient = ?entry.recipient,
            model_provider = %request_provider,
            model = %request_model,
            tool_calls = tool_call_total,
            tool_executed = tool_exec_total,
            response_chars = text_chars,
            preview = %truncated_preview,
            "[cron] llm response"
        );

        // Outbound publish: only when the operator wired a publisher
        // AND the entry carries both a channel hint and a recipient.
        // Tool-call responses are still published — operators get a
        // visible "(tool_calls: foo, bar)" line so the cron is not
        // silently swallowing the model's intent.
        if let Some(publisher) = self.publisher.as_ref() {
            match (entry.channel.as_deref(), entry.recipient.as_deref()) {
                (Some(ch), Some(to)) if !ch.is_empty() && !to.is_empty() => {
                    if let Err(e) = publisher.publish(ch, to, &body).await {
                        tracing::warn!(
                            id = %entry.id,
                            channel = %ch,
                            error = %e,
                            "[cron] outbound publish failed"
                        );
                    }
                }
                _ => {
                    tracing::debug!(
                        id = %entry.id,
                        "[cron] no channel+recipient — response logged only"
                    );
                }
            }
        }

        Ok(())
    }
}

impl LlmCronDispatcher {
    async fn execute_tool_call(
        &self,
        entry: &CronEntry,
        call: &ToolCall,
    ) -> anyhow::Result<String> {
        let Some(exec) = self.tool_executor.as_ref() else {
            anyhow::bail!("tool executor not configured");
        };
        let value = exec
            .call_tool(entry, &call.name, call.arguments.clone())
            .await?;
        Ok(stringify_tool_result(&value))
    }
}

fn stringify_tool_result(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_config::types::llm::{LlmProviderConfig, RateLimitConfig, RetryConfig};
    use nexo_llm::LlmProviderFactory;
    use nexo_llm::{ChatResponse, FinishReason, ResponseContent, TokenUsage};
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct MockLlmClient {
        captured: Mutex<Vec<ChatRequest>>,
        force_error: Mutex<Option<String>>,
        canned_response: String,
    }

    impl MockLlmClient {
        fn new(canned: impl Into<String>) -> Arc<Self> {
            Arc::new(Self {
                captured: Mutex::new(Vec::new()),
                force_error: Mutex::new(None),
                canned_response: canned.into(),
            })
        }
        fn force_err(&self, msg: &str) {
            *self.force_error.lock().unwrap() = Some(msg.to_string());
        }
        fn captured_requests(&self) -> Vec<ChatRequest> {
            self.captured.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmClient for MockLlmClient {
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.captured.lock().unwrap().push(req.clone());
            if let Some(msg) = self.force_error.lock().unwrap().clone() {
                anyhow::bail!(msg);
            }
            Ok(ChatResponse {
                content: ResponseContent::Text(self.canned_response.clone()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }
        fn model_id(&self) -> &str {
            "mock-model"
        }
        fn provider(&self) -> &str {
            "mock"
        }
    }

    fn entry(prompt: &str, recurring: bool) -> CronEntry {
        CronEntry {
            id: uuid::Uuid::new_v4().to_string(),
            binding_id: "whatsapp:default".into(),
            cron_expr: "*/5 * * * *".into(),
            prompt: prompt.into(),
            channel: None,
            model_provider: None,
            model_name: None,
            recurring,
            created_at: 0,
            next_fire_at: 0,
            last_fired_at: None,
            failure_count: 0,
            paused: false,
            recipient: None,
        }
    }

    #[tokio::test]
    async fn fires_chat_with_user_prompt() {
        let mock = MockLlmClient::new("ok");
        let dispatcher = LlmCronDispatcher::new(mock.clone());
        dispatcher
            .fire(&entry("ping the build queue", true))
            .await
            .unwrap();
        let reqs = mock.captured_requests();
        assert_eq!(reqs.len(), 1);
        let user_msg = reqs[0]
            .messages
            .iter()
            .find(|m| matches!(m.role, ChatRole::User))
            .unwrap();
        assert_eq!(user_msg.content, "ping the build queue");
        // No system prompt by default.
        assert!(reqs[0]
            .messages
            .iter()
            .all(|m| !matches!(m.role, ChatRole::System)));
    }

    #[tokio::test]
    async fn system_prompt_prepended_when_set() {
        let mock = MockLlmClient::new("ok");
        let dispatcher = LlmCronDispatcher::new(mock.clone()).with_system_prompt("Be terse.");
        dispatcher.fire(&entry("status?", true)).await.unwrap();
        let reqs = mock.captured_requests();
        let sys = reqs[0]
            .messages
            .iter()
            .find(|m| matches!(m.role, ChatRole::System))
            .unwrap();
        assert_eq!(sys.content, "Be terse.");
        // System message ordered before user.
        let roles: Vec<_> = reqs[0]
            .messages
            .iter()
            .map(|m| matches!(m.role, ChatRole::System))
            .collect();
        assert_eq!(roles, vec![true, false]);
    }

    #[tokio::test]
    async fn empty_system_prompt_is_ignored() {
        let mock = MockLlmClient::new("ok");
        let dispatcher = LlmCronDispatcher::new(mock.clone()).with_system_prompt("   ");
        dispatcher.fire(&entry("x", true)).await.unwrap();
        let reqs = mock.captured_requests();
        assert!(reqs[0]
            .messages
            .iter()
            .all(|m| !matches!(m.role, ChatRole::System)));
    }

    #[tokio::test]
    async fn max_tokens_propagates_to_request() {
        let mock = MockLlmClient::new("ok");
        let dispatcher = LlmCronDispatcher::new(mock.clone()).with_max_tokens(512);
        dispatcher.fire(&entry("x", true)).await.unwrap();
        let reqs = mock.captured_requests();
        assert_eq!(reqs[0].max_tokens, 512);
    }

    #[tokio::test]
    async fn llm_failure_propagates_as_error() {
        let mock = MockLlmClient::new("ok");
        mock.force_err("upstream-down");
        let dispatcher = LlmCronDispatcher::new(mock.clone());
        let err = dispatcher
            .fire(&entry("x", true))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("upstream-down"), "got: {err}");
        assert!(err.contains("LLM chat failed"), "got: {err}");
    }

    #[tokio::test]
    async fn empty_response_still_counts_as_fired() {
        let mock = MockLlmClient::new("");
        let dispatcher = LlmCronDispatcher::new(mock.clone());
        let res = dispatcher.fire(&entry("hello", true)).await;
        assert!(res.is_ok());
    }

    struct MockPublisher {
        captured: Mutex<Vec<(String, String, String)>>,
        force_error: Mutex<Option<String>>,
    }

    impl MockPublisher {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                captured: Mutex::new(Vec::new()),
                force_error: Mutex::new(None),
            })
        }
        fn force_err(&self, msg: &str) {
            *self.force_error.lock().unwrap() = Some(msg.to_string());
        }
        fn published(&self) -> Vec<(String, String, String)> {
            self.captured.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ChannelPublisher for MockPublisher {
        async fn publish(&self, ch: &str, to: &str, body: &str) -> anyhow::Result<()> {
            if let Some(msg) = self.force_error.lock().unwrap().clone() {
                anyhow::bail!(msg);
            }
            self.captured
                .lock()
                .unwrap()
                .push((ch.to_string(), to.to_string(), body.to_string()));
            Ok(())
        }
    }

    struct MockToolExecutor {
        calls: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl MockToolExecutor {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
            })
        }
        fn calls(&self) -> Vec<(String, serde_json::Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl CronToolExecutor for MockToolExecutor {
        fn list_tools(&self, _entry: &CronEntry) -> Vec<ToolDef> {
            vec![ToolDef {
                name: "echo_tool".to_string(),
                description: "echo".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer" }
                    }
                }),
            }]
        }

        async fn call_tool(
            &self,
            _entry: &CronEntry,
            tool_name: &str,
            args: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            self.calls
                .lock()
                .unwrap()
                .push((tool_name.to_string(), args.clone()));
            Ok(serde_json::json!({ "ok": true, "echo": args }))
        }
    }

    struct SequencedMockLlmClient {
        captured: Mutex<Vec<ChatRequest>>,
        responses: Mutex<Vec<ResponseContent>>,
    }

    impl SequencedMockLlmClient {
        fn new(responses: Vec<ResponseContent>) -> Arc<Self> {
            Arc::new(Self {
                captured: Mutex::new(Vec::new()),
                responses: Mutex::new(responses),
            })
        }
        fn captured_requests(&self) -> Vec<ChatRequest> {
            self.captured.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmClient for SequencedMockLlmClient {
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.captured.lock().unwrap().push(req.clone());
            let content = self.responses.lock().unwrap().remove(0);
            Ok(ChatResponse {
                content,
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        fn provider(&self) -> &str {
            "mock"
        }
    }

    fn entry_with_route(prompt: &str, channel: &str, recipient: &str) -> CronEntry {
        let mut e = entry(prompt, true);
        e.channel = Some(channel.to_string());
        e.recipient = Some(recipient.to_string());
        e
    }

    #[tokio::test]
    async fn publisher_invoked_when_channel_and_recipient_set() {
        let mock = MockLlmClient::new("hello back");
        let pub_ = MockPublisher::new();
        let dispatcher = LlmCronDispatcher::new(mock.clone()).with_publisher(pub_.clone());
        dispatcher
            .fire(&entry_with_route(
                "ping",
                "whatsapp:primary",
                "5511999@s.whatsapp.net",
            ))
            .await
            .unwrap();
        let pubs = pub_.published();
        assert_eq!(pubs.len(), 1);
        assert_eq!(pubs[0].0, "whatsapp:primary");
        assert_eq!(pubs[0].1, "5511999@s.whatsapp.net");
        assert_eq!(pubs[0].2, "hello back");
    }

    #[tokio::test]
    async fn publisher_skipped_when_channel_missing() {
        let mock = MockLlmClient::new("ok");
        let pub_ = MockPublisher::new();
        let dispatcher = LlmCronDispatcher::new(mock.clone()).with_publisher(pub_.clone());
        // entry() leaves channel/recipient = None
        dispatcher.fire(&entry("x", true)).await.unwrap();
        assert!(pub_.published().is_empty());
    }

    #[tokio::test]
    async fn publisher_skipped_when_recipient_missing() {
        let mock = MockLlmClient::new("ok");
        let pub_ = MockPublisher::new();
        let dispatcher = LlmCronDispatcher::new(mock.clone()).with_publisher(pub_.clone());
        let mut e = entry("x", true);
        e.channel = Some("whatsapp:primary".into());
        // recipient stays None
        dispatcher.fire(&e).await.unwrap();
        assert!(pub_.published().is_empty());
    }

    #[tokio::test]
    async fn publisher_failure_does_not_fail_fire() {
        let mock = MockLlmClient::new("ok");
        let pub_ = MockPublisher::new();
        pub_.force_err("upstream-down");
        let dispatcher = LlmCronDispatcher::new(mock.clone()).with_publisher(pub_.clone());
        // fire() still returns Ok — cron state advances even if the
        // user-facing publish failed (the runner already counts the
        // entry as fired).
        let res = dispatcher
            .fire(&entry_with_route("x", "whatsapp:primary", "abc"))
            .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn dispatcher_without_publisher_still_logs() {
        let mock = MockLlmClient::new("ok");
        let dispatcher = LlmCronDispatcher::new(mock.clone());
        // No publisher set; should still succeed and not panic.
        let res = dispatcher
            .fire(&entry_with_route("x", "whatsapp:primary", "abc"))
            .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn tool_calls_execute_when_executor_enabled() {
        let llm = SequencedMockLlmClient::new(vec![
            ResponseContent::ToolCalls(vec![ToolCall {
                id: "call-1".to_string(),
                name: "echo_tool".to_string(),
                arguments: serde_json::json!({"x": 7}),
            }]),
            ResponseContent::Text("final answer".to_string()),
        ]);
        let exec = MockToolExecutor::new();
        let dispatcher = LlmCronDispatcher::new(llm.clone()).with_tool_executor(exec.clone(), 4);
        dispatcher.fire(&entry("x", true)).await.unwrap();

        let calls = exec.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "echo_tool");
        assert_eq!(calls[0].1, serde_json::json!({"x": 7}));

        let reqs = llm.captured_requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].tools.len(), 1);
        // Second request includes tool result message fed back into model.
        assert!(reqs[1]
            .messages
            .iter()
            .any(|m| matches!(m.role, ChatRole::Tool)));
    }

    #[tokio::test]
    async fn tool_calls_without_executor_fall_back_to_text_publish() {
        let llm = SequencedMockLlmClient::new(vec![ResponseContent::ToolCalls(vec![ToolCall {
            id: "call-1".to_string(),
            name: "echo_tool".to_string(),
            arguments: serde_json::json!({"x": 7}),
        }])]);
        let pub_ = MockPublisher::new();
        let dispatcher = LlmCronDispatcher::new(llm).with_publisher(pub_.clone());
        dispatcher
            .fire(&entry_with_route(
                "x",
                "whatsapp:primary",
                "5511999@s.whatsapp.net",
            ))
            .await
            .unwrap();
        let pubs = pub_.published();
        assert_eq!(pubs.len(), 1);
        assert_eq!(pubs[0].2, "(tool_calls: echo_tool)");
    }

    #[tokio::test]
    async fn tool_calls_respect_max_iterations_cap() {
        let llm = SequencedMockLlmClient::new(vec![ResponseContent::ToolCalls(vec![ToolCall {
            id: "call-1".to_string(),
            name: "echo_tool".to_string(),
            arguments: serde_json::json!({"x": 7}),
        }])]);
        let exec = MockToolExecutor::new();
        let pub_ = MockPublisher::new();
        let dispatcher = LlmCronDispatcher::new(llm)
            .with_tool_executor(exec, 1)
            .with_publisher(pub_.clone());
        dispatcher
            .fire(&entry_with_route("x", "whatsapp:primary", "abc"))
            .await
            .unwrap();
        let pubs = pub_.published();
        assert_eq!(pubs.len(), 1);
        assert!(pubs[0].2.contains("max tool iterations reached"));
    }

    #[test]
    fn parse_channel_hint_accepts_well_formed() {
        let (p, i) = parse_channel_hint("whatsapp:primary").unwrap();
        assert_eq!(p, "whatsapp");
        assert_eq!(i, "primary");
    }

    #[test]
    fn parse_channel_hint_trims_surrounding_whitespace() {
        let (p, i) = parse_channel_hint(" whatsapp : primary ").unwrap();
        assert_eq!(p, "whatsapp");
        assert_eq!(i, "primary");
    }

    #[test]
    fn parse_channel_hint_rejects_missing_separator() {
        assert!(parse_channel_hint("whatsapp").is_none());
    }

    #[test]
    fn parse_channel_hint_rejects_empty_halves() {
        assert!(parse_channel_hint(":primary").is_none());
        assert!(parse_channel_hint("whatsapp:").is_none());
        assert!(parse_channel_hint(":").is_none());
    }

    #[test]
    fn parse_channel_hint_rejects_extra_separator() {
        assert!(parse_channel_hint("whatsapp:primary:extra").is_none());
    }

    #[tokio::test]
    async fn model_id_taken_from_client() {
        let mock = MockLlmClient::new("ok");
        let dispatcher = LlmCronDispatcher::new(mock.clone());
        dispatcher.fire(&entry("x", true)).await.unwrap();
        let reqs = mock.captured_requests();
        assert_eq!(reqs[0].model, "mock-model");
    }

    #[derive(Clone)]
    struct RoutedMockFactory {
        builds: Arc<Mutex<Vec<String>>>,
        seen_request_models: Arc<Mutex<Vec<String>>>,
    }

    struct RoutedMockClient {
        model: String,
        seen_request_models: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl LlmClient for RoutedMockClient {
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.seen_request_models
                .lock()
                .unwrap()
                .push(req.model.clone());
            Ok(ChatResponse {
                content: ResponseContent::Text("ok".into()),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                cache_usage: None,
            })
        }

        fn model_id(&self) -> &str {
            &self.model
        }

        fn provider(&self) -> &str {
            "stub"
        }
    }

    impl LlmProviderFactory for RoutedMockFactory {
        fn name(&self) -> &str {
            "stub"
        }

        fn build(
            &self,
            _provider_cfg: &LlmProviderConfig,
            model: &str,
            _retry: RetryConfig,
        ) -> anyhow::Result<Arc<dyn LlmClient>> {
            self.builds.lock().unwrap().push(model.to_string());
            Ok(Arc::new(RoutedMockClient {
                model: model.to_string(),
                seen_request_models: Arc::clone(&self.seen_request_models),
            }))
        }
    }

    fn routed_llm_cfg() -> LlmConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "stub".to_string(),
            LlmProviderConfig {
                api_key: "k".into(),
                base_url: "http://example.invalid".into(),
                group_id: None,
                rate_limit: RateLimitConfig {
                    requests_per_second: 1.0,
                    quota_alert_threshold: Some(100),
                },
                auth: None,
                api_flavor: None,
                embedding_model: None,
                safety_settings: None,
            },
        );
        LlmConfig {
            providers,
            retry: RetryConfig {
                max_attempts: 1,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
                backoff_multiplier: 1.0,
            },
            context_optimization: Default::default(),
        }
    }

    #[tokio::test]
    async fn routed_dispatcher_uses_entry_model_and_caches_client() {
        let builds = Arc::new(Mutex::new(Vec::new()));
        let seen_request_models = Arc::new(Mutex::new(Vec::new()));
        let mut registry = LlmRegistry::new();
        registry
            .register(Box::new(RoutedMockFactory {
                builds: Arc::clone(&builds),
                seen_request_models: Arc::clone(&seen_request_models),
            }))
            .unwrap();
        let dispatcher = LlmCronDispatcher::from_registry(
            Arc::new(registry),
            routed_llm_cfg(),
            HashMap::new(),
            Some(ModelConfig {
                provider: "stub".into(),
                model: "fallback-model".into(),
            }),
        );

        let mut e = entry("hello", true);
        e.model_provider = Some("stub".into());
        e.model_name = Some("campaign-model".into());

        dispatcher.fire(&e).await.unwrap();
        dispatcher.fire(&e).await.unwrap();

        let built = builds.lock().unwrap().clone();
        assert_eq!(built, vec!["campaign-model".to_string()]);
        let seen = seen_request_models.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec!["campaign-model".to_string(), "campaign-model".to_string()]
        );
    }

    #[tokio::test]
    async fn routed_dispatcher_falls_back_for_legacy_rows() {
        let builds = Arc::new(Mutex::new(Vec::new()));
        let seen_request_models = Arc::new(Mutex::new(Vec::new()));
        let mut registry = LlmRegistry::new();
        registry
            .register(Box::new(RoutedMockFactory {
                builds: Arc::clone(&builds),
                seen_request_models: Arc::clone(&seen_request_models),
            }))
            .unwrap();
        let dispatcher = LlmCronDispatcher::from_registry(
            Arc::new(registry),
            routed_llm_cfg(),
            HashMap::new(),
            Some(ModelConfig {
                provider: "stub".into(),
                model: "fallback-model".into(),
            }),
        );

        dispatcher.fire(&entry("legacy", true)).await.unwrap();
        let seen = seen_request_models.lock().unwrap().clone();
        assert_eq!(seen, vec!["fallback-model".to_string()]);
    }

    #[tokio::test]
    async fn routed_dispatcher_uses_binding_model_for_legacy_rows_before_fallback() {
        let builds = Arc::new(Mutex::new(Vec::new()));
        let seen_request_models = Arc::new(Mutex::new(Vec::new()));
        let mut registry = LlmRegistry::new();
        registry
            .register(Box::new(RoutedMockFactory {
                builds: Arc::clone(&builds),
                seen_request_models: Arc::clone(&seen_request_models),
            }))
            .unwrap();
        let mut by_binding = HashMap::new();
        by_binding.insert(
            "agent-marketing".to_string(),
            ModelConfig {
                provider: "stub".into(),
                model: "binding-model".into(),
            },
        );
        let dispatcher = LlmCronDispatcher::from_registry(
            Arc::new(registry),
            routed_llm_cfg(),
            by_binding,
            Some(ModelConfig {
                provider: "stub".into(),
                model: "fallback-model".into(),
            }),
        );

        let mut e = entry("legacy", true);
        e.binding_id = "agent-marketing".into();
        dispatcher.fire(&e).await.unwrap();

        let seen = seen_request_models.lock().unwrap().clone();
        assert_eq!(seen, vec!["binding-model".to_string()]);
        let built = builds.lock().unwrap().clone();
        assert_eq!(built, vec!["binding-model".to_string()]);
    }
}
