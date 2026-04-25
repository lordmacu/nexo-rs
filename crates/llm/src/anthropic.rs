//! Anthropic Messages API client (Claude).
//!
//! Supports native tool calling (tool_use / tool_result blocks), stop
//! sequences, and assistant tool-call history preservation. Uses
//! `x-api-key` + `anthropic-version` headers; API key never appears in
//! URLs or logs.

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{json, Value};

use agent_config::types::llm::{LlmProviderConfig, RetryConfig};
use agent_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitError};

use crate::anthropic_auth::{
    validate_setup_token, AnthropicAuth, OAuthBundle, OAuthState, DEFAULT_CLIENT_ID,
    DEFAULT_REFRESH_ENDPOINT,
};
use crate::client::LlmClient;
use crate::rate_limiter::RateLimiter;
use crate::registry::LlmProviderFactory;
use crate::retry::{parse_retry_after_ms, with_retry, LlmError};
use crate::stream::{
    ensure_event_stream, parse_anthropic_sse, record_usage_tap, stream_metrics_tap, StreamChunk,
};
use crate::types::{
    Attachment, AttachmentData, ChatRequest, ChatResponse, ChatRole, FinishReason, ResponseContent,
    TokenUsage, ToolCall, ToolChoice,
};

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const DEFAULT_API_VERSION: &str = "2023-06-01";

/// Resolve the Anthropic API version header. `ANTHROPIC_VERSION` env
/// overrides the hardcoded default so deployments can opt into newer
/// features (PDF vision, prompt-caching breakpoints, extended thinking)
/// without a code change.
fn api_version() -> String {
    std::env::var("ANTHROPIC_VERSION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API_VERSION.to_string())
}

pub struct AnthropicClient {
    http: reqwest::Client,
    base_url: String,
    auth: AnthropicAuth,
    api_version: String,
    model: String,
    rate_limiter: Arc<RateLimiter>,
    circuit: Arc<CircuitBreaker>,
    retry: RetryConfig,
}

impl AnthropicClient {
    /// Construct a client, resolving the auth mode from `cfg.auth`.
    /// Falls back to the legacy `cfg.api_key` path when `auth` is
    /// absent or `mode = api_key`. Returns `Err` only for explicit
    /// modes whose preconditions cannot be satisfied (e.g.
    /// `oauth_bundle` with a missing bundle file).
    pub fn new(
        cfg: &LlmProviderConfig,
        model: impl Into<String>,
        retry: RetryConfig,
    ) -> anyhow::Result<Self> {
        let auth = resolve_auth(cfg)?;
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
            "llm.anthropic",
            CircuitBreakerConfig::default(),
        ));
        Ok(Self {
            http,
            base_url: base,
            auth,
            api_version: api_version(),
            model: model.into(),
            rate_limiter,
            circuit,
            retry,
        })
    }

    /// Classify an HTTP response into our error taxonomy. Shared between
    /// chat and streaming so both paths retry the same way.
    async fn classify_response(
        &self,
        response: reqwest::Response,
    ) -> Result<reqwest::Response, LlmError> {
        let status = response.status().as_u16();
        if status == 429 {
            let retry_after_ms = parse_retry_after(response.headers()).unwrap_or(60_000);
            return Err(LlmError::RateLimit { retry_after_ms });
        }
        if status >= 500 {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::ServerError { status, body });
        }
        if status == 401 || status == 403 {
            let body = response.text().await.unwrap_or_default();
            // OAuth/setup-token: mark stale so the next request tries
            // a fresh refresh. Static API keys simply fail — user
            // needs to fix them in `secrets/` + re-run setup.
            self.auth.mark_stale();
            let hint = if self.auth.is_subscription() {
                format!(
                    "HTTP {status} from Anthropic; run `agent setup anthropic` to re-authenticate. Body: {}",
                    truncate_for_log(&body, 200)
                )
            } else {
                format!(
                    "HTTP {status} from Anthropic; check ANTHROPIC_API_KEY or `agent setup anthropic`. Body: {}",
                    truncate_for_log(&body, 200)
                )
            };
            return Err(LlmError::CredentialInvalid { hint });
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
        let url = format!("{}/v1/messages", self.base_url);
        let body = build_body(&self.model, req);
        let headers = self
            .auth
            .resolve_headers(&self.http)
            .await
            .map_err(LlmError::Other)?;

        let mut builder = self
            .http
            .post(&url)
            .header(headers.auth.0, headers.auth.1.as_str())
            .header("anthropic-version", &self.api_version)
            .header("content-type", "application/json");
        if let Some(beta) = headers.beta {
            builder = builder.header("anthropic-beta", beta);
        }
        let response = builder
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;

        let response = self.classify_response(response).await?;
        // Read the raw body once so parse errors can include it — a
        // bare `response.json()` hides the actual payload, which makes
        // outages / schema drift impossible to debug from logs.
        let raw_text = response
            .text()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        let raw: AnthropicResponse = serde_json::from_str(&raw_text).map_err(|e| {
            LlmError::Other(anyhow::anyhow!(
                "anthropic: response parse failed ({e}); body was: {}",
                truncate_for_log(&raw_text, 512)
            ))
        })?;
        let resp = to_chat_response(raw);
        if let Some(tracker) = self.rate_limiter.quota_tracker() {
            tracker.record_usage(resp.usage.prompt_tokens, resp.usage.completion_tokens);
        }
        Ok(resp)
    }

    /// Streaming setup: send the POST and return the live `Response` once
    /// the headers are back. Wrapped in retry + circuit breaker by the
    /// `stream()` entrypoint — retries happen before any SSE bytes land,
    /// so partially-consumed streams can't produce duplicate output.
    async fn open_stream(&self, req: &ChatRequest) -> Result<reqwest::Response, LlmError> {
        validate_request(req)?;
        self.rate_limiter.acquire().await;
        let url = format!("{}/v1/messages", self.base_url);
        let mut body = build_body(&self.model, req);
        body["stream"] = json!(true);

        let headers = self
            .auth
            .resolve_headers(&self.http)
            .await
            .map_err(LlmError::Other)?;
        let mut builder = self
            .http
            .post(&url)
            .header(headers.auth.0, headers.auth.1.as_str())
            .header("anthropic-version", &self.api_version)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        if let Some(beta) = headers.beta {
            builder = builder.header("anthropic-beta", beta);
        }
        let response = builder
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(e.into()))?;
        self.classify_response(response).await
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let retry = self.retry.clone();
        match self
            .circuit
            .call(|| with_retry(&retry, || self.do_request(&req)))
            .await
        {
            Ok(r) => Ok(r),
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
        "anthropic"
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
        let resp = ensure_event_stream(resp)?;
        // Quota tracker: the non-streaming path records usage inline;
        // streaming pipes it through a tap that fires on the final Usage
        // chunk so threshold alerts still trigger.
        Ok(stream_metrics_tap(
            record_usage_tap(
                parse_anthropic_sse(resp.bytes_stream()),
                self.rate_limiter.clone(),
            ),
            self.provider(),
        ))
    }
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    // Anthropic may send either `retry-after` (seconds or HTTP-date) or
    // the provider-specific `anthropic-ratelimit-*-reset` header. Prefer
    // the generic one; fall back to the ratelimit reset when absent.
    if headers.get("retry-after").is_some() {
        return Some(parse_retry_after_ms(headers, "retry-after", 60_000));
    }
    headers
        .get("anthropic-ratelimit-requests-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| s.saturating_mul(1000))
}

fn build_body(model: &str, req: &ChatRequest) -> Value {
    let mut system_parts: Vec<String> = Vec::new();
    if let Some(s) = &req.system_prompt {
        system_parts.push(s.clone());
    }
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            ChatRole::System => system_parts.push(m.content.clone()),
            ChatRole::User => {
                let mut blocks: Vec<Value> = Vec::new();
                // Anthropic rejects empty-string text blocks in some
                // positions, so omit them entirely when the user turn
                // carries only media (e.g. a photo without caption).
                if !m.content.is_empty() {
                    blocks.push(json!({"type":"text","text": m.content}));
                }
                for att in &m.attachments {
                    if let Some(b) = anthropic_image_block(att) {
                        blocks.push(b);
                    }
                }
                if blocks.is_empty() {
                    // Every attachment was skipped and text was empty —
                    // still need at least one block per Anthropic spec.
                    blocks.push(json!({"type":"text","text":"(no content)"}));
                }
                messages.push(json!({"role":"user","content": blocks}));
            }
            ChatRole::Assistant => {
                // Preserve prior tool_use blocks so the follow-up
                // tool_result messages correlate by id.
                let mut blocks: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    blocks.push(json!({"type":"text","text": m.content}));
                }
                for tc in &m.tool_calls {
                    blocks.push(json!({
                        "type":"tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
                if blocks.is_empty() {
                    blocks.push(json!({"type":"text","text":""}));
                }
                messages.push(json!({"role":"assistant","content": blocks}));
            }
            ChatRole::Tool => messages.push(json!({
                "role":"user",
                "content":[{
                    "type":"tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                }]
            })),
        }
    }
    let mut body = json!({
        "model": model,
        "max_tokens": req.max_tokens,
        "messages": messages,
        "temperature": req.temperature,
    });
    if !system_parts.is_empty() {
        body["system"] = Value::String(system_parts.join("\n\n"));
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
        if let Some(tc) = anthropic_tool_choice(&req.tool_choice) {
            body["tool_choice"] = tc;
        }
    }
    body
}

/// Fail fast on obvious request-shape problems so the caller sees a clear
/// message instead of the API returning HTTP 400 with a minimal body.
/// Doesn't try to cover every edge — just the ones with low-quality
/// upstream error messages.
fn validate_request(req: &ChatRequest) -> Result<(), LlmError> {
    if req.max_tokens == 0 {
        return Err(LlmError::Other(anyhow::anyhow!(
            "anthropic: max_tokens must be > 0 (got 0)"
        )));
    }
    if req.messages.is_empty() && req.system_prompt.is_none() {
        return Err(LlmError::Other(anyhow::anyhow!(
            "anthropic: messages cannot be empty when system_prompt is also missing"
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

fn anthropic_tool_choice(tc: &ToolChoice) -> Option<Value> {
    match tc {
        ToolChoice::Auto => None,
        ToolChoice::Any => Some(json!({"type":"any"})),
        ToolChoice::None => Some(json!({"type":"none"})),
        ToolChoice::Specific(name) => Some(json!({"type":"tool","name": name})),
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicBlock {
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

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
    /// Prompt-caching: tokens we paid to WRITE into the cache on this
    /// turn. Counted toward prompt spend (same price tier as regular
    /// input in Anthropic's billing today).
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    /// Prompt-caching: tokens we READ from a previously cached prefix.
    /// Billed at ~10% of regular input but we fold them into the prompt
    /// tally so quota thresholds stay conservative.
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

fn to_chat_response(resp: AnthropicResponse) -> ChatResponse {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for block in resp.content {
        match block {
            AnthropicBlock::Text { text } => text_parts.push(text),
            AnthropicBlock::ToolUse { id, name, input } => tool_calls.push(ToolCall {
                id,
                name,
                arguments: input,
            }),
            AnthropicBlock::Unknown => {}
        }
    }
    let finish_reason = match resp.stop_reason.as_deref() {
        Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolUse,
        Some(other) => FinishReason::Other(other.to_string()),
        None => FinishReason::Stop,
    };
    let usage = TokenUsage {
        // Fold prompt-cache read/write tokens into the prompt tally so
        // quota tracking stays conservative even when the caller opts
        // into caching. Anthropic reports them as separate fields.
        prompt_tokens: {
            let u = resp.usage.as_ref();
            u.and_then(|u| u.input_tokens).unwrap_or(0)
                + u.and_then(|u| u.cache_creation_input_tokens).unwrap_or(0)
                + u.and_then(|u| u.cache_read_input_tokens).unwrap_or(0)
        },
        completion_tokens: resp
            .usage
            .as_ref()
            .and_then(|u| u.output_tokens)
            .unwrap_or(0),
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
        cache_usage: None,
    }
}

fn anthropic_image_block(att: &Attachment) -> Option<Value> {
    if att.kind != "image" {
        tracing::debug!(
            kind = %att.kind,
            "anthropic: non-image attachment skipped (only image supported on the messages wire)"
        );
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
        AttachmentData::Path { path } => {
            tracing::warn!(
                path,
                "anthropic: Path attachment not materialized; skipping. \
                 Call Attachment::materialize() before sending the request."
            );
            return None;
        }
    };
    Some(json!({"type":"image","source": source}))
}

pub struct AnthropicFactory;

impl LlmProviderFactory for AnthropicFactory {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn build(
        &self,
        provider_cfg: &LlmProviderConfig,
        model: &str,
        retry: RetryConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        Ok(Arc::new(AnthropicClient::new(provider_cfg, model, retry)?))
    }
}

/// Resolve an [`AnthropicAuth`] for the given provider config.
///
/// Modes:
/// * `api_key` (default when `auth` missing) — uses `cfg.api_key`.
/// * `setup_token` — reads `setup_token_file` (or falls back to
///   `cfg.api_key` if the file is unset), validates prefix+length.
/// * `oauth_bundle` — loads the bundle from `auth.bundle` and
///   constructs a refreshing [`OAuthState`].
/// * `cli_import` — reads `~/.claude/.credentials.json` (resolved at
///   startup), copies the bundle to `auth.bundle` for subsequent
///   runs, then behaves like `oauth_bundle`.
/// * `auto` — tries in order: oauth_bundle (if file exists) →
///   cli_import (if available) → setup_token (if file exists) →
///   api_key.
fn resolve_auth(cfg: &LlmProviderConfig) -> anyhow::Result<AnthropicAuth> {
    let auth = cfg.auth.as_ref();
    let mode = auth.map(|a| a.mode.as_str()).unwrap_or("api_key");
    let bundle_path = auth
        .and_then(|a| a.bundle.as_ref())
        .map(std::path::PathBuf::from);
    let setup_token_file = auth.and_then(|a| a.setup_token_file.as_ref());
    let refresh_endpoint = auth
        .and_then(|a| a.refresh_endpoint.clone())
        .unwrap_or_else(|| DEFAULT_REFRESH_ENDPOINT.to_string());
    let client_id = auth
        .and_then(|a| a.client_id.clone())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());

    match mode {
        "api_key" | "static" => Ok(AnthropicAuth::api_key(trim_or_warn(cfg.api_key.clone()))),
        "setup_token" => {
            let raw = read_setup_token(setup_token_file, cfg)?;
            let validated = validate_setup_token(&raw)?;
            Ok(AnthropicAuth::setup_token(validated))
        }
        "oauth_bundle" => {
            let path = bundle_path.ok_or_else(|| {
                anyhow::anyhow!("anthropic auth.mode=oauth_bundle requires auth.bundle path")
            })?;
            let bundle = OAuthBundle::load(&path)?;
            let state = OAuthState::new(bundle, path, refresh_endpoint, client_id);
            Ok(AnthropicAuth::oauth(Arc::new(state)))
        }
        "cli_import" => {
            let bundle = crate::anthropic_auth::read_claude_cli_credentials().ok_or_else(|| {
                anyhow::anyhow!(
                    "anthropic auth.mode=cli_import: no Claude Code credentials found. \
                     Run `claude login` (or `agent setup anthropic` to paste manually)."
                )
            })?;
            let path = bundle_path
                .unwrap_or_else(|| std::path::PathBuf::from("./secrets/anthropic_oauth.json"));
            // Snapshot into our own bundle so subsequent starts don't
            // depend on the CLI file shape staying stable.
            if let Err(e) = bundle.save_atomic(&path) {
                tracing::warn!(error = %e, path = %path.display(), "persist CLI-imported bundle");
            }
            let state = OAuthState::new(bundle, path, refresh_endpoint, client_id);
            Ok(AnthropicAuth::oauth(Arc::new(state)))
        }
        "auto" => {
            // oauth_bundle first
            if let Some(path) = bundle_path.clone() {
                if path.is_file() {
                    if let Ok(bundle) = OAuthBundle::load(&path) {
                        let state = OAuthState::new(
                            bundle,
                            path,
                            refresh_endpoint.clone(),
                            client_id.clone(),
                        );
                        return Ok(AnthropicAuth::oauth(Arc::new(state)));
                    }
                }
            }
            // cli_import
            if let Some(bundle) = crate::anthropic_auth::read_claude_cli_credentials() {
                let path = bundle_path
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from("./secrets/anthropic_oauth.json"));
                let _ = bundle.save_atomic(&path);
                let state = OAuthState::new(bundle, path, refresh_endpoint, client_id);
                return Ok(AnthropicAuth::oauth(Arc::new(state)));
            }
            // setup_token file
            if let Some(f) = setup_token_file {
                if std::path::Path::new(f).is_file() {
                    if let Ok(raw) = std::fs::read_to_string(f) {
                        if let Ok(validated) = validate_setup_token(&raw) {
                            return Ok(AnthropicAuth::setup_token(validated));
                        }
                    }
                }
            }
            // Fallback api_key
            if !cfg.api_key.trim().is_empty() {
                return Ok(AnthropicAuth::api_key(cfg.api_key.trim().to_string()));
            }
            anyhow::bail!(
                "anthropic auth.mode=auto: no bundle, no Claude CLI credentials, no setup-token, no api_key"
            )
        }
        other => anyhow::bail!("anthropic: unknown auth.mode `{other}`"),
    }
}

fn read_setup_token(file: Option<&String>, cfg: &LlmProviderConfig) -> anyhow::Result<String> {
    if let Some(path) = file {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read setup-token file {path}"))?;
        return Ok(text);
    }
    if !cfg.api_key.trim().is_empty() {
        // Allow pasting the setup-token directly into `api_key` for
        // Docker/ENV-driven deployments.
        return Ok(cfg.api_key.clone());
    }
    anyhow::bail!(
        "anthropic auth.mode=setup_token requires either auth.setup_token_file or a non-empty api_key"
    )
}

fn trim_or_warn(key: String) -> String {
    if key.trim().is_empty() {
        tracing::warn!(
            "anthropic: api_key is empty — requests will fail with 401 until a valid key is set"
        );
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Attachment, ChatMessage, ToolDef};

    fn req_with_tools() -> ChatRequest {
        ChatRequest {
            model: "claude-sonnet-4".into(),
            messages: vec![ChatMessage::user("what's the weather?")],
            tools: vec![ToolDef {
                name: "get_weather".into(),
                description: "Look up weather".into(),
                parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
            }],
            max_tokens: 1024,
            temperature: 0.7,
            system_prompt: Some("you are helpful".into()),
            stop_sequences: vec!["END".into()],
            tool_choice: ToolChoice::Auto,
            system_blocks: Vec::new(),
            cache_tools: false,
        }
    
    }

    #[test]
    fn body_includes_tools_and_stops() {
        let body = build_body("claude-sonnet-4", &req_with_tools());
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert!(body["tools"][0]["input_schema"].is_object());
        assert_eq!(body["stop_sequences"][0], "END");
        assert_eq!(body["system"], "you are helpful");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn assistant_tool_calls_reemitted_as_tool_use() {
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
        let body = build_body("claude-sonnet-4", &r);
        let msgs = body["messages"].as_array().unwrap();
        let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
        assert_eq!(assistant["content"][0]["type"], "tool_use");
        assert_eq!(assistant["content"][0]["id"], "call_1");
        let tool_msg = msgs.last().unwrap();
        assert_eq!(tool_msg["content"][0]["type"], "tool_result");
        assert_eq!(tool_msg["content"][0]["tool_use_id"], "call_1");
    }

    #[test]
    fn parses_tool_use_response() {
        let raw: AnthropicResponse = serde_json::from_value(json!({
            "content":[
                {"type":"tool_use","id":"tu_1","name":"get_weather","input":{"city":"Bogota"}}
            ],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":10,"output_tokens":5}
        }))
        .unwrap();
        let resp = to_chat_response(raw);
        match resp.content {
            ResponseContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_weather");
                assert_eq!(calls[0].arguments["city"], "Bogota");
            }
            _ => panic!("expected ToolCalls"),
        }
        assert_eq!(resp.finish_reason, FinishReason::ToolUse);
        assert_eq!(resp.usage.prompt_tokens, 10);
    }

    #[test]
    fn user_attachment_becomes_image_block() {
        let mut r = req_with_tools();
        r.messages = vec![ChatMessage {
            role: ChatRole::User,
            content: "what's in this?".into(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
            attachments: vec![Attachment::image_base64("image/png", "aGVsbG8=")],
        }];
        let body = build_body("claude-sonnet-4", &r);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "aGVsbG8=");
    }

    #[test]
    fn tool_choice_variants_serialize() {
        let mut r = req_with_tools();
        r.tool_choice = ToolChoice::Any;
        assert_eq!(build_body("m", &r)["tool_choice"]["type"], "any");
        r.tool_choice = ToolChoice::None;
        assert_eq!(build_body("m", &r)["tool_choice"]["type"], "none");
        r.tool_choice = ToolChoice::Specific("get_weather".into());
        let b = build_body("m", &r);
        assert_eq!(b["tool_choice"]["type"], "tool");
        assert_eq!(b["tool_choice"]["name"], "get_weather");
        r.tool_choice = ToolChoice::Auto;
        assert!(build_body("m", &r).get("tool_choice").is_none());
    }

    #[test]
    fn user_turn_with_only_attachment_omits_empty_text_block() {
        let r = ChatRequest {
            model: "claude-sonnet-4".into(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "".into(), // no caption
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
                attachments: vec![Attachment::image_base64("image/jpeg", "aGVsbG8=")],
            }],
            tools: vec![],
            max_tokens: 512,
            temperature: 0.5,
            system_prompt: None,
            stop_sequences: Vec::new(),
            tool_choice: ToolChoice::Auto,
        
            system_blocks: Vec::new(),
            cache_tools: false,
        };
        let body = build_body("claude-sonnet-4", &r);
        let content = &body["messages"][0]["content"];
        // Must start with the image block — no leading empty text.
        assert_eq!(
            content[0]["type"], "image",
            "expected first block to be image, got {content}"
        );
        assert_eq!(content.as_array().unwrap().len(), 1);
    }

    #[test]
    fn user_turn_with_neither_text_nor_renderable_attachment_falls_back() {
        // All attachments are Path variants (unmaterialized) — they
        // get skipped. Anthropic rejects empty content arrays, so we
        // must emit at least one block.
        let r = ChatRequest {
            model: "claude-sonnet-4".into(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "".into(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
                attachments: vec![Attachment::image_path("image/jpeg", "/tmp/foo.jpg")],
            }],
            tools: vec![],
            max_tokens: 512,
            temperature: 0.5,
            system_prompt: None,
            stop_sequences: Vec::new(),
            tool_choice: ToolChoice::Auto,
        
            system_blocks: Vec::new(),
            cache_tools: false,
        };
        let body = build_body("claude-sonnet-4", &r);
        let content = &body["messages"][0]["content"];
        assert_eq!(content.as_array().unwrap().len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "(no content)");
    }

    #[test]
    fn path_variant_attachment_is_skipped() {
        let att = Attachment::image_path("image/jpeg", "/no/such/file.jpg");
        assert!(anthropic_image_block(&att).is_none());
    }

    #[test]
    fn non_image_attachment_is_skipped() {
        let att = Attachment {
            kind: "audio".into(),
            mime_type: "audio/ogg".into(),
            data: crate::types::AttachmentData::Base64 {
                base64: "AAAA".into(),
            },
        };
        assert!(anthropic_image_block(&att).is_none());
    }

    #[test]
    fn retry_after_numeric_seconds() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("retry-after", "30".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(30_000));
    }

    #[test]
    fn retry_after_http_date_uses_delta_not_1s() {
        // A future HTTP-date — we should NOT fall back to 1000ms.
        let mut h = reqwest::header::HeaderMap::new();
        // Use a date well in the future so the parsed delta >> 1s.
        h.insert(
            "retry-after",
            "Wed, 31 Dec 2099 23:59:59 GMT".parse().unwrap(),
        );
        let got = parse_retry_after(&h).unwrap();
        assert!(got > 10_000, "expected >10s, got {got}ms");
    }

    #[test]
    fn retry_after_unparseable_defaults_to_60s() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("retry-after", "garbage".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(60_000));
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
    fn validate_rejects_empty_messages_and_system() {
        let r = ChatRequest {
            model: "claude-sonnet-4".into(),
            messages: vec![],
            tools: vec![],
            max_tokens: 512,
            temperature: 0.5,
            system_prompt: None,
            stop_sequences: Vec::new(),
            tool_choice: ToolChoice::Auto,
        
            system_blocks: Vec::new(),
            cache_tools: false,
        };
        let err = validate_request(&r).unwrap_err();
        assert!(
            format!("{err:?}").contains("messages cannot be empty"),
            "{err:?}"
        );
    }

    #[test]
    fn prompt_cache_tokens_folded_into_prompt_tally() {
        let raw: AnthropicResponse = serde_json::from_value(json!({
            "content":[{"type":"text","text":"ok"}],
            "stop_reason":"end_turn",
            "usage":{
                "input_tokens": 100,
                "output_tokens": 20,
                "cache_creation_input_tokens": 500,
                "cache_read_input_tokens": 1500
            }
        }))
        .unwrap();
        let resp = to_chat_response(raw);
        // 100 + 500 + 1500 = 2100
        assert_eq!(resp.usage.prompt_tokens, 2100);
        assert_eq!(resp.usage.completion_tokens, 20);
    }

    #[test]
    fn validate_accepts_system_only() {
        let r = ChatRequest {
            model: "claude-sonnet-4".into(),
            messages: vec![],
            tools: vec![],
            max_tokens: 512,
            temperature: 0.5,
            system_prompt: Some("ok".into()),
            stop_sequences: Vec::new(),
            tool_choice: ToolChoice::Auto,
        
            system_blocks: Vec::new(),
            cache_tools: false,
        };
        assert!(validate_request(&r).is_ok());
    }
}
