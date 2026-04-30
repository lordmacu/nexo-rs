//! Standalone turn loop using [`nexo_llm::LlmClient`] directly.
//! Step 80.19 / 6.
//!
//! Equivalent to KAIROS's `query()` invoked inside `runForkedAgent`
//! (leak `forkedAgent.ts:489-541`). Does NOT go through Phase 67
//! driver-loop, which is goal-flow heavyweight (claude subprocess +
//! workspace + acceptance + binding store).
//!
//! Cache-key invariant from leak `:522-525` is enforced by
//! [`CacheSafeParams`] not filtering incomplete tool_use blocks.
//! Phase 77.4 cache-break detection reuses `CacheUsage::hit_ratio` —
//! we emit a `WARN` on the `fork.cache_break_detected` target when
//! `hit_ratio < 0.5` so dashboards can pick it up.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use nexo_llm::types::{
    CacheUsage, ChatMessage, ChatRequest, ChatResponse, ChatRole, FinishReason, ResponseContent,
    TokenUsage, ToolCall,
};
use nexo_llm::LlmClient;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::cache_safe::CacheSafeParams;
use crate::error::ForkError;
use crate::on_message::OnMessage;
use crate::tool_filter::ToolFilter;

/// How fork tool calls are serviced. The fork loop calls
/// [`dispatch`](Self::dispatch) for every approved `ToolCall`; the
/// returned string becomes the synthetic `tool_result` body.
///
/// Implementations adapt to the parent's tool registry. Phase 80.1
/// autoDream's dispatcher routes through the parent's normal
/// `ToolRegistry` so the fork can reuse the existing `FileEdit` /
/// `FileWrite` / `Bash` / etc. handlers — but only the ones the
/// [`ToolFilter`] approves.
#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    async fn dispatch(&self, tool_name: &str, args: Value) -> Result<String, String>;
}

pub struct TurnLoopParams {
    pub llm: Arc<dyn LlmClient>,
    pub cache_safe: CacheSafeParams,
    /// Prompt messages. Prepended with `cache_safe.fork_context_messages`
    /// before the first turn.
    pub prompt_messages: Vec<ChatMessage>,
    pub tool_dispatcher: Arc<dyn ToolDispatcher>,
    pub tool_filter: Arc<dyn ToolFilter>,
    /// Maximum API round-trips before bailing. `0` returns immediately
    /// without firing a turn (caller's signal that the fork should be
    /// validated only).
    pub max_turns: u32,
    pub on_message: Option<Arc<dyn OnMessage>>,
    pub abort: CancellationToken,
    /// Optional critical system reminder injected on every turn as a
    /// system-role message right after the parent prefix. Mirrors
    /// `criticalSystemReminder_EXPERIMENTAL` (leak `:316-318`).
    pub critical_system_reminder: Option<String>,
    /// Telemetry label for traceability.
    pub fork_label: String,
}

#[derive(Debug, Default)]
pub struct TurnLoopResult {
    pub messages: Vec<ChatMessage>,
    pub total_usage: TokenUsage,
    pub total_cache_usage: CacheUsage,
    pub final_text: Option<String>,
    pub turns_executed: u32,
}

/// Run the fork's turn loop to completion (or `max_turns`, or abort).
///
/// Loop semantics:
/// 1. Build a `ChatRequest` from `cache_safe` + accumulated messages.
/// 2. Call `llm.chat(request)`.
/// 3. If response is `Text`: append assistant message, fire `on_message`,
///    break if `finish_reason == Stop | Length`.
/// 4. If response is `ToolCalls`: append assistant tool-calls message,
///    dispatch each call through `tool_filter` then `tool_dispatcher`,
///    append synthesized `tool_result` rows, loop.
/// 5. Cancellation via `abort` returns [`ForkError::Aborted`].
pub async fn run_turn_loop(params: TurnLoopParams) -> Result<TurnLoopResult, ForkError> {
    let TurnLoopParams {
        llm,
        cache_safe,
        prompt_messages,
        tool_dispatcher,
        tool_filter,
        max_turns,
        on_message,
        abort,
        critical_system_reminder,
        fork_label,
    } = params;

    if max_turns == 0 {
        return Ok(TurnLoopResult::default());
    }

    let started = Instant::now();
    let mut messages = cache_safe.fork_context_messages.clone();
    if let Some(reminder) = &critical_system_reminder {
        // Inject as system-role at the start of the per-fork prefix so
        // it shows up before the prompt messages but AFTER the parent
        // prefix (parent's cache hit covers anything before).
        messages.push(ChatMessage::system(reminder.clone()));
    }
    messages.extend(prompt_messages);

    let mut total_usage = TokenUsage::default();
    let mut total_cache = CacheUsage::default();
    let mut turns: u32 = 0;

    while turns < max_turns {
        if abort.is_cancelled() {
            return Err(ForkError::Aborted);
        }

        let request = ChatRequest {
            model: cache_safe.model.clone(),
            messages: messages.clone(),
            tools: cache_safe.tools.clone(),
            max_tokens: cache_safe.max_tokens.unwrap_or(4096),
            temperature: cache_safe.temperature,
            system_prompt: cache_safe.system_prompt.clone(),
            stop_sequences: Vec::new(),
            tool_choice: nexo_llm::types::ToolChoice::Auto,
            system_blocks: cache_safe.system_blocks.clone(),
            cache_tools: cache_safe.cache_tools,
        };

        let response: ChatResponse = tokio::select! {
            biased;
            _ = abort.cancelled() => return Err(ForkError::Aborted),
            r = llm.chat(request) => r.map_err(|e| ForkError::Llm(e.to_string()))?,
        };

        // Accumulate usage
        total_usage.prompt_tokens += response.usage.prompt_tokens;
        total_usage.completion_tokens += response.usage.completion_tokens;
        if let Some(cu) = &response.cache_usage {
            total_cache.cache_read_input_tokens += cu.cache_read_input_tokens;
            total_cache.cache_creation_input_tokens += cu.cache_creation_input_tokens;
            total_cache.input_tokens += cu.input_tokens;
            total_cache.output_tokens += cu.output_tokens;

            // Phase 77.4 — cache-break heuristic. Emit WARN when the
            // hit ratio drops below 50 %. Dashboards alert on this.
            let ratio = cu.hit_ratio();
            if turns == 0 && ratio < 0.5 && cu.cache_creation_input_tokens > 0 {
                warn!(
                    target: "fork.cache_break_detected",
                    fork_label = %fork_label,
                    cache_read = cu.cache_read_input_tokens,
                    cache_creation = cu.cache_creation_input_tokens,
                    hit_ratio = ratio,
                    "fork prompt cache likely missed — verify CacheSafeParams parity"
                );
            }
        }
        turns += 1;

        match response.content {
            ResponseContent::Text(text) => {
                let asst = ChatMessage::assistant(text);
                if let Some(cb) = &on_message {
                    cb.on_message(&asst).await;
                }
                let stop = matches!(
                    response.finish_reason,
                    FinishReason::Stop | FinishReason::Length
                );
                messages.push(asst);
                if stop {
                    break;
                }
            }
            ResponseContent::ToolCalls(calls) => {
                let asst = ChatMessage::assistant_tool_calls(calls.clone(), String::new());
                if let Some(cb) = &on_message {
                    cb.on_message(&asst).await;
                }
                messages.push(asst);

                for call in calls {
                    let result_str = dispatch_one(
                        &*tool_filter,
                        &*tool_dispatcher,
                        &call,
                        &fork_label,
                    )
                    .await;
                    let result_msg =
                        ChatMessage::tool_result(call.id.clone(), call.name.clone(), result_str);
                    if let Some(cb) = &on_message {
                        cb.on_message(&result_msg).await;
                    }
                    messages.push(result_msg);
                }
            }
        }
    }

    let final_text = messages.iter().rev().find_map(|m| {
        if matches!(m.role, ChatRole::Assistant) && !m.content.is_empty() {
            Some(m.content.clone())
        } else {
            None
        }
    });

    debug!(
        target: "fork.turn_loop",
        fork_label = %fork_label,
        turns,
        elapsed_ms = started.elapsed().as_millis() as u64,
        prompt_tokens = total_usage.prompt_tokens,
        completion_tokens = total_usage.completion_tokens,
        cache_read = total_cache.cache_read_input_tokens,
        cache_creation = total_cache.cache_creation_input_tokens,
        "turn loop complete"
    );

    Ok(TurnLoopResult {
        messages,
        total_usage,
        total_cache_usage: total_cache,
        final_text,
        turns_executed: turns,
    })
}

async fn dispatch_one(
    filter: &dyn ToolFilter,
    dispatcher: &dyn ToolDispatcher,
    call: &ToolCall,
    fork_label: &str,
) -> String {
    if !filter.allows(&call.name, &call.arguments) {
        debug!(
            target: "fork.tool_filter",
            fork_label = %fork_label,
            tool_name = %call.name,
            "tool denied by filter"
        );
        return filter.denial_message(&call.name);
    }
    match dispatcher.dispatch(&call.name, call.arguments.clone()).await {
        Ok(s) => s,
        Err(e) => format!("Error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_safe::CacheSafeParams;
    use crate::tool_filter::AllowAllFilter;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use nexo_llm::stream::StreamChunk;
    use nexo_llm::types::{ChatRequest, ChatResponse, ResponseContent, ToolCall, ToolDef};
    use std::sync::Mutex;

    fn mk_cache_safe() -> CacheSafeParams {
        CacheSafeParams::from_parent_request(&ChatRequest {
            model: "test-model".into(),
            messages: vec![ChatMessage::user("parent")],
            tools: vec![ToolDef {
                name: "echo".into(),
                description: "echo".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            max_tokens: 1024,
            temperature: 0.0,
            system_prompt: Some("system".into()),
            stop_sequences: vec![],
            tool_choice: nexo_llm::types::ToolChoice::Auto,
            system_blocks: vec![],
            cache_tools: false,
        })
    }

    /// Mock LLM with scripted responses. Each call to `chat` consumes
    /// the next scripted response. Exhausted → error.
    struct MockLlm {
        responses: Mutex<std::collections::VecDeque<ChatResponse>>,
        captured_requests: Mutex<Vec<ChatRequest>>,
    }

    impl MockLlm {
        fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses.into()),
                captured_requests: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.captured_requests.lock().unwrap().push(req);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("mock llm exhausted"))
        }
        fn model_id(&self) -> &str {
            "test-model"
        }
        fn provider(&self) -> &str {
            "mock"
        }
        async fn stream<'a>(
            &'a self,
            _req: ChatRequest,
        ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
            anyhow::bail!("stream not used in tests")
        }
    }

    struct EchoDispatcher;
    #[async_trait]
    impl ToolDispatcher for EchoDispatcher {
        async fn dispatch(&self, name: &str, args: Value) -> Result<String, String> {
            Ok(format!("dispatched:{name}:{args}"))
        }
    }

    fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            content: ResponseContent::Text(text.into()),
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
            },
            finish_reason: FinishReason::Stop,
            cache_usage: None,
        }
    }

    fn tool_call_response(call_id: &str, name: &str) -> ChatResponse {
        ChatResponse {
            content: ResponseContent::ToolCalls(vec![ToolCall {
                id: call_id.into(),
                name: name.into(),
                arguments: serde_json::json!({"x": 1}),
            }]),
            usage: TokenUsage {
                prompt_tokens: 8,
                completion_tokens: 4,
            },
            finish_reason: FinishReason::ToolUse,
            cache_usage: None,
        }
    }

    #[tokio::test]
    async fn single_turn_text_completes_with_final_text() {
        let llm = MockLlm::new(vec![text_response("done")]);
        let result = run_turn_loop(TurnLoopParams {
            llm,
            cache_safe: mk_cache_safe(),
            prompt_messages: vec![ChatMessage::user("go")],
            tool_dispatcher: Arc::new(EchoDispatcher),
            tool_filter: Arc::new(AllowAllFilter),
            max_turns: 5,
            on_message: None,
            abort: CancellationToken::new(),
            critical_system_reminder: None,
            fork_label: "test".into(),
        })
        .await
        .unwrap();
        assert_eq!(result.turns_executed, 1);
        assert_eq!(result.final_text.as_deref(), Some("done"));
        assert_eq!(result.total_usage.prompt_tokens, 10);
    }

    #[tokio::test]
    async fn tool_call_then_text_runs_two_turns_and_dispatches() {
        let llm = MockLlm::new(vec![
            tool_call_response("c1", "echo"),
            text_response("after tool"),
        ]);
        let result = run_turn_loop(TurnLoopParams {
            llm: llm.clone(),
            cache_safe: mk_cache_safe(),
            prompt_messages: vec![ChatMessage::user("hi")],
            tool_dispatcher: Arc::new(EchoDispatcher),
            tool_filter: Arc::new(AllowAllFilter),
            max_turns: 5,
            on_message: None,
            abort: CancellationToken::new(),
            critical_system_reminder: None,
            fork_label: "test".into(),
        })
        .await
        .unwrap();
        assert_eq!(result.turns_executed, 2);
        assert_eq!(result.final_text.as_deref(), Some("after tool"));
        // Verify the second request includes the tool_result row.
        let captured = llm.captured_requests.lock().unwrap();
        let second = &captured[1];
        let tool_msg = second
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, ChatRole::Tool))
            .expect("expected tool_result row in second turn");
        assert!(tool_msg.content.starts_with("dispatched:echo"));
    }

    #[tokio::test]
    async fn aborted_returns_aborted_error() {
        let llm = MockLlm::new(vec![text_response("never returned")]);
        let abort = CancellationToken::new();
        abort.cancel();
        let err = run_turn_loop(TurnLoopParams {
            llm,
            cache_safe: mk_cache_safe(),
            prompt_messages: vec![],
            tool_dispatcher: Arc::new(EchoDispatcher),
            tool_filter: Arc::new(AllowAllFilter),
            max_turns: 5,
            on_message: None,
            abort,
            critical_system_reminder: None,
            fork_label: "test".into(),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, ForkError::Aborted));
    }

    #[tokio::test]
    async fn max_turns_zero_returns_immediately() {
        let llm = MockLlm::new(vec![]);
        let result = run_turn_loop(TurnLoopParams {
            llm,
            cache_safe: mk_cache_safe(),
            prompt_messages: vec![],
            tool_dispatcher: Arc::new(EchoDispatcher),
            tool_filter: Arc::new(AllowAllFilter),
            max_turns: 0,
            on_message: None,
            abort: CancellationToken::new(),
            critical_system_reminder: None,
            fork_label: "test".into(),
        })
        .await
        .unwrap();
        assert_eq!(result.turns_executed, 0);
        assert!(result.final_text.is_none());
    }

    #[tokio::test]
    async fn tool_filter_denial_substitutes_message_without_dispatch() {
        #[derive(Debug)]
        struct DenyAll;
        impl ToolFilter for DenyAll {
            fn allows(&self, _: &str, _: &Value) -> bool {
                false
            }
            fn denial_message(&self, name: &str) -> String {
                format!("tool {name} denied")
            }
        }

        struct PanicDispatcher;
        #[async_trait]
        impl ToolDispatcher for PanicDispatcher {
            async fn dispatch(&self, _: &str, _: Value) -> Result<String, String> {
                panic!("dispatcher should not run when filter denies");
            }
        }

        let llm = MockLlm::new(vec![
            tool_call_response("c1", "echo"),
            text_response("after denied"),
        ]);
        let result = run_turn_loop(TurnLoopParams {
            llm: llm.clone(),
            cache_safe: mk_cache_safe(),
            prompt_messages: vec![],
            tool_dispatcher: Arc::new(PanicDispatcher),
            tool_filter: Arc::new(DenyAll),
            max_turns: 5,
            on_message: None,
            abort: CancellationToken::new(),
            critical_system_reminder: None,
            fork_label: "test".into(),
        })
        .await
        .unwrap();
        assert_eq!(result.turns_executed, 2);
        let captured = llm.captured_requests.lock().unwrap();
        let second = &captured[1];
        let tool_msg = second
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, ChatRole::Tool))
            .expect("expected tool_result row");
        assert_eq!(tool_msg.content, "tool echo denied");
    }

    #[tokio::test]
    async fn cache_usage_aggregates_across_turns() {
        let with_cache = ChatResponse {
            content: ResponseContent::Text("ok".into()),
            usage: TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
            },
            finish_reason: FinishReason::Stop,
            cache_usage: Some(CacheUsage {
                cache_read_input_tokens: 800,
                cache_creation_input_tokens: 50,
                input_tokens: 25,
                output_tokens: 20,
            }),
        };
        let llm = MockLlm::new(vec![with_cache]);
        let result = run_turn_loop(TurnLoopParams {
            llm,
            cache_safe: mk_cache_safe(),
            prompt_messages: vec![ChatMessage::user("ping")],
            tool_dispatcher: Arc::new(EchoDispatcher),
            tool_filter: Arc::new(AllowAllFilter),
            max_turns: 5,
            on_message: None,
            abort: CancellationToken::new(),
            critical_system_reminder: None,
            fork_label: "cache_test".into(),
        })
        .await
        .unwrap();
        assert_eq!(result.total_cache_usage.cache_read_input_tokens, 800);
        assert_eq!(result.total_cache_usage.cache_creation_input_tokens, 50);
    }

    #[tokio::test]
    async fn critical_system_reminder_injected_at_prefix() {
        let llm = MockLlm::new(vec![text_response("ok")]);
        let result = run_turn_loop(TurnLoopParams {
            llm: llm.clone(),
            cache_safe: mk_cache_safe(),
            prompt_messages: vec![ChatMessage::user("hi")],
            tool_dispatcher: Arc::new(EchoDispatcher),
            tool_filter: Arc::new(AllowAllFilter),
            max_turns: 5,
            on_message: None,
            abort: CancellationToken::new(),
            critical_system_reminder: Some("critical reminder".into()),
            fork_label: "test".into(),
        })
        .await
        .unwrap();
        assert_eq!(result.turns_executed, 1);
        let captured = llm.captured_requests.lock().unwrap();
        let first = &captured[0];
        // Find the system-role reminder; it sits between parent prefix and prompt.
        let reminder = first
            .messages
            .iter()
            .find(|m| matches!(m.role, ChatRole::System));
        assert!(reminder.is_some());
        assert_eq!(reminder.unwrap().content, "critical reminder");
    }
}
