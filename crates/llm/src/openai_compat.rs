use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use nexo_config::types::llm::{LlmProviderConfig, RetryConfig};
use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitError};

use crate::client::LlmClient;
use crate::rate_limiter::RateLimiter;
use crate::retry::{parse_retry_after_ms, with_retry, LlmError};
use crate::stream::{
    ensure_event_stream, parse_openai_sse, record_usage_tap, stream_metrics_tap, StreamChunk,
};
use crate::types::{
    Attachment, AttachmentData, ChatRequest, ChatResponse, ChatRole, FinishReason, ResponseContent,
    TokenUsage, ToolCall, ToolChoice,
};
use futures::stream::BoxStream;

pub struct OpenAiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    rate_limiter: Arc<RateLimiter>,
    circuit: Arc<CircuitBreaker>,
    retry: RetryConfig,
}

impl OpenAiClient {
    pub fn new(cfg: &LlmProviderConfig, model: impl Into<String>, retry: RetryConfig) -> Self {
        if cfg.api_key.trim().is_empty() {
            tracing::warn!(
                "openai: api_key is empty — requests will fail with 401 until a valid key is set"
            );
        }
        let rate_limiter = Arc::new(RateLimiter::with_quota(
            cfg.rate_limit.requests_per_second,
            cfg.rate_limit.quota_alert_threshold,
        ));
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "reqwest client build failed; falling back to default client (no timeout)");
                reqwest::Client::new()
            });

        let circuit = Arc::new(CircuitBreaker::new(
            "llm.openai",
            CircuitBreakerConfig::default(),
        ));

        // Default to the real ChatGPT endpoint when base_url is blank
        // so `provider: openai` in config works out of the box. Any
        // OpenAI-compatible endpoint (Azure, OpenRouter, Groq, LM
        // Studio, vLLM, MiniMax chatcompletion_v2) can still override.
        let base = if cfg.base_url.trim().is_empty() {
            "https://api.openai.com/v1".to_string()
        } else {
            cfg.base_url.trim_end_matches('/').to_string()
        };
        Self {
            http,
            base_url: base,
            api_key: cfg.api_key.clone(),
            model: model.into(),
            rate_limiter,
            circuit,
            retry,
        }
    }

    async fn classify_response(
        &self,
        response: reqwest::Response,
    ) -> Result<reqwest::Response, LlmError> {
        let status = response.status().as_u16();
        if status == 429 {
            let retry_after_ms = parse_retry_after_ms(response.headers(), "retry-after", 30_000);
            return Err(LlmError::RateLimit { retry_after_ms });
        }
        if status >= 500 {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::ServerError { status, body });
        }
        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Other(anyhow::anyhow!("HTTP {status}: {body}")));
        }
        Ok(response)
    }

    async fn do_request(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        validate_request(req)?;
        self.rate_limiter.acquire().await;

        let url = format!("{}/chat/completions", self.base_url);
        let body = build_openai_body(req);

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let response = self.classify_response(response).await?;

        let raw_text = response
            .text()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let raw: OpenAiResponse = serde_json::from_str(&raw_text).map_err(|e| {
            LlmError::Other(anyhow::anyhow!(
                "openai: response parse failed ({e}); body was: {}",
                truncate_for_log(&raw_text, 512)
            ))
        })?;

        let resp = parse_openai_response(raw).map_err(LlmError::Other)?;
        if let Some(tracker) = self.rate_limiter.quota_tracker() {
            tracker.record_usage(resp.usage.prompt_tokens, resp.usage.completion_tokens);
        }
        Ok(resp)
    }

    async fn open_stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, anyhow::Result<StreamChunk>>, LlmError> {
        validate_request(req)?;
        self.rate_limiter.acquire().await;

        let url = format!("{}/chat/completions", self.base_url);
        let mut body = build_openai_body(req);
        body["stream"] = json!(true);
        body["stream_options"] = json!({ "include_usage": true });

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let response = self.classify_response(response).await?;
        let response = ensure_event_stream(response).map_err(LlmError::Other)?;

        let byte_stream = response.bytes_stream();
        Ok(parse_openai_sse(byte_stream))
    }

    async fn do_embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.rate_limiter.acquire().await;
        let url = format!("{}/embeddings", self.base_url);
        let body = json!({
            "model": self.model,
            "input": texts,
        });
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let response = self.classify_response(response).await?;
        let raw_text = response
            .text()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let raw: EmbeddingResponse = serde_json::from_str(&raw_text).map_err(|e| {
            LlmError::Other(anyhow::anyhow!(
                "openai: embed parse failed ({e}); body was: {}",
                truncate_for_log(&raw_text, 512)
            ))
        })?;
        let mut rows = raw.data;
        rows.sort_by_key(|r| r.index);
        Ok(rows.into_iter().map(|r| r.embedding).collect())
    }
}

fn validate_request(req: &ChatRequest) -> Result<(), LlmError> {
    if req.max_tokens == 0 {
        return Err(LlmError::Other(anyhow::anyhow!(
            "openai: max_tokens must be > 0 (got 0)"
        )));
    }
    if req.messages.is_empty() && req.system_prompt.is_none() {
        return Err(LlmError::Other(anyhow::anyhow!(
            "openai: messages cannot be empty when system_prompt is also missing"
        )));
    }
    Ok(())
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, ToolDef};

    fn req_with_tools() -> ChatRequest {
        ChatRequest {
            model: "gpt-4o-mini".into(),
            messages: vec![ChatMessage::user("hola")],
            tools: vec![ToolDef {
                name: "get_weather".into(),
                description: "Weather lookup".into(),
                parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
            }],
            max_tokens: 512,
            temperature: 0.5,
            system_prompt: Some("be brief".into()),
            stop_sequences: vec!["END".into()],
            tool_choice: ToolChoice::Auto,
            system_blocks: Vec::new(),
            cache_tools: false,
        }
    }

    #[test]
    fn body_basic_shape() {
        let body = build_openai_body(&req_with_tools());
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be brief");
        assert_eq!(body["stop"][0], "END");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
    }

    #[test]
    fn tool_choice_variants_map_correctly() {
        let mut r = req_with_tools();
        r.tool_choice = ToolChoice::Auto;
        assert_eq!(build_openai_body(&r)["tool_choice"], json!("auto"));
        r.tool_choice = ToolChoice::Any;
        assert_eq!(build_openai_body(&r)["tool_choice"], json!("required"));
        r.tool_choice = ToolChoice::None;
        assert_eq!(build_openai_body(&r)["tool_choice"], json!("none"));
        r.tool_choice = ToolChoice::Specific("get_weather".into());
        let b = build_openai_body(&r);
        assert_eq!(b["tool_choice"]["type"], "function");
        assert_eq!(b["tool_choice"]["function"]["name"], "get_weather");
    }

    #[test]
    fn user_image_becomes_multipart_content() {
        let mut r = req_with_tools();
        r.messages = vec![ChatMessage {
            role: ChatRole::User,
            content: "describe".into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
            attachments: vec![Attachment::image_base64("image/png", "aGVsbG8=")],
        }];
        let body = build_openai_body(&r);
        let content = &body["messages"][1]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert!(content[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn assistant_tool_calls_emit_tool_calls_array() {
        let mut r = req_with_tools();
        r.messages.push(ChatMessage::assistant_tool_calls(
            vec![ToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                arguments: json!({"city":"Bogota"}),
            }],
            "",
        ));
        r.messages.push(ChatMessage::tool_result(
            "call_1",
            "get_weather",
            "{\"temp\":22}",
        ));
        let body = build_openai_body(&r);
        let messages = body["messages"].as_array().unwrap();
        let assistant = messages.iter().find(|m| m["role"] == "assistant").unwrap();
        assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
        assert_eq!(assistant["tool_calls"][0]["type"], "function");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        // arguments field is a JSON-encoded string per OpenAI wire.
        let args_str = assistant["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        let args: Value = serde_json::from_str(args_str).unwrap();
        assert_eq!(args["city"], "Bogota");

        let tool_msg = messages.last().unwrap();
        assert_eq!(tool_msg["role"], "tool");
        assert_eq!(tool_msg["tool_call_id"], "call_1");
    }

    #[test]
    fn path_attachment_is_skipped_with_warning() {
        let att = Attachment::image_path("image/jpeg", "/tmp/noent.jpg");
        assert!(openai_image_part(&att).is_none());
    }

    #[test]
    fn empty_base_url_defaults_to_openai() {
        let cfg = LlmProviderConfig {
            api_key: "sk-foo".into(),
            base_url: "".into(),
            group_id: None,
            rate_limit: Default::default(),
            auth: None,
            api_flavor: None,
            embedding_model: None,
            safety_settings: None,
        };
        let retry = RetryConfig {
            max_attempts: 1,
            initial_backoff_ms: 10,
            max_backoff_ms: 10,
            backoff_multiplier: 1.0,
        };
        let c = OpenAiClient::new(&cfg, "gpt-4o-mini", retry);
        // Inspect via model_id which is the only public accessor;
        // actual URL correctness is covered by the integration test.
        assert_eq!(c.model_id(), "gpt-4o-mini");
    }

    // ---- Phase A.3 cache_usage parsing ----

    #[test]
    fn parse_response_with_cached_tokens_emits_cache_usage() {
        let raw_json = r#"{
          "choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop","index":0}],
          "usage":{
            "prompt_tokens": 1500,
            "completion_tokens": 20,
            "prompt_tokens_details": { "cached_tokens": 1200 }
          }
        }"#;
        let parsed: OpenAiResponse = serde_json::from_str(raw_json).unwrap();
        let resp = parse_openai_response(parsed).unwrap();
        let cu = resp.cache_usage.expect("cache_usage populated");
        assert_eq!(cu.cache_read_input_tokens, 1200);
        assert_eq!(cu.cache_creation_input_tokens, 0);
        assert_eq!(cu.input_tokens, 300); // 1500 - 1200
        assert_eq!(cu.output_tokens, 20);
        assert!(cu.hit_ratio() > 0.79);
    }

    #[test]
    fn parse_response_without_cached_tokens_leaves_none() {
        let raw_json = r#"{
          "choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop","index":0}],
          "usage":{ "prompt_tokens": 100, "completion_tokens": 10 }
        }"#;
        let parsed: OpenAiResponse = serde_json::from_str(raw_json).unwrap();
        let resp = parse_openai_response(parsed).unwrap();
        assert!(resp.cache_usage.is_none());
    }

    #[test]
    fn parse_response_with_zero_cached_tokens_is_treated_as_no_hit() {
        let raw_json = r#"{
          "choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop","index":0}],
          "usage":{
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "prompt_tokens_details": { "cached_tokens": 0 }
          }
        }"#;
        let parsed: OpenAiResponse = serde_json::from_str(raw_json).unwrap();
        let resp = parse_openai_response(parsed).unwrap();
        assert!(resp.cache_usage.is_none());
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let retry = self.retry.clone();
        match self
            .circuit
            .call(|| with_retry(&retry, || self.do_request(&req)))
            .await
        {
            Ok(resp) => Ok(resp),
            Err(CircuitError::Open(name)) => {
                Err(anyhow::anyhow!("circuit breaker `{name}` is open"))
            }
            Err(CircuitError::Inner(e)) => Err(anyhow::anyhow!("{e}")),
        }
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn provider(&self) -> &str {
        "openai"
    }

    async fn stream<'a>(
        &'a self,
        req: ChatRequest,
    ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
        let retry = self.retry.clone();
        match self
            .circuit
            .call(|| with_retry(&retry, || self.open_stream(&req)))
            .await
        {
            Ok(s) => Ok(stream_metrics_tap(
                record_usage_tap(s, self.rate_limiter.clone()),
                self.provider(),
            )),
            Err(CircuitError::Open(name)) => {
                Err(anyhow::anyhow!("circuit breaker `{name}` is open"))
            }
            Err(CircuitError::Inner(e)) => Err(anyhow::anyhow!("{e}")),
        }
    }

    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let retry = self.retry.clone();
        match self
            .circuit
            .call(|| with_retry(&retry, || self.do_embed(texts)))
            .await
        {
            Ok(v) => Ok(v),
            Err(CircuitError::Open(name)) => {
                Err(anyhow::anyhow!("circuit breaker `{name}` is open"))
            }
            Err(CircuitError::Inner(e)) => Err(anyhow::anyhow!("{e}")),
        }
    }
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingRow>,
}

#[derive(Deserialize)]
struct EmbeddingRow {
    index: usize,
    embedding: Vec<f32>,
}

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: OpenAiUsage,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct OpenAiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCall>,
}

#[derive(Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiFunction,
}

#[derive(Deserialize)]
struct OpenAiFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    /// OpenAI / DeepSeek / OpenAI-compat providers report cache hits
    /// inside `prompt_tokens_details.cached_tokens`. The field appears
    /// only when the prefix matched (>1024 tokens for OpenAI today),
    /// so the optional wrapper covers both pre-caching APIs and
    /// first-write turns where nothing was hit.
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Deserialize, Default)]
struct OpenAiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}

fn parse_openai_response(raw: OpenAiResponse) -> anyhow::Result<ChatResponse> {
    let choice = raw
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("OpenAI returned no choices"))?;

    let finish_reason = match choice.finish_reason.as_deref() {
        Some("stop") => FinishReason::Stop,
        Some("tool_calls") => FinishReason::ToolUse,
        Some("length") => FinishReason::Length,
        Some(other) => FinishReason::Other(other.to_string()),
        None => FinishReason::Stop,
    };

    let usage = TokenUsage {
        prompt_tokens: raw.usage.prompt_tokens,
        completion_tokens: raw.usage.completion_tokens,
    };

    // Phase A.3 — OpenAI-style automatic prefix caching reports hits
    // through `prompt_tokens_details.cached_tokens`. Only emit a
    // `CacheUsage` when at least one cached token came back so
    // dashboards don't accumulate denominator-only entries on every
    // turn. Same field shape works for DeepSeek (OpenAI-compat) and
    // most OpenAI-compatible gateways.
    let cache_usage = raw.usage.prompt_tokens_details.as_ref().and_then(|d| {
        if d.cached_tokens == 0 {
            None
        } else {
            Some(crate::types::CacheUsage {
                cache_read_input_tokens: d.cached_tokens,
                // OpenAI's caching is automatic (no per-write
                // accounting); leave creation at zero.
                cache_creation_input_tokens: 0,
                // `prompt_tokens` here is total (cached + uncached),
                // so the uncached portion is the difference. Saturating
                // sub keeps the field non-negative if a future API
                // change inverts the semantics.
                input_tokens: raw.usage.prompt_tokens.saturating_sub(d.cached_tokens),
                output_tokens: raw.usage.completion_tokens,
            })
        }
    });

    if !choice.message.tool_calls.is_empty() {
        let calls = choice
            .message
            .tool_calls
            .into_iter()
            .map(|tc| {
                let args = serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
                ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: args,
                }
            })
            .collect();
        return Ok(ChatResponse {
            content: ResponseContent::ToolCalls(calls),
            usage,
            finish_reason: FinishReason::ToolUse,
            cache_usage,
        });
    }

    Ok(ChatResponse {
        content: ResponseContent::Text(choice.message.content.unwrap_or_default()),
        usage,
        finish_reason,
        cache_usage,
    })
}

fn build_openai_body(req: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(system) = &req.system_prompt {
        messages.push(json!({ "role": "system", "content": system }));
    }

    for msg in &req.messages {
        let role = match msg.role {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        };

        // Build content. User turns with image attachments get the
        // multipart content array (`[{type:"text",...},{type:"image_url",...}]`)
        // so vision-capable models (gpt-4o / 4o-mini) can see them.
        // Assistant turns that requested tools get an empty text content
        // plus a parallel `tool_calls` array — OpenAI's required shape
        // for multi-turn tool use.
        let mut m = json!({ "role": role });
        if matches!(msg.role, ChatRole::Assistant) && !msg.tool_calls.is_empty() {
            // Assistant tool-call turn: content may be null / empty,
            // tool_calls carries the structured calls so the follow-up
            // tool role messages correlate via tool_call_id.
            let content: Value = if msg.content.is_empty() {
                Value::Null
            } else {
                Value::String(msg.content.clone())
            };
            m["content"] = content;
            let calls: Vec<Value> = msg
                .tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()),
                        }
                    })
                })
                .collect();
            m["tool_calls"] = json!(calls);
        } else if matches!(msg.role, ChatRole::User) && !msg.attachments.is_empty() {
            let mut parts: Vec<Value> = Vec::new();
            if !msg.content.is_empty() {
                parts.push(json!({"type":"text","text": msg.content}));
            }
            for att in &msg.attachments {
                if let Some(p) = openai_image_part(att) {
                    parts.push(p);
                }
            }
            if parts.is_empty() {
                parts.push(json!({"type":"text","text":""}));
            }
            m["content"] = Value::Array(parts);
        } else {
            m["content"] = Value::String(msg.content.clone());
        }

        if let Some(id) = &msg.tool_call_id {
            m["tool_call_id"] = json!(id);
        }
        if let Some(name) = &msg.name {
            m["name"] = json!(name);
        }
        messages.push(m);
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    });

    if !req.stop_sequences.is_empty() {
        body["stop"] = json!(req.stop_sequences);
    }

    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        body["tools"] = json!(tools);
        body["tool_choice"] = openai_tool_choice(&req.tool_choice);
    }

    body
}

fn openai_tool_choice(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Any => json!("required"),
        ToolChoice::None => json!("none"),
        ToolChoice::Specific(name) => json!({
            "type": "function",
            "function": { "name": name }
        }),
    }
}

fn openai_image_part(att: &Attachment) -> Option<Value> {
    if att.kind != "image" {
        return None;
    }
    let url = match &att.data {
        AttachmentData::Base64 { base64 } => {
            format!("data:{};base64,{}", att.mime_type, base64)
        }
        AttachmentData::Url { url } => url.clone(),
        AttachmentData::Path { path } => {
            tracing::warn!(
                path,
                "openai: Path attachment not materialized; skipping. \
                 Call Attachment::materialize() before sending the request."
            );
            return None;
        }
    };
    Some(json!({
        "type": "image_url",
        "image_url": { "url": url }
    }))
}
