//! MiniMax LLM client with two wire formats:
//!
//! * **`openai_compat`** (default) — POST
//!   `{base_url}/text/chatcompletion_v2` with OpenAI-shaped JSON. Matches
//!   the public MiniMax API docs for regular API keys.
//! * **`anthropic_messages`** — POST `{base_url}/v1/messages` with
//!   Anthropic Messages JSON. Required by the Coding / Token Plan keys
//!   served at `api.minimax.io/anthropic` (and its CN mirror). OpenClaw's
//!   `minimax` / `minimax-portal` plugins both use this path.
//!
//! Resilience (rate-limit, retry, circuit-breaker, auth refresh on 401)
//! is flavour-agnostic — the flavour only decides how we encode the
//! request and parse the response.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use agent_config::types::llm::{LlmProviderConfig, RetryConfig};
use agent_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitError};

use crate::client::LlmClient;
use crate::minimax_auth::{build_auth_source, AuthSource};
use crate::rate_limiter::RateLimiter;
use crate::retry::{parse_retry_after_ms, with_retry, LlmError};
use crate::stream::{
    ensure_event_stream, parse_anthropic_sse, parse_openai_sse, record_usage_tap,
    stream_metrics_tap, StreamChunk,
};
use crate::types::{
    Attachment, AttachmentData, ChatRequest, ChatResponse, ChatRole, FinishReason, ResponseContent,
    TokenUsage, ToolCall, ToolChoice,
};
use futures::stream::BoxStream;

/// Wire protocol the client should speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiFlavor {
    OpenAiCompat,
    AnthropicMessages,
}

impl ApiFlavor {
    fn from_cfg(cfg: &LlmProviderConfig) -> Self {
        // Explicit YAML wins.
        if let Some(flavor) = cfg.api_flavor.as_deref() {
            return match flavor {
                "anthropic_messages" | "anthropic-messages" | "anthropic" => {
                    Self::AnthropicMessages
                }
                _ => Self::OpenAiCompat,
            };
        }
        // Implicit: a base URL that ends in `/anthropic` (what
        // OpenClaw writes and what MiniMax's Token Plan endpoint
        // expects) auto-selects the Anthropic flavour.
        let trimmed = cfg.base_url.trim_end_matches('/');
        if trimmed.ends_with("/anthropic") {
            return Self::AnthropicMessages;
        }
        Self::OpenAiCompat
    }
}

pub struct MiniMaxClient {
    http: reqwest::Client,
    base_url: String,
    auth: AuthSource,
    group_id: Option<String>,
    model: String,
    flavor: ApiFlavor,
    rate_limiter: Arc<RateLimiter>,
    circuit: Arc<CircuitBreaker>,
    retry: RetryConfig,
}

impl MiniMaxClient {
    pub fn new(cfg: &LlmProviderConfig, model: impl Into<String>, retry: RetryConfig) -> Self {
        if cfg.api_key.trim().is_empty() && cfg.auth.is_none() {
            tracing::warn!(
                "minimax: both api_key and auth.bundle are empty — requests will fail with 401"
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

        let circuit = Arc::new(CircuitBreaker::new(
            "llm.minimax",
            CircuitBreakerConfig::default(),
        ));

        let auth = match build_auth_source(cfg) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "MiniMax auth source build failed — falling back to static api_key"
                );
                AuthSource::static_key(cfg.api_key.clone())
            }
        };

        let flavor = ApiFlavor::from_cfg(cfg);
        tracing::info!(?flavor, base_url = %cfg.base_url, "MiniMax client flavor");

        Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            auth,
            group_id: resolve_group_id(cfg),
            model: model.into(),
            flavor,
            rate_limiter,
            circuit,
            retry,
        }
    }

    fn request_url(&self) -> String {
        match self.flavor {
            ApiFlavor::OpenAiCompat => format!("{}/text/chatcompletion_v2", self.base_url),
            ApiFlavor::AnthropicMessages => format!("{}/v1/messages", self.base_url),
        }
    }

    fn build_body(&self, req: &ChatRequest) -> Value {
        match self.flavor {
            ApiFlavor::OpenAiCompat => build_openai_body(req),
            ApiFlavor::AnthropicMessages => build_anthropic_body(req),
        }
    }

    async fn do_request(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        validate_request(req)?;
        self.rate_limiter.acquire().await;

        let url = self.request_url();
        let body = self.build_body(req);

        // First attempt.
        let resp = self.post_once(&url, &body).await?;
        let status = resp.status().as_u16();

        // 401 path: Token Plan tokens can expire mid-flight; force
        // refresh and retry exactly once before giving up.
        if status == 401 {
            if let Err(e) = self.auth.force_refresh(&self.http).await {
                tracing::warn!(error = %e, "MiniMax forced refresh failed; surfacing 401");
            } else {
                let previous_body = resp.text().await.unwrap_or_default();
                tracing::info!(
                    previous_body = %previous_body,
                    "MiniMax 401 → refreshed token, retrying request"
                );
                let resp2 = self.post_once(&url, &body).await?;
                return self.finish(resp2).await;
            }
        }

        self.finish(resp).await
    }

    async fn post_once(&self, url: &str, body: &Value) -> Result<reqwest::Response, LlmError> {
        let token = self
            .auth
            .bearer(&self.http)
            .await
            .map_err(LlmError::Other)?;
        let mut request = self.http.post(url).bearer_auth(&token).json(body);
        if let Some(gid) = &self.group_id {
            request = request.header("X-MiniMax-Group-Id", gid);
        }
        if matches!(self.flavor, ApiFlavor::AnthropicMessages) {
            // Anthropic-compat endpoints require an API-version header;
            // MiniMax's mirror accepts either the classic Anthropic one
            // or no header, so we send it to be safe.
            request = request.header("anthropic-version", "2023-06-01");
        }
        request.send().await.map_err(|e| LlmError::Other(e.into()))
    }

    async fn do_stream_request(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, anyhow::Result<StreamChunk>>, LlmError> {
        validate_request(req)?;
        self.rate_limiter.acquire().await;

        let url = self.request_url();
        let mut body = self.build_body(req);
        body["stream"] = json!(true);
        if matches!(self.flavor, ApiFlavor::OpenAiCompat) {
            body["stream_options"] = json!({ "include_usage": true });
        }

        let resp = self.post_stream_once(&url, &body).await?;
        let status = resp.status().as_u16();

        // 401 path: force-refresh + retry once (mirrors `do_request`).
        let resp = if status == 401 {
            if let Err(e) = self.auth.force_refresh(&self.http).await {
                tracing::warn!(error = %e, "MiniMax stream forced refresh failed");
                resp
            } else {
                let body_txt = resp.text().await.unwrap_or_default();
                tracing::info!(
                    previous_body = %body_txt,
                    "MiniMax stream 401 → refreshed token, retrying"
                );
                self.post_stream_once(&url, &body).await?
            }
        } else {
            resp
        };

        let status = resp.status().as_u16();
        if status == 429 {
            let retry_after_ms = parse_retry_after_ms(resp.headers(), "retry-after", 30_000);
            return Err(LlmError::RateLimit { retry_after_ms });
        }
        if status >= 500 {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::ServerError { status, body });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Other(anyhow::anyhow!("HTTP {status}: {body}")));
        }
        let resp = ensure_event_stream(resp).map_err(LlmError::Other)?;

        let byte_stream = resp.bytes_stream();
        let parsed = match self.flavor {
            ApiFlavor::OpenAiCompat => parse_openai_sse(byte_stream),
            ApiFlavor::AnthropicMessages => parse_anthropic_sse(byte_stream),
        };
        Ok(parsed)
    }

    async fn post_stream_once(
        &self,
        url: &str,
        body: &Value,
    ) -> Result<reqwest::Response, LlmError> {
        let token = self
            .auth
            .bearer(&self.http)
            .await
            .map_err(LlmError::Other)?;
        let mut request = self
            .http
            .post(url)
            .bearer_auth(&token)
            .header("accept", "text/event-stream")
            .json(body);
        if let Some(gid) = &self.group_id {
            request = request.header("X-MiniMax-Group-Id", gid);
        }
        if matches!(self.flavor, ApiFlavor::AnthropicMessages) {
            request = request.header("anthropic-version", "2023-06-01");
        }
        request.send().await.map_err(|e| LlmError::Other(e.into()))
    }

    async fn finish(&self, response: reqwest::Response) -> Result<ChatResponse, LlmError> {
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
        // Read full body as text so parse errors surface what the server
        // actually said (e.g. an HTML 502 page from a proxy).
        let raw_text = response
            .text()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let resp = self.parse_body_text(&raw_text)?;
        if let Some(tracker) = self.rate_limiter.quota_tracker() {
            tracker.record_usage(resp.usage.prompt_tokens, resp.usage.completion_tokens);
        }
        Ok(resp)
    }

    fn parse_body_text(&self, raw_text: &str) -> Result<ChatResponse, LlmError> {
        match self.flavor {
            ApiFlavor::OpenAiCompat => {
                let raw: MiniMaxResponse = serde_json::from_str(raw_text).map_err(|e| {
                    LlmError::Other(anyhow::anyhow!(
                        "minimax(openai): response parse failed ({e}); body was: {}",
                        truncate_for_log(raw_text, 512)
                    ))
                })?;
                parse_openai_response(raw).map_err(LlmError::Other)
            }
            ApiFlavor::AnthropicMessages => {
                let raw: AnthropicResponse = serde_json::from_str(raw_text).map_err(|e| {
                    LlmError::Other(anyhow::anyhow!(
                        "minimax(anthropic): response parse failed ({e}); body was: {}",
                        truncate_for_log(raw_text, 512)
                    ))
                })?;
                parse_anthropic_response(raw).map_err(LlmError::Other)
            }
        }
    }
}

#[async_trait]
impl LlmClient for MiniMaxClient {
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
        "minimax"
    }

    async fn stream<'a>(
        &'a self,
        req: ChatRequest,
    ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
        let retry = self.retry.clone();
        match self
            .circuit
            .call(|| with_retry(&retry, || self.do_stream_request(&req)))
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
        // Only the OpenAI-compatible wire exposes an embeddings endpoint.
        // The Anthropic-flavour proxy at `api.minimax.io/anthropic` does
        // not serve /embeddings; fall back to the default trait error.
        if matches!(self.flavor, ApiFlavor::AnthropicMessages) {
            return Err(anyhow::anyhow!(
                "embed() not supported by MiniMax Anthropic-flavour endpoint"
            ));
        }
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.rate_limiter.acquire().await;

        let token = self
            .auth
            .bearer(&self.http)
            .await
            .map_err(|e| anyhow::anyhow!("minimax auth failed: {e}"))?;
        let url = format!("{}/embeddings", self.base_url);
        let body = json!({
            "model": &self.model,
            "input": texts,
        });
        let mut req = self.http.post(&url).bearer_auth(&token).json(&body);
        if let Some(gid) = &self.group_id {
            req = req.header("X-MiniMax-Group-Id", gid);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("embed HTTP {status}: {body}"));
        }
        let parsed: OpenAiEmbedResponse = resp.json().await?;
        // OpenAI returns entries in request order; sort by `index` to be safe.
        let mut entries = parsed.data;
        entries.sort_by_key(|e| e.index);
        Ok(entries.into_iter().map(|e| e.embedding).collect())
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedResponse {
    data: Vec<OpenAiEmbedEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedEntry {
    embedding: Vec<f32>,
    #[serde(default)]
    index: u32,
}

fn validate_request(req: &ChatRequest) -> Result<(), LlmError> {
    if req.max_tokens == 0 {
        return Err(LlmError::Other(anyhow::anyhow!(
            "minimax: max_tokens must be > 0 (got 0)"
        )));
    }
    if req.messages.is_empty() && req.system_prompt.is_none() {
        return Err(LlmError::Other(anyhow::anyhow!(
            "minimax: messages cannot be empty when system_prompt is also missing"
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

/// Resolve `X-MiniMax-Group-Id` header value with a fallback chain
/// (matches the `resolve_static_key` layout in `minimax_auth.rs`):
/// `MINIMAX_GROUP_ID` env → `secrets/minimax_group_id.txt` on disk →
/// `cfg.group_id` from YAML. Returns `None` when every source is
/// empty so we don't send a blank header.
fn resolve_group_id(cfg: &LlmProviderConfig) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(v) = std::env::var("MINIMAX_GROUP_ID") {
        candidates.push(v);
    }
    if let Ok(v) = std::fs::read_to_string("./secrets/minimax_group_id.txt") {
        candidates.push(v);
    }
    if let Some(v) = cfg.group_id.clone() {
        candidates.push(v);
    }
    candidates
        .into_iter()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

// ── OpenAI-compat wire ────────────────────────────────────────────────────────

fn build_openai_body(req: &ChatRequest) -> Value {
    let messages: Vec<Value> = build_openai_messages(req);
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

fn build_openai_messages(req: &ChatRequest) -> Vec<Value> {
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
        // User turns with image attachments must use the multipart
        // content array (`[{type:"text",...}, {type:"image_url",...}]`)
        // so OpenAI-compatible vision models can see them. Non-user or
        // attachment-free turns keep the plain-string shape.
        let content: Value = if matches!(msg.role, ChatRole::User) && !msg.attachments.is_empty() {
            let mut parts: Vec<Value> = Vec::new();
            if !msg.content.is_empty() {
                parts.push(json!({"type":"text","text": msg.content}));
            }
            for att in &msg.attachments {
                if let Some(p) = openai_image_part(att) {
                    parts.push(p);
                }
            }
            Value::Array(parts)
        } else {
            Value::String(msg.content.clone())
        };
        let mut m = json!({ "role": role, "content": content });
        if let Some(id) = &msg.tool_call_id {
            m["tool_call_id"] = json!(id);
        }
        if let Some(name) = &msg.name {
            m["name"] = json!(name);
        }
        messages.push(m);
    }
    messages
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
        AttachmentData::Path { .. } => return None,
    };
    Some(json!({
        "type": "image_url",
        "image_url": { "url": url }
    }))
}

#[derive(Deserialize)]
struct MiniMaxResponse {
    choices: Vec<MiniMaxChoice>,
    #[serde(default)]
    usage: MiniMaxUsage,
}

#[derive(Deserialize)]
struct MiniMaxChoice {
    message: MiniMaxMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct MiniMaxMessage {
    content: Option<MiniMaxContent>,
    #[serde(default)]
    tool_calls: Vec<MiniMaxToolCall>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MiniMaxContent {
    Text(String),
    Parts(Vec<MiniMaxContentPart>),
}

#[derive(Deserialize)]
struct MiniMaxContentPart {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct MiniMaxToolCall {
    id: String,
    function: MiniMaxFunction,
}

#[derive(Deserialize)]
struct MiniMaxFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize, Default)]
struct MiniMaxUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

fn parse_openai_response(raw: MiniMaxResponse) -> anyhow::Result<ChatResponse> {
    let choice = raw
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("MiniMax returned no choices"))?;

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
        });
    }

    let text = match choice.message.content {
        Some(MiniMaxContent::Text(t)) => t,
        Some(MiniMaxContent::Parts(parts)) => parts
            .into_iter()
            .filter(|p| p.kind == "text")
            .filter_map(|p| p.text)
            .collect::<Vec<_>>()
            .join(""),
        None => String::new(),
    };

    Ok(ChatResponse {
        content: ResponseContent::Text(text),
        usage,
        finish_reason,
    })
}

// ── Anthropic Messages wire ───────────────────────────────────────────────────

fn build_anthropic_body(req: &ChatRequest) -> Value {
    // Anthropic splits system out to a top-level field. Turn history +
    // our tool-result messages translate into content blocks.
    let mut messages: Vec<Value> = Vec::new();
    for msg in &req.messages {
        let role = match msg.role {
            ChatRole::Assistant => "assistant",
            // `tool` and `system` don't exist in Anthropic; we flatten
            // both into `user` messages with tool_result blocks / plain
            // text. Bona-fide system prompts go on the top-level
            // `system` field (set below from `req.system_prompt`).
            ChatRole::Tool => "user",
            ChatRole::System => continue,
            _ => "user",
        };
        let content = if matches!(msg.role, ChatRole::Tool) {
            json!([{
                "type": "tool_result",
                "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                "content": msg.content,
            }])
        } else if matches!(msg.role, ChatRole::Assistant) && !msg.tool_calls.is_empty() {
            // Assistant turn that requested tools — emit `tool_use`
            // blocks so the subsequent `tool_result` messages can
            // correlate via `tool_use_id`. Preamble text (rare but
            // possible) sits in front as its own text block.
            let mut blocks: Vec<Value> = Vec::new();
            if !msg.content.is_empty() {
                blocks.push(json!({ "type": "text", "text": msg.content }));
            }
            for tc in &msg.tool_calls {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": tc.arguments,
                }));
            }
            json!(blocks)
        } else {
            // User / (fallback) turns: emit a text block, plus any image
            // attachments so MiniMax Anthropic-flavour vision models can
            // see them. Non-image kinds are skipped — they flow through
            // dedicated skills instead of riding on the LLM wire.
            let mut blocks: Vec<Value> = Vec::new();
            blocks.push(json!({ "type": "text", "text": msg.content }));
            if matches!(msg.role, ChatRole::User) {
                for att in &msg.attachments {
                    if let Some(b) = anthropic_image_block(att) {
                        blocks.push(b);
                    }
                }
            }
            json!(blocks)
        };
        messages.push(json!({ "role": role, "content": content }));
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    });
    if let Some(system) = &req.system_prompt {
        body["system"] = json!(system);
    }
    if !req.stop_sequences.is_empty() {
        body["stop_sequences"] = json!(req.stop_sequences);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        body["tools"] = json!(tools);
        if let Some(tc) = anthropic_tool_choice_value(&req.tool_choice) {
            body["tool_choice"] = tc;
        }
    }
    body
}

fn anthropic_tool_choice_value(tc: &ToolChoice) -> Option<Value> {
    match tc {
        ToolChoice::Auto => None,
        ToolChoice::Any => Some(json!({"type":"any"})),
        ToolChoice::None => Some(json!({"type":"none"})),
        ToolChoice::Specific(name) => Some(json!({"type":"tool","name": name})),
    }
}

fn anthropic_image_block(att: &Attachment) -> Option<Value> {
    if att.kind != "image" {
        return None;
    }
    let source = match &att.data {
        AttachmentData::Base64 { base64 } => json!({
            "type": "base64",
            "media_type": att.mime_type,
            "data": base64,
        }),
        AttachmentData::Url { url } => json!({
            "type": "url",
            "url": url,
        }),
        AttachmentData::Path { .. } => return None,
    };
    Some(json!({ "type": "image", "source": source }))
}

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

fn parse_anthropic_response(raw: AnthropicResponse) -> anyhow::Result<ChatResponse> {
    let usage = TokenUsage {
        prompt_tokens: raw.usage.input_tokens,
        completion_tokens: raw.usage.output_tokens,
    };

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for block in raw.content {
        match block {
            AnthropicContentBlock::Text { text } => text_parts.push(text),
            AnthropicContentBlock::ToolUse { id, name, input } => tool_calls.push(ToolCall {
                id,
                name,
                arguments: input,
            }),
            AnthropicContentBlock::Unknown => {}
        }
    }

    let finish_reason = match raw.stop_reason.as_deref() {
        Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
        Some("tool_use") => FinishReason::ToolUse,
        Some("max_tokens") => FinishReason::Length,
        Some(other) => FinishReason::Other(other.to_string()),
        None => FinishReason::Stop,
    };

    if !tool_calls.is_empty() {
        return Ok(ChatResponse {
            content: ResponseContent::ToolCalls(tool_calls),
            usage,
            finish_reason: FinishReason::ToolUse,
        });
    }

    Ok(ChatResponse {
        content: ResponseContent::Text(text_parts.join("")),
        usage,
        finish_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_cfg(flavor: Option<&str>, base_url: &str) -> LlmProviderConfig {
        LlmProviderConfig {
            api_key: "k".into(),
            group_id: None,
            base_url: base_url.into(),
            rate_limit: Default::default(),
            auth: None,
            api_flavor: flavor.map(str::to_string),
            embedding_model: None,
            safety_settings: None,
        }
    }

    #[test]
    fn flavor_explicit_wins() {
        let cfg = provider_cfg(Some("anthropic_messages"), "https://api.minimax.chat/v1");
        assert_eq!(ApiFlavor::from_cfg(&cfg), ApiFlavor::AnthropicMessages);
    }

    #[test]
    fn flavor_auto_from_anthropic_suffix() {
        let cfg = provider_cfg(None, "https://api.minimax.io/anthropic");
        assert_eq!(ApiFlavor::from_cfg(&cfg), ApiFlavor::AnthropicMessages);
    }

    #[test]
    fn flavor_default_is_openai() {
        let cfg = provider_cfg(None, "https://api.minimax.io/v1");
        assert_eq!(ApiFlavor::from_cfg(&cfg), ApiFlavor::OpenAiCompat);
    }

    #[test]
    fn anthropic_body_lifts_system_to_top_level() {
        let req = ChatRequest {
            model: "MiniMax-M2.7".into(),
            system_prompt: Some("be helpful".into()),
            messages: vec![crate::types::ChatMessage {
                role: ChatRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
                attachments: Vec::new(),
            }],
            tools: vec![],
            max_tokens: 32,
            temperature: 0.7,
            stop_sequences: Vec::new(),
            tool_choice: Default::default(),
        };
        let body = build_anthropic_body(&req);
        assert_eq!(body["system"], "be helpful");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
    }

    #[test]
    fn anthropic_tool_schema_uses_input_schema_key() {
        let req = ChatRequest {
            model: "MiniMax-M2.7".into(),
            system_prompt: None,
            messages: vec![],
            tools: vec![crate::types::ToolDef {
                name: "weather".into(),
                description: "...".into(),
                parameters: json!({"type":"object"}),
            }],
            max_tokens: 1,
            temperature: 0.0,
            stop_sequences: Vec::new(),
            tool_choice: Default::default(),
        };
        let body = build_anthropic_body(&req);
        assert_eq!(body["tools"][0]["name"], "weather");
        assert!(body["tools"][0]["input_schema"].is_object());
        // Must not use the OpenAI `function` wrapper:
        assert!(body["tools"][0].get("function").is_none());
    }

    #[test]
    fn openai_tool_choice_variants() {
        assert_eq!(openai_tool_choice(&ToolChoice::Auto), json!("auto"));
        assert_eq!(openai_tool_choice(&ToolChoice::Any), json!("required"));
        assert_eq!(openai_tool_choice(&ToolChoice::None), json!("none"));
        let s = openai_tool_choice(&ToolChoice::Specific("weather".into()));
        assert_eq!(s["type"], "function");
        assert_eq!(s["function"]["name"], "weather");
    }

    #[test]
    fn openai_user_image_becomes_content_array() {
        let req = ChatRequest {
            model: "MiniMax-M2.7".into(),
            system_prompt: None,
            messages: vec![crate::types::ChatMessage {
                role: ChatRole::User,
                content: "describe".into(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
                attachments: vec![crate::types::Attachment::image_base64(
                    "image/png",
                    "aGVsbG8=",
                )],
            }],
            tools: vec![],
            max_tokens: 1,
            temperature: 0.0,
            stop_sequences: Vec::new(),
            tool_choice: Default::default(),
        };
        let body = build_openai_body(&req);
        let content = &body["messages"][0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert!(content[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn anthropic_user_image_becomes_image_block() {
        let req = ChatRequest {
            model: "MiniMax-M2.7".into(),
            system_prompt: None,
            messages: vec![crate::types::ChatMessage {
                role: ChatRole::User,
                content: "describe".into(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
                attachments: vec![crate::types::Attachment::image_base64(
                    "image/jpeg",
                    "aGVsbG8=",
                )],
            }],
            tools: vec![],
            max_tokens: 1,
            temperature: 0.0,
            stop_sequences: Vec::new(),
            tool_choice: Default::default(),
        };
        let body = build_anthropic_body(&req);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
    }

    #[test]
    fn parses_anthropic_text_response() {
        let resp = serde_json::from_value::<AnthropicResponse>(json!({
            "content": [{"type":"text","text":"hi there"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        }))
        .unwrap();
        let parsed = parse_anthropic_response(resp).unwrap();
        match parsed.content {
            ResponseContent::Text(t) => assert_eq!(t, "hi there"),
            _ => panic!("expected text"),
        }
        assert_eq!(parsed.usage.prompt_tokens, 5);
        assert_eq!(parsed.usage.completion_tokens, 3);
        assert!(matches!(parsed.finish_reason, FinishReason::Stop));
    }

    #[test]
    fn parses_anthropic_tool_use_response() {
        let resp = serde_json::from_value::<AnthropicResponse>(json!({
            "content": [
                {"type":"text","text":"let me check"},
                {"type":"tool_use","id":"tu_1","name":"weather","input":{"city":"bogota"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 7, "output_tokens": 4}
        }))
        .unwrap();
        let parsed = parse_anthropic_response(resp).unwrap();
        match parsed.content {
            ResponseContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "weather");
                assert_eq!(calls[0].arguments["city"], "bogota");
            }
            _ => panic!("expected tool calls"),
        }
        assert!(matches!(parsed.finish_reason, FinishReason::ToolUse));
    }
}
