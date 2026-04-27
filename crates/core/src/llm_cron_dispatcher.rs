//! Phase 79.7 follow-up — `LlmCronDispatcher`.
//!
//! Real LLM-call cron dispatcher: builds a `ChatRequest` from the
//! entry's prompt, calls `LlmClient::chat`, and logs the response
//! body. Outbound publish to the binding's channel is the
//! remaining follow-up — once shipped, this dispatcher will route
//! the response through the broker like Phase 20 `agent_turn`
//! does.
//!
//! Reference (PRIMARY):
//!   * Phase 20 `agent_turn` poller (`crates/poller/src/builtins/agent_turn.rs:157-260`)
//!     — same pattern: build messages, call `chat`, take response.
//!   * `claude-code-leak/src/utils/cronTasks.ts` — sequential
//!     fire loop the leak uses.
//!
//! Design choices:
//!
//! - The dispatcher takes a pre-built `Arc<dyn LlmClient>` rather
//!   than `(LlmRegistry, LlmConfig, ModelConfig)`. Production
//!   wiring in `src/main.rs` builds the client once at boot from
//!   the first agent's `model` config; per-entry model override is
//!   a sub-follow-up. Tests use a mock client without touching
//!   the registry.
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
use nexo_broker::{BrokerHandle, Event};
use nexo_llm::{ChatMessage, ChatRequest, ChatRole, LlmClient, ResponseContent};
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
    client: Arc<dyn LlmClient>,
    system_prompt: Option<String>,
    max_tokens: Option<u32>,
    publisher: Option<Arc<dyn ChannelPublisher>>,
}

impl LlmCronDispatcher {
    pub fn new(client: Arc<dyn LlmClient>) -> Self {
        Self {
            client,
            system_prompt: None,
            max_tokens: None,
            publisher: None,
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
}

#[async_trait]
impl CronDispatcher for LlmCronDispatcher {
    async fn fire(&self, entry: &CronEntry) -> anyhow::Result<()> {
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

        let mut req = ChatRequest::new(self.client.model_id(), messages);
        if let Some(mt) = self.max_tokens {
            req.max_tokens = mt;
        }

        let response =
            self.client.chat(req).await.map_err(|e| {
                anyhow::anyhow!("LLM chat failed for cron entry `{}`: {e}", entry.id)
            })?;

        let body = match &response.content {
            ResponseContent::Text(s) => s.clone(),
            ResponseContent::ToolCalls(calls) => {
                // Cron firing doesn't (yet) execute tool calls — log
                // the names so operators see what the model wanted.
                let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
                format!("(tool_calls: {})", names.join(", "))
            }
        };

        let text_chars = body.chars().count();
        let truncated_preview: String = body.chars().take(200).collect();

        tracing::info!(
            id = %entry.id,
            binding_id = %entry.binding_id,
            cron = %entry.cron_expr,
            recurring = entry.recurring,
            channel = ?entry.channel,
            recipient = ?entry.recipient,
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

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_llm::{ChatResponse, FinishReason, ResponseContent, TokenUsage};
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
            recurring,
            created_at: 0,
            next_fire_at: 0,
            last_fired_at: None,
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
}
