//! `DefaultSamplingProvider` — wraps `agent_llm::LlmClient` instances
//! and maps between MCP sampling requests and our `ChatRequest` /
//! `ChatResponse`.
//!
//! Resolution order for `modelPreferences.hints`:
//! 1. Exact match against any named client's `provider()` or `model_id()`.
//! 2. Fall back to the configured `default` client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use agent_llm::types::{ChatMessage, ChatRequest, FinishReason, ResponseContent};
use agent_llm::LlmClient;

use super::policy::SamplingPolicy;
use super::types::{
    IncludeContext, SamplingMessage, SamplingRequest, SamplingResponse, SamplingRole, StopReason,
};
use super::{SamplingError, SamplingProvider};

/// Hard ceiling on how long an LLM call can block a sampling request.
/// Chosen to be less than typical MCP server call timeouts.
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct DefaultSamplingProvider {
    default: Arc<dyn LlmClient>,
    /// Named clients keyed by either `provider()` or `model_id()` for
    /// hint-based resolution.
    named: HashMap<String, Arc<dyn LlmClient>>,
    policy: SamplingPolicy,
}

impl DefaultSamplingProvider {
    pub fn new(
        default: Arc<dyn LlmClient>,
        named: HashMap<String, Arc<dyn LlmClient>>,
        policy: SamplingPolicy,
    ) -> Self {
        Self {
            default,
            named,
            policy,
        }
    }

    fn resolve_client(&self, req: &SamplingRequest) -> Arc<dyn LlmClient> {
        if let Some(mp) = &req.model_preferences {
            for hint in &mp.hints {
                if let Some(c) = self.named.get(hint) {
                    return c.clone();
                }
            }
        }
        self.default.clone()
    }
}

#[async_trait]
impl SamplingProvider for DefaultSamplingProvider {
    async fn sample(&self, req: SamplingRequest) -> Result<SamplingResponse, SamplingError> {
        self.policy.check(&req)?;
        let req = self.policy.cap(&req);

        if !matches!(req.include_context, IncludeContext::None) {
            tracing::warn!(
                server = %req.server_id,
                include_context = ?req.include_context,
                "sampling includeContext not supported — ignoring"
            );
        }

        let client = self.resolve_client(&req);
        let chat_req = map_to_chat_request(&req, client.model_id());

        let fut = client.chat(chat_req);
        let resp = tokio::time::timeout(SAMPLE_TIMEOUT, fut)
            .await
            .map_err(|_| SamplingError::LlmError("sampling chat() timed out".into()))?
            .map_err(|e| SamplingError::LlmError(e.to_string()))?;

        map_from_chat_response(resp, client.model_id())
    }
}

fn map_to_chat_request(req: &SamplingRequest, model: &str) -> ChatRequest {
    let messages: Vec<ChatMessage> = req
        .messages
        .iter()
        .map(|m: &SamplingMessage| match m.role {
            SamplingRole::User => ChatMessage::user(m.text.clone()),
            SamplingRole::Assistant => ChatMessage::assistant(m.text.clone()),
        })
        .collect();
    let mut out = ChatRequest::new(model.to_string(), messages);
    out.system_prompt = req.system_prompt.clone();
    if let Some(t) = req.temperature {
        out.temperature = t;
    }
    out.max_tokens = req.max_tokens;
    out.stop_sequences = req.stop_sequences.clone();
    out
}

fn map_from_chat_response(
    resp: agent_llm::types::ChatResponse,
    model: &str,
) -> Result<SamplingResponse, SamplingError> {
    let text = match resp.content {
        ResponseContent::Text(t) => t,
        ResponseContent::ToolCalls(_) => return Err(SamplingError::ToolCallsRejected),
    };
    let stop_reason = match resp.finish_reason {
        FinishReason::Stop => StopReason::EndTurn,
        FinishReason::Length => StopReason::MaxTokens,
        FinishReason::ToolUse => {
            tracing::warn!("sampling finish_reason=ToolUse mapped to endTurn");
            StopReason::EndTurn
        }
        FinishReason::Other(ref s) => {
            tracing::warn!(reason = %s, "sampling finish_reason unknown → endTurn");
            StopReason::EndTurn
        }
    };
    Ok(SamplingResponse {
        role: SamplingRole::Assistant,
        text,
        model: model.to_string(),
        stop_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampling::types::{ModelPreferences, SamplingMessage};
    use agent_llm::types::{ChatResponse, TokenUsage, ToolCall};

    struct FakeClient {
        provider: &'static str,
        model: &'static str,
        resp: ChatResponse,
    }

    #[async_trait]
    impl LlmClient for FakeClient {
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            Ok(self.resp.clone())
        }
        fn model_id(&self) -> &str {
            self.model
        }
        fn provider(&self) -> &str {
            self.provider
        }
    }

    fn fake(resp: ChatResponse) -> Arc<dyn LlmClient> {
        Arc::new(FakeClient {
            provider: "fake",
            model: "fake-1",
            resp,
        })
    }

    fn text_resp(s: &str) -> ChatResponse {
        ChatResponse {
            content: ResponseContent::Text(s.into()),
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Stop,
        }
    }

    fn req(hints: Vec<&str>) -> SamplingRequest {
        SamplingRequest {
            server_id: "srv".into(),
            messages: vec![SamplingMessage {
                role: SamplingRole::User,
                text: "hi".into(),
            }],
            model_preferences: Some(ModelPreferences {
                hints: hints.into_iter().map(String::from).collect(),
                cost_priority: None,
                speed_priority: None,
                intelligence_priority: None,
            }),
            system_prompt: None,
            include_context: IncludeContext::None,
            temperature: None,
            max_tokens: 128,
            stop_sequences: vec![],
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn resolves_default_when_no_hints() {
        let provider = DefaultSamplingProvider::new(
            fake(text_resp("hello")),
            HashMap::new(),
            SamplingPolicy::permissive_for_tests(),
        );
        let r = provider.sample(req(vec![])).await.unwrap();
        assert_eq!(r.text, "hello");
        assert_eq!(r.model, "fake-1");
        assert_eq!(r.stop_reason, StopReason::EndTurn);
    }

    #[tokio::test]
    async fn resolves_by_hint_exact_match() {
        let mut named: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
        named.insert(
            "gpt-5".into(),
            Arc::new(FakeClient {
                provider: "openai",
                model: "gpt-5",
                resp: text_resp("from-gpt"),
            }),
        );
        let provider = DefaultSamplingProvider::new(
            fake(text_resp("default")),
            named,
            SamplingPolicy::permissive_for_tests(),
        );
        let r = provider.sample(req(vec!["gpt-5"])).await.unwrap();
        assert_eq!(r.text, "from-gpt");
        assert_eq!(r.model, "gpt-5");
    }

    #[tokio::test]
    async fn falls_back_to_default_when_no_hint_matches() {
        let provider = DefaultSamplingProvider::new(
            fake(text_resp("default")),
            HashMap::new(),
            SamplingPolicy::permissive_for_tests(),
        );
        let r = provider.sample(req(vec!["nonexistent"])).await.unwrap();
        assert_eq!(r.text, "default");
    }

    #[tokio::test]
    async fn rejects_tool_calls_response() {
        let resp = ChatResponse {
            content: ResponseContent::ToolCalls(vec![ToolCall {
                id: "c1".into(),
                name: "x".into(),
                arguments: serde_json::json!({}),
            }]),
            usage: TokenUsage::default(),
            finish_reason: FinishReason::ToolUse,
        };
        let provider = DefaultSamplingProvider::new(
            fake(resp),
            HashMap::new(),
            SamplingPolicy::permissive_for_tests(),
        );
        let err = provider.sample(req(vec![])).await.unwrap_err();
        assert!(matches!(err, SamplingError::ToolCallsRejected));
    }

    #[tokio::test]
    async fn maps_length_to_max_tokens() {
        let resp = ChatResponse {
            content: ResponseContent::Text("cut".into()),
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Length,
        };
        let provider = DefaultSamplingProvider::new(
            fake(resp),
            HashMap::new(),
            SamplingPolicy::permissive_for_tests(),
        );
        let r = provider.sample(req(vec![])).await.unwrap();
        assert_eq!(r.stop_reason, StopReason::MaxTokens);
    }
}
