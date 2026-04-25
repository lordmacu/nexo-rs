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

use nexo_config::types::llm::{LlmProviderConfig, RetryConfig};
use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitError};

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
use crate::prompt_block::{CachePolicy, PromptBlock};
use crate::types::{
    Attachment, AttachmentData, CacheUsage, ChatRequest, ChatResponse, ChatRole, FinishReason,
    ResponseContent, TokenUsage, ToolCall, ToolChoice,
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

/// Anthropic beta headers needed when a request asks for prompt caching.
/// `prompt-caching-2024-07-31` unlocks the basic 5min ephemeral cache;
/// `extended-cache-ttl-2025-04-11` unlocks the 1h TTL. Both can be
/// listed together — Anthropic ignores betas the model doesn't honor.
/// Operators can override either via env so a renamed beta doesn't
/// require a release.
const CACHE_BETA_BASIC: &str = "prompt-caching-2024-07-31";
const CACHE_BETA_LONG_TTL: &str = "extended-cache-ttl-2025-04-11";

/// Merge an existing beta header (set by the auth layer for OAuth /
/// Claude Code paths) with the prompt-caching betas. Returns `None`
/// when neither side wants a beta header. `want_long_ttl` adds the
/// extended-TTL beta on top of the basic one (only when at least one
/// `Ephemeral1h` block or `cache_tools=true` was present).
pub(crate) fn merge_beta_headers(
    existing: Option<&str>,
    want_cache_beta: bool,
    want_long_ttl: bool,
) -> Option<String> {
    let mut cache_pieces: Vec<String> = Vec::new();
    if want_cache_beta {
        cache_pieces.push(
            std::env::var("ANTHROPIC_CACHE_BETA")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| CACHE_BETA_BASIC.to_string()),
        );
    }
    if want_long_ttl {
        cache_pieces.push(
            std::env::var("ANTHROPIC_CACHE_LONG_TTL_BETA")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| CACHE_BETA_LONG_TTL.to_string()),
        );
    }
    if existing.is_none() && cache_pieces.is_empty() {
        return None;
    }
    let mut seen: Vec<String> = Vec::new();
    let from_existing: Vec<&str> = existing.map(|s| s.split(',').collect()).unwrap_or_default();
    for piece in from_existing
        .into_iter()
        .chain(cache_pieces.iter().flat_map(|s| s.split(',')))
    {
        let t = piece.trim();
        if t.is_empty() {
            continue;
        }
        if !seen.iter().any(|s| s == t) {
            seen.push(t.to_string());
        }
    }
    if seen.is_empty() {
        None
    } else {
        Some(seen.join(","))
    }
}

/// Read a reqwest response body lossily — `text()` returns `Err` on
/// invalid UTF-8 or transport-level read errors, both of which lose
/// the body entirely and leave us with empty error logs. Read raw
/// bytes and run them through `from_utf8_lossy` so we always keep
/// *something* on disk for debugging.
async fn read_body_lossy(response: reqwest::Response) -> String {
    match response.bytes().await {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => String::new(),
    }
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
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "reqwest client build failed; falling back to default client (no timeout)");
                reqwest::Client::new()
            });
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
            let body = read_body_lossy(response).await;
            return Err(LlmError::ServerError { status, body });
        }
        if status == 401 || status == 403 {
            let body = read_body_lossy(response).await;
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
            let body = read_body_lossy(response).await;
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
        let cache_flags = caching_flags(&self.model, req);
        if let Some(beta) = merge_beta_headers(
            headers.beta.as_deref(),
            cache_flags.any_cache,
            cache_flags.any_long_ttl,
        ) {
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
        let cache_flags = caching_flags(&self.model, req);
        if let Some(beta) = merge_beta_headers(
            headers.beta.as_deref(),
            cache_flags.any_cache,
            cache_flags.any_long_ttl,
        ) {
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

/// Models that pre-date prompt-caching (claude-2.x). Trying to attach
/// `cache_control` to one of these returns HTTP 400; emit one warning
/// then strip the markers before building the body.
fn model_supports_caching(model: &str) -> bool {
    !model.starts_with("claude-2")
}

/// Materialize a contiguous run of `PromptBlock`s into a JSON array of
/// Anthropic content blocks, placing `cache_control` on the LAST block
/// of each contiguous same-policy run with policy != None. Anthropic
/// caps requests at 4 breakpoints — we silently drop the 5th and
/// onwards (the prefix-cache still hits, the tail just won't).
fn render_system_blocks(blocks: &[PromptBlock], allow_cache: bool) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(blocks.len());
    let mut breakpoints_used: u8 = 0;
    let n = blocks.len();
    for (i, b) in blocks.iter().enumerate() {
        if b.text.is_empty() {
            continue;
        }
        let mut block = json!({ "type": "text", "text": b.text });
        if allow_cache && b.cache.is_cached() && breakpoints_used < 4 {
            // Place breakpoint when the next block has a different
            // policy (or this is the last block). Within a same-policy
            // run we let the marker land on the tail block — Anthropic
            // matches prefix-up-to-and-including the marker.
            let next_policy = blocks.get(i + 1).map(|nb| nb.cache);
            let last_in_run = match next_policy {
                None => true,
                Some(p) => p != b.cache,
            };
            if last_in_run || i + 1 == n {
                block["cache_control"] = cache_control_for(b.cache);
                breakpoints_used = breakpoints_used.saturating_add(1);
            }
        }
        out.push(block);
    }
    out
}

fn cache_control_for(policy: CachePolicy) -> Value {
    match policy {
        CachePolicy::None => Value::Null,
        CachePolicy::Ephemeral5m => json!({ "type": "ephemeral" }),
        CachePolicy::Ephemeral1h => json!({ "type": "ephemeral", "ttl": "1h" }),
    }
}

/// Detects whether a request would actually request prompt-caching from
/// Anthropic, given the model's eligibility. The do_request / open_stream
/// paths use this to decide whether to attach the `prompt-caching` /
/// `extended-cache-ttl` beta headers.
pub(crate) struct CachingFlags {
    pub any_cache: bool,
    pub any_long_ttl: bool,
}

pub(crate) fn caching_flags(model: &str, req: &ChatRequest) -> CachingFlags {
    if !model_supports_caching(model) {
        return CachingFlags {
            any_cache: false,
            any_long_ttl: false,
        };
    }
    let mut any_cache = false;
    let mut any_long_ttl = false;
    for b in &req.system_blocks {
        if b.text.is_empty() {
            continue;
        }
        if b.cache.is_cached() {
            any_cache = true;
        }
        if matches!(b.cache, CachePolicy::Ephemeral1h) {
            any_long_ttl = true;
        }
    }
    if req.cache_tools && !req.tools.is_empty() {
        any_cache = true;
        any_long_ttl = true; // tools always use 1h (stable catalog)
    }
    CachingFlags {
        any_cache,
        any_long_ttl,
    }
}

fn build_body(model: &str, req: &ChatRequest) -> Value {
    let allow_cache = model_supports_caching(model);
    if !allow_cache && (!req.system_blocks.is_empty() || req.cache_tools) {
        // Warn once-per-process per legacy model so logs don't drown.
        log_unsupported_caching_once(model);
    }
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
    // System: prefer structured `system_blocks` when present (enables
    // cache_control breakpoints). Fallback to flat string from legacy
    // `system_prompt` + any role=System messages collected above. When
    // both are present, the blocks come first and the legacy parts are
    // appended as one trailing uncached text block (back-compat).
    let blocks_present = req.system_blocks.iter().any(|b| !b.text.is_empty());
    if blocks_present {
        let mut sys = render_system_blocks(&req.system_blocks, allow_cache);
        if !system_parts.is_empty() {
            sys.push(json!({
                "type": "text",
                "text": system_parts.join("\n\n"),
            }));
        }
        body["system"] = Value::Array(sys);
    } else if !system_parts.is_empty() {
        body["system"] = Value::String(system_parts.join("\n\n"));
    }
    if !req.stop_sequences.is_empty() {
        body["stop_sequences"] = json!(req.stop_sequences);
    }
    if !req.tools.is_empty() {
        // Sort tools by name for cache stability — order matters for
        // Anthropic's prefix matching, and the tool registry can boot
        // them in non-deterministic order (DashMap iteration).
        let mut tools_sorted: Vec<&crate::types::ToolDef> = req.tools.iter().collect();
        tools_sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let last_idx = tools_sorted.len() - 1;
        let tools: Vec<Value> = tools_sorted
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut obj = json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                });
                if i == last_idx && req.cache_tools && allow_cache {
                    obj["cache_control"] = cache_control_for(CachePolicy::Ephemeral1h);
                }
                obj
            })
            .collect();
        body["tools"] = json!(tools);
        if let Some(tc) = anthropic_tool_choice(&req.tool_choice) {
            body["tool_choice"] = tc;
        }
    }
    body
}

fn log_unsupported_caching_once(model: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let set = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut g) = set.lock() {
        if g.insert(model.to_string()) {
            tracing::warn!(
                model,
                "anthropic: model predates prompt-caching — stripping cache_control markers \
                 (PromptBlock/cache_tools fields are silently passed through as plain content)"
            );
        }
    }
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
    // Cache accounting: only emit `cache_usage` when at least one cache
    // counter came back set (otherwise providers / models without
    // caching would muddy hit-ratio dashboards with denominator-only
    // entries).
    let cache_usage = resp.usage.as_ref().and_then(|u| {
        let read = u.cache_read_input_tokens.unwrap_or(0);
        let creation = u.cache_creation_input_tokens.unwrap_or(0);
        if read == 0 && creation == 0 {
            return None;
        }
        Some(CacheUsage {
            cache_read_input_tokens: read,
            cache_creation_input_tokens: creation,
            input_tokens: u.input_tokens.unwrap_or(0),
            output_tokens: u.output_tokens.unwrap_or(0),
        })
    });
    ChatResponse {
        content,
        usage,
        finish_reason,
        cache_usage,
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

    // -------- prompt-caching tests (Phase A) --------

    use crate::prompt_block::{CachePolicy, PromptBlock};

    #[test]
    fn model_caching_eligibility() {
        assert!(model_supports_caching("claude-sonnet-4-5"));
        assert!(model_supports_caching("claude-opus-4-5"));
        assert!(model_supports_caching("claude-haiku-4-5"));
        assert!(!model_supports_caching("claude-2.1"));
        assert!(!model_supports_caching("claude-2.0"));
    }

    fn req_with_blocks() -> ChatRequest {
        ChatRequest {
            model: "claude-sonnet-4-5".into(),
            messages: vec![ChatMessage::user("hi")],
            tools: vec![
                ToolDef {
                    name: "b_tool".into(),
                    description: "b".into(),
                    parameters: json!({"type":"object"}),
                },
                ToolDef {
                    name: "a_tool".into(),
                    description: "a".into(),
                    parameters: json!({"type":"object"}),
                },
            ],
            max_tokens: 1024,
            temperature: 0.7,
            system_prompt: None,
            stop_sequences: Vec::new(),
            tool_choice: ToolChoice::Auto,
            system_blocks: vec![
                PromptBlock::cached_long("identity", "You are Ana."),
                PromptBlock::cached_long("skills", "## SKILLS\n- weather"),
                PromptBlock::cached_short("tail", "current time: 12:00"),
            ],
            cache_tools: true,
        }
    }

    #[test]
    fn system_blocks_render_with_cache_control() {
        let body = build_body("claude-sonnet-4-5", &req_with_blocks());
        let sys = body["system"].as_array().expect("system is array");
        assert_eq!(sys.len(), 3);
        assert_eq!(sys[0]["text"], "You are Ana.");
        // Two consecutive Ephemeral1h: marker only on the LAST of the run.
        assert!(sys[0].get("cache_control").is_none());
        assert_eq!(sys[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(sys[1]["cache_control"]["ttl"], "1h");
        // Different policy on tail → its own marker.
        assert_eq!(sys[2]["cache_control"]["type"], "ephemeral");
        assert!(sys[2]["cache_control"].get("ttl").is_none());
    }

    #[test]
    fn tools_sorted_and_last_gets_cache_control() {
        let body = build_body("claude-sonnet-4-5", &req_with_blocks());
        let tools = body["tools"].as_array().expect("tools is array");
        assert_eq!(tools[0]["name"], "a_tool");
        assert_eq!(tools[1]["name"], "b_tool");
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(tools[1]["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn legacy_model_strips_cache_control() {
        let mut r = req_with_blocks();
        r.model = "claude-2.1".into();
        let body = build_body("claude-2.1", &r);
        // Blocks still render but no cache_control fields.
        let sys = body["system"].as_array().unwrap();
        for b in sys {
            assert!(b.get("cache_control").is_none());
        }
        let tools = body["tools"].as_array().unwrap();
        for t in tools {
            assert!(t.get("cache_control").is_none());
        }
    }

    #[test]
    fn empty_blocks_fallback_to_string_system() {
        let mut r = req_with_blocks();
        r.system_blocks.clear();
        r.system_prompt = Some("legacy text".into());
        let body = build_body("claude-sonnet-4-5", &r);
        assert_eq!(body["system"], "legacy text");
    }

    #[test]
    fn breakpoint_cap_at_four_silently_drops_extras() {
        let blocks: Vec<PromptBlock> = (0..6)
            .map(|i| PromptBlock {
                label: "x",
                text: format!("block {i}"),
                cache: if i % 2 == 0 {
                    CachePolicy::Ephemeral1h
                } else {
                    CachePolicy::Ephemeral5m
                },
            })
            .collect();
        let rendered = render_system_blocks(&blocks, true);
        let with_marker = rendered.iter().filter(|b| b.get("cache_control").is_some()).count();
        assert!(with_marker <= 4, "got {with_marker}");
    }

    #[test]
    fn caching_flags_detect_long_ttl() {
        let r = req_with_blocks();
        let f = caching_flags("claude-sonnet-4-5", &r);
        assert!(f.any_cache);
        assert!(f.any_long_ttl);
    }

    #[test]
    fn caching_flags_legacy_model_disables_all() {
        let mut r = req_with_blocks();
        r.model = "claude-2.1".into();
        let f = caching_flags("claude-2.1", &r);
        assert!(!f.any_cache);
        assert!(!f.any_long_ttl);
    }

    #[test]
    fn merge_beta_headers_combines_existing_and_cache() {
        let m = merge_beta_headers(Some("foo-2024-01-01"), true, true).unwrap();
        assert!(m.contains("foo-2024-01-01"));
        assert!(m.contains("prompt-caching-2024-07-31"));
        assert!(m.contains("extended-cache-ttl-2025-04-11"));
    }

    #[test]
    fn merge_beta_headers_dedupes() {
        let m = merge_beta_headers(
            Some("prompt-caching-2024-07-31,foo"),
            true,
            false,
        )
        .unwrap();
        assert_eq!(m.matches("prompt-caching-2024-07-31").count(), 1);
        assert!(m.contains("foo"));
    }

    #[test]
    fn merge_beta_headers_returns_none_when_no_input() {
        assert!(merge_beta_headers(None, false, false).is_none());
    }

    #[test]
    fn merge_beta_headers_no_long_ttl_when_only_short() {
        let m = merge_beta_headers(None, true, false).unwrap();
        assert!(m.contains("prompt-caching-2024-07-31"));
        assert!(!m.contains("extended-cache-ttl"));
    }

    #[test]
    fn parse_response_with_cache_usage_populates_field() {
        let raw = r#"{
          "content":[{"type":"text","text":"ok"}],
          "stop_reason":"end_turn",
          "usage":{
            "input_tokens": 50,
            "output_tokens": 10,
            "cache_read_input_tokens": 8000,
            "cache_creation_input_tokens": 0
          }
        }"#;
        let parsed: AnthropicResponse = serde_json::from_str(raw).unwrap();
        let resp = to_chat_response(parsed);
        let cu = resp.cache_usage.expect("cache_usage populated");
        assert_eq!(cu.cache_read_input_tokens, 8000);
        assert_eq!(cu.cache_creation_input_tokens, 0);
        assert_eq!(cu.input_tokens, 50);
        assert_eq!(cu.output_tokens, 10);
        // hit_ratio: 8000 / (8000 + 0 + 50) ≈ 0.99
        assert!(cu.hit_ratio() > 0.99);
    }

    #[test]
    fn parse_response_without_cache_usage_leaves_none() {
        let raw = r#"{
          "content":[{"type":"text","text":"ok"}],
          "stop_reason":"end_turn",
          "usage":{ "input_tokens": 50, "output_tokens": 10 }
        }"#;
        let parsed: AnthropicResponse = serde_json::from_str(raw).unwrap();
        let resp = to_chat_response(parsed);
        assert!(resp.cache_usage.is_none());
    }
}
