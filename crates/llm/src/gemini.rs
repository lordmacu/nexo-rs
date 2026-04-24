//! Google Gemini API client (`generateContent`).
//!
//! Supports native function calling (functionDeclarations / functionCall /
//! functionResponse), stop sequences, and system instructions.
//!
//! Auth: API key goes in `x-goog-api-key` header so it never leaks in URLs
//! or proxy access logs. Gemini tool calls have no server-side id, so we
//! synthesize one from the call index to keep correlation stable across
//! turns in our `ToolCall` model.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{json, Value};

use agent_config::types::llm::{LlmProviderConfig, RetryConfig};
use agent_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitError};

use crate::client::LlmClient;
use crate::rate_limiter::RateLimiter;
use crate::registry::LlmProviderFactory;
use crate::retry::{parse_retry_after_ms, with_retry, LlmError};
use crate::stream::{parse_gemini_sse, record_usage_tap, StreamChunk};
use crate::types::{
    Attachment, AttachmentData, ChatRequest, ChatResponse, ChatRole, FinishReason,
    ResponseContent, TokenUsage, ToolCall, ToolChoice,
};

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct GeminiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    embedding_model: String,
    safety_settings: Option<Value>,
    rate_limiter: Arc<RateLimiter>,
    circuit: Arc<CircuitBreaker>,
    retry: RetryConfig,
}

impl GeminiClient {
    pub fn new(cfg: &LlmProviderConfig, model: impl Into<String>, retry: RetryConfig) -> Self {
        if cfg.api_key.trim().is_empty() {
            tracing::warn!(
                "gemini: api_key is empty — requests will fail with 401 until a valid key is set"
            );
        }
        let rate_limiter = Arc::new(RateLimiter::with_quota(
            cfg.rate_limit.requests_per_second,
            cfg.rate_limit.quota_alert_threshold,
        ));
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");
        let base = if cfg.base_url.trim().is_empty() {
            DEFAULT_BASE.to_string()
        } else {
            cfg.base_url.trim_end_matches('/').to_string()
        };
        let circuit = Arc::new(CircuitBreaker::new(
            "llm.gemini",
            CircuitBreakerConfig::default(),
        ));
        Self {
            http,
            base_url: base,
            api_key: cfg.api_key.clone(),
            model: model.into(),
            embedding_model: cfg
                .embedding_model
                .clone()
                .unwrap_or_else(|| "text-embedding-004".to_string()),
            safety_settings: cfg.safety_settings.clone(),
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
            let retry_after_ms = parse_retry_after(response.headers()).unwrap_or(30_000);
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
        let url = format!("{}/models/{}:generateContent", self.base_url, self.model);
        let mut body = build_body(req);
        if let Some(ss) = &self.safety_settings {
            body["safetySettings"] = ss.clone();
        }

        let response = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let response = self.classify_response(response).await?;

        let raw_text = response
            .text()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let raw: GeminiResponse = serde_json::from_str(&raw_text).map_err(|e| {
            LlmError::Other(anyhow::anyhow!(
                "gemini: response parse failed ({e}); body was: {}",
                truncate_for_log(&raw_text, 512)
            ))
        })?;
        let resp = to_chat_response(raw);
        if let Some(tracker) = self.rate_limiter.quota_tracker() {
            tracker.record_usage(resp.usage.prompt_tokens, resp.usage.completion_tokens);
        }
        Ok(resp)
    }

    async fn open_stream(&self, req: &ChatRequest) -> Result<reqwest::Response, LlmError> {
        validate_request(req)?;
        self.rate_limiter.acquire().await;
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url, self.model
        );
        let mut body = build_body(req);
        if let Some(ss) = &self.safety_settings {
            body["safetySettings"] = ss.clone();
        }
        let response = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        self.classify_response(response).await
    }

    async fn do_embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        if texts.iter().any(|t| t.is_empty()) {
            return Err(LlmError::Other(anyhow::anyhow!(
                "gemini: embed() received an empty-string input — Gemini rejects those"
            )));
        }
        self.rate_limiter.acquire().await;
        let url = format!(
            "{}/models/{}:batchEmbedContents",
            self.base_url, self.embedding_model
        );
        let requests: Vec<Value> = texts
            .iter()
            .map(|t| {
                json!({
                    "model": format!("models/{}", self.embedding_model),
                    "content": {"parts": [{"text": t}]},
                })
            })
            .collect();
        let body = json!({ "requests": requests });
        let response = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let response = self.classify_response(response).await?;
        let parsed: GeminiBatchEmbedResponse = response
            .json()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        Ok(parsed.embeddings.into_iter().map(|e| e.values).collect())
    }
}

#[async_trait]
impl LlmClient for GeminiClient {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let retry = self.retry.clone();
        match self
            .circuit
            .call(|| with_retry(&retry, || self.do_request(&req)))
            .await
        {
            Ok(r) => Ok(r),
            Err(CircuitError::Open(name)) => Err(anyhow::anyhow!("circuit breaker `{name}` is open")),
            Err(CircuitError::Inner(e)) => Err(anyhow::anyhow!("{e}")),
        }
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn provider(&self) -> &str {
        "gemini"
    }

    async fn stream<'a>(
        &'a self,
        req: ChatRequest,
    ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
        let retry = self.retry.clone();
        let resp = match self
            .circuit
            .call(|| with_retry(&retry, || self.open_stream(&req)))
            .await
        {
            Ok(r) => r,
            Err(CircuitError::Open(name)) => {
                return Err(anyhow::anyhow!("circuit breaker `{name}` is open"))
            }
            Err(CircuitError::Inner(e)) => return Err(anyhow::anyhow!("{e}")),
        };
        Ok(record_usage_tap(
            parse_gemini_sse(resp.bytes_stream()),
            self.rate_limiter.clone(),
        ))
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
            Ok(v) => {
                // Embeddings don't go through `do_request`'s inline
                // usage record, so bump the tracker manually. Gemini's
                // batchEmbedContents doesn't return token counts —
                // approximate as input token count from text lengths.
                if let Some(tracker) = self.rate_limiter.quota_tracker() {
                    let approx_tokens: u32 = texts
                        .iter()
                        .map(|t| (t.len() / 4).max(1) as u32)
                        .sum();
                    tracker.record_usage(approx_tokens, 0);
                }
                Ok(v)
            }
            Err(CircuitError::Open(name)) => {
                Err(anyhow::anyhow!("circuit breaker `{name}` is open"))
            }
            Err(CircuitError::Inner(e)) => Err(anyhow::anyhow!("{e}")),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GeminiBatchEmbedResponse {
    #[serde(default)]
    embeddings: Vec<GeminiEmbedding>,
}

#[derive(Debug, Deserialize)]
struct GeminiEmbedding {
    #[serde(default)]
    values: Vec<f32>,
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if headers.get("retry-after").is_some() {
        Some(parse_retry_after_ms(headers, "retry-after", 30_000))
    } else {
        None
    }
}

fn build_body(req: &ChatRequest) -> Value {
    let mut system_parts: Vec<String> = Vec::new();
    if let Some(s) = &req.system_prompt {
        system_parts.push(s.clone());
    }
    let mut contents: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            ChatRole::System => system_parts.push(m.content.clone()),
            ChatRole::User => {
                let mut parts: Vec<Value> = Vec::new();
                parts.push(json!({"text": m.content}));
                for att in &m.attachments {
                    if let Some(p) = gemini_media_part(att) {
                        parts.push(p);
                    }
                }
                contents.push(json!({"role":"user","parts": parts}));
            }
            ChatRole::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    parts.push(json!({"text": m.content}));
                }
                for tc in &m.tool_calls {
                    parts.push(json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": tc.arguments,
                        }
                    }));
                }
                if parts.is_empty() {
                    parts.push(json!({"text":""}));
                }
                contents.push(json!({"role":"model","parts": parts}));
            }
            ChatRole::Tool => {
                let tool_name = m.name.clone().unwrap_or_default();
                let response_value: Value = serde_json::from_str(&m.content)
                    .unwrap_or_else(|_| json!({"content": m.content}));
                contents.push(json!({
                    "role":"user",
                    "parts":[{
                        "functionResponse": {
                            "name": tool_name,
                            "response": response_value,
                        }
                    }]
                }));
            }
        }
    }

    let mut generation_config = json!({
        "maxOutputTokens": req.max_tokens,
        "temperature": req.temperature,
    });
    if !req.stop_sequences.is_empty() {
        generation_config["stopSequences"] = json!(req.stop_sequences);
    }

    let mut body = json!({
        "contents": contents,
        "generationConfig": generation_config,
    });
    if !system_parts.is_empty() {
        body["systemInstruction"] = json!({
            "parts": [{"text": system_parts.join("\n\n")}]
        });
    }
    if !req.tools.is_empty() {
        let declarations: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
        body["tools"] = json!([{"functionDeclarations": declarations}]);
        if let Some(tc) = gemini_tool_config(&req.tool_choice) {
            body["toolConfig"] = tc;
        }
    }
    body
}

fn validate_request(req: &ChatRequest) -> Result<(), LlmError> {
    if req.max_tokens == 0 {
        return Err(LlmError::Other(anyhow::anyhow!(
            "gemini: max_tokens must be > 0 (got 0)"
        )));
    }
    if req.messages.is_empty() && req.system_prompt.is_none() {
        return Err(LlmError::Other(anyhow::anyhow!(
            "gemini: contents cannot be empty when system_prompt is also missing"
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

fn gemini_tool_config(tc: &ToolChoice) -> Option<Value> {
    match tc {
        ToolChoice::Auto => None,
        ToolChoice::Any => Some(json!({
            "functionCallingConfig": { "mode": "ANY" }
        })),
        ToolChoice::None => Some(json!({
            "functionCallingConfig": { "mode": "NONE" }
        })),
        ToolChoice::Specific(name) => Some(json!({
            "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [name],
            }
        })),
    }
}

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(default, rename = "usageMetadata")]
    usage: Option<GeminiUsage>,
    /// Populated when the *prompt* (not the output) was rejected: the
    /// model returns no candidates and moves the reason here.
    #[serde(default, rename = "promptFeedback")]
    prompt_feedback: Option<GeminiPromptFeedback>,
}

#[derive(Debug, Deserialize)]
struct GeminiPromptFeedback {
    #[serde(default, rename = "blockReason")]
    block_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContent>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiContent {
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "functionCall")]
    function_call: Option<GeminiFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Deserialize)]
struct GeminiUsage {
    #[serde(default, rename = "promptTokenCount")]
    prompt: Option<u32>,
    #[serde(default, rename = "candidatesTokenCount")]
    output: Option<u32>,
}

fn to_chat_response(resp: GeminiResponse) -> ChatResponse {
    let usage = TokenUsage {
        prompt_tokens: resp.usage.as_ref().and_then(|u| u.prompt).unwrap_or(0),
        completion_tokens: resp.usage.as_ref().and_then(|u| u.output).unwrap_or(0),
    };

    // Prompt-block short-circuit: when Gemini rejects the input, the
    // response carries an empty `candidates` array and moves the reason
    // to `promptFeedback.blockReason`. Surface that as `FinishReason::
    // Other("BLOCKED:<reason>")` so callers can distinguish it from a
    // normal empty completion.
    if resp.candidates.is_empty() {
        let reason = resp
            .prompt_feedback
            .as_ref()
            .and_then(|pf| pf.block_reason.clone())
            .unwrap_or_else(|| "UNSPECIFIED".to_string());
        return ChatResponse {
            content: ResponseContent::Text(String::new()),
            usage,
            finish_reason: FinishReason::Other(format!("BLOCKED:{reason}")),
        };
    }

    let candidate = resp.candidates.into_iter().next();
    let finish = candidate.as_ref().and_then(|c| c.finish_reason.clone());

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(content) = candidate.and_then(|c| c.content) {
        for (idx, part) in content.parts.into_iter().enumerate() {
            if let Some(t) = part.text {
                text_parts.push(t);
            }
            if let Some(fc) = part.function_call {
                tool_calls.push(ToolCall {
                    id: format!("gemini_call_{idx}"),
                    name: fc.name,
                    arguments: fc.args,
                });
            }
        }
    }

    let finish_reason = match finish.as_deref() {
        Some("STOP") => {
            if !tool_calls.is_empty() {
                FinishReason::ToolUse
            } else {
                FinishReason::Stop
            }
        }
        Some("MAX_TOKENS") => FinishReason::Length,
        Some(other) => FinishReason::Other(other.to_string()),
        None => {
            if !tool_calls.is_empty() {
                FinishReason::ToolUse
            } else {
                FinishReason::Stop
            }
        }
    };

    let content = if !tool_calls.is_empty() {
        ResponseContent::ToolCalls(tool_calls)
    } else {
        ResponseContent::Text(text_parts.join(""))
    };
    ChatResponse {
        content,
        usage,
        finish_reason,
    }
}

fn gemini_media_part(att: &Attachment) -> Option<Value> {
    // Gemini accepts images, audio, video, PDFs via `inlineData` (base64)
    // or `fileData` (Files API / GCS URIs only).
    match &att.data {
        AttachmentData::Base64 { base64 } => Some(json!({
            "inlineData": { "mimeType": att.mime_type, "data": base64 }
        })),
        AttachmentData::Url { url } => {
            // `fileData.fileUri` is strict: only `files/<name>` from the
            // Gemini File API or `gs://bucket/path` from Cloud Storage
            // are accepted. Arbitrary https URLs return HTTP 400 — warn
            // and skip so the caller can fix (typically by downloading
            // the URL themselves and switching to `Attachment::
            // image_base64` / materialize()).
            let is_file_api = url.starts_with("files/");
            let is_gcs = url.starts_with("gs://");
            if !(is_file_api || is_gcs) {
                tracing::warn!(
                    url,
                    "gemini: fileUri accepts only `files/...` (File API) or `gs://...` (GCS). \
                     Download the URL and pass as Base64 (or call Attachment::materialize() \
                     on a Path variant) — skipping this attachment."
                );
                return None;
            }
            Some(json!({
                "fileData": { "mimeType": att.mime_type, "fileUri": url }
            }))
        }
        AttachmentData::Path { path } => {
            tracing::warn!(
                path,
                "gemini: Path attachment not materialized; skipping. \
                 Call Attachment::materialize() before sending the request."
            );
            None
        }
    }
}

pub struct GeminiFactory;

impl LlmProviderFactory for GeminiFactory {
    fn name(&self) -> &str {
        "gemini"
    }

    fn build(
        &self,
        provider_cfg: &LlmProviderConfig,
        model: &str,
        retry: RetryConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        Ok(Arc::new(GeminiClient::new(provider_cfg, model, retry)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Attachment, ChatMessage, ToolDef};

    fn req_with_tools() -> ChatRequest {
        ChatRequest {
            model: "gemini-2.0-flash".into(),
            messages: vec![ChatMessage::user("weather in Bogota?")],
            tools: vec![ToolDef {
                name: "get_weather".into(),
                description: "Look up weather".into(),
                parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
            }],
            max_tokens: 1024,
            temperature: 0.3,
            system_prompt: Some("be brief".into()),
            stop_sequences: vec!["END".into()],
            tool_choice: ToolChoice::Auto,
        }
    }

    #[test]
    fn body_includes_function_declarations_and_stops() {
        let body = build_body(&req_with_tools());
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "get_weather");
        assert_eq!(body["generationConfig"]["stopSequences"][0], "END");
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
    }

    #[test]
    fn assistant_tool_calls_become_function_call_parts() {
        let mut r = req_with_tools();
        r.messages.push(ChatMessage::assistant_tool_calls(
            vec![ToolCall {
                id: "gemini_call_0".into(),
                name: "get_weather".into(),
                arguments: json!({"city":"Bogota"}),
            }],
            "",
        ));
        r.messages.push(ChatMessage::tool_result(
            "gemini_call_0",
            "get_weather",
            "{\"temp\":22}",
        ));
        let body = build_body(&r);
        let contents = body["contents"].as_array().unwrap();
        let model_msg = contents.iter().find(|c| c["role"] == "model").unwrap();
        assert_eq!(model_msg["parts"][0]["functionCall"]["name"], "get_weather");
        let tool_msg = contents.last().unwrap();
        assert_eq!(tool_msg["parts"][0]["functionResponse"]["name"], "get_weather");
        assert_eq!(tool_msg["parts"][0]["functionResponse"]["response"]["temp"], 22);
    }

    #[test]
    fn parses_function_call_response() {
        let raw: GeminiResponse = serde_json::from_value(json!({
            "candidates":[{
                "content":{"parts":[{
                    "functionCall":{"name":"get_weather","args":{"city":"Bogota"}}
                }]},
                "finishReason":"STOP"
            }],
            "usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":3}
        })).unwrap();
        let resp = to_chat_response(raw);
        match resp.content {
            ResponseContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_weather");
                assert_eq!(calls[0].arguments["city"], "Bogota");
                assert!(calls[0].id.starts_with("gemini_call_"));
            }
            _ => panic!("expected ToolCalls"),
        }
        assert_eq!(resp.finish_reason, FinishReason::ToolUse);
    }

    #[test]
    fn user_attachment_becomes_inline_data_part() {
        let mut r = req_with_tools();
        r.messages = vec![ChatMessage {
            role: ChatRole::User,
            content: "describe".into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
            attachments: vec![Attachment::image_base64(
                "image/jpeg",
                "aGVsbG8=",
            )],
        }];
        let body = build_body(&r);
        let parts = &body["contents"][0]["parts"];
        assert_eq!(parts[0]["text"], "describe");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/jpeg");
        assert_eq!(parts[1]["inlineData"]["data"], "aGVsbG8=");
    }

    #[test]
    fn tool_choice_variants_serialize() {
        let mut r = req_with_tools();
        r.tool_choice = ToolChoice::Any;
        assert_eq!(
            build_body(&r)["toolConfig"]["functionCallingConfig"]["mode"],
            "ANY"
        );
        r.tool_choice = ToolChoice::None;
        assert_eq!(
            build_body(&r)["toolConfig"]["functionCallingConfig"]["mode"],
            "NONE"
        );
        r.tool_choice = ToolChoice::Specific("get_weather".into());
        let b = build_body(&r);
        assert_eq!(b["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            b["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "get_weather"
        );
        r.tool_choice = ToolChoice::Auto;
        assert!(build_body(&r).get("toolConfig").is_none());
    }

    #[test]
    fn filedata_accepts_files_api_uri() {
        let att = Attachment::image_url("image/jpeg", "files/abc123");
        let part = gemini_media_part(&att).unwrap();
        assert_eq!(part["fileData"]["fileUri"], "files/abc123");
    }

    #[test]
    fn filedata_accepts_gcs_uri() {
        let att = Attachment::image_url("image/jpeg", "gs://bucket/path.jpg");
        let part = gemini_media_part(&att).unwrap();
        assert_eq!(part["fileData"]["fileUri"], "gs://bucket/path.jpg");
    }

    #[test]
    fn filedata_rejects_arbitrary_https_url() {
        let att = Attachment::image_url("image/jpeg", "https://example.com/cat.jpg");
        assert!(
            gemini_media_part(&att).is_none(),
            "arbitrary URLs must be skipped (Gemini rejects them with 400)"
        );
    }

    #[test]
    fn path_variant_attachment_is_skipped() {
        let att = Attachment::image_path("image/jpeg", "/tmp/foo.jpg");
        assert!(gemini_media_part(&att).is_none());
    }

    #[test]
    fn validate_rejects_zero_max_tokens() {
        let mut r = req_with_tools();
        r.max_tokens = 0;
        let err = validate_request(&r).unwrap_err();
        assert!(
            format!("{err:?}").contains("max_tokens must be > 0"),
            "{err:?}"
        );
    }

    #[test]
    fn blocked_prompt_surfaces_block_reason() {
        // Gemini returns empty candidates + promptFeedback.blockReason
        // when it refuses the input (safety, recitation, etc).
        let raw: GeminiResponse = serde_json::from_value(json!({
            "candidates": [],
            "promptFeedback": { "blockReason": "SAFETY" },
            "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 0}
        })).unwrap();
        let resp = to_chat_response(raw);
        match resp.content {
            ResponseContent::Text(t) => assert!(t.is_empty()),
            _ => panic!("blocked prompt should be empty text"),
        }
        match resp.finish_reason {
            FinishReason::Other(s) => assert_eq!(s, "BLOCKED:SAFETY"),
            other => panic!("expected FinishReason::Other(BLOCKED:SAFETY), got {other:?}"),
        }
    }

    #[test]
    fn blocked_prompt_without_explicit_reason_uses_unspecified() {
        let raw: GeminiResponse = serde_json::from_value(json!({
            "candidates": [],
        })).unwrap();
        let resp = to_chat_response(raw);
        match resp.finish_reason {
            FinishReason::Other(s) => assert_eq!(s, "BLOCKED:UNSPECIFIED"),
            other => panic!("expected BLOCKED:UNSPECIFIED, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_empty_everything() {
        let r = ChatRequest {
            model: "gemini-2.0-flash".into(),
            messages: vec![],
            tools: vec![],
            max_tokens: 512,
            temperature: 0.5,
            system_prompt: None,
            stop_sequences: Vec::new(),
            tool_choice: ToolChoice::Auto,
        };
        let err = validate_request(&r).unwrap_err();
        assert!(format!("{err:?}").contains("contents cannot be empty"), "{err:?}");
    }
}
