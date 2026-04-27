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
use nexo_llm::{ChatMessage, ChatRequest, ChatRole, LlmClient, ResponseContent};
use std::sync::Arc;

pub struct LlmCronDispatcher {
    client: Arc<dyn LlmClient>,
    system_prompt: Option<String>,
    max_tokens: Option<u32>,
}

impl LlmCronDispatcher {
    pub fn new(client: Arc<dyn LlmClient>) -> Self {
        Self {
            client,
            system_prompt: None,
            max_tokens: None,
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

        let response = self.client.chat(req).await.map_err(|e| {
            anyhow::anyhow!(
                "LLM chat failed for cron entry `{}`: {e}",
                entry.id
            )
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
            response_chars = text_chars,
            preview = %truncated_preview,
            "[cron] llm response (outbound publish is a Phase 79.7 sub-follow-up)"
        );

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
        let dispatcher =
            LlmCronDispatcher::new(mock.clone()).with_system_prompt("Be terse.");
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

    #[tokio::test]
    async fn model_id_taken_from_client() {
        let mock = MockLlmClient::new("ok");
        let dispatcher = LlmCronDispatcher::new(mock.clone());
        dispatcher.fire(&entry("x", true)).await.unwrap();
        let reqs = mock.captured_requests();
        assert_eq!(reqs[0].model, "mock-model");
    }
}
