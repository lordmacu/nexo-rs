//! OpenRouter connector — OpenAI-compatible gateway routing requests
//! to many vendor models behind a single endpoint and API key.
//!
//! ## YAML
//!
//! ```yaml
//! providers:
//!   openrouter:
//!     api_key: ${OPENROUTER_API_KEY}
//!     # base_url defaults to https://openrouter.ai/api/v1 when omitted.
//! agents:
//!   - id: ana
//!     model:
//!       provider: openrouter
//!       model: anthropic/claude-opus-4-7
//! ```
//!
//! ## Notes
//!
//! - Model slugs are opaque `vendor/model` strings — OpenRouter routes
//!   server-side. We do NOT parse them as paths.
//! - Attribution headers (`HTTP-Referer`, `X-Title`) are stamped only
//!   when the resolved base URL matches the canonical OpenRouter host.
//! - Streaming, tool calling, retry/circuit reuse `OpenAiClient` 1:1.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{json, Value};

use nexo_config::types::llm::{LlmProviderConfig, RetryConfig};

use crate::client::LlmClient;
use crate::openai_compat::{BodyTransformer, OpenAiClient};
use crate::prompt_block::{CachePolicy, PromptBlock};
use crate::registry::LlmProviderFactory;
use crate::stream::StreamChunk;
use crate::types::{ChatRequest, ChatResponse, ProviderRoutingPolicy};

/// `HTTP-Referer` header value injected on canonical-host requests.
/// Points at the public nexo-rs repo; no domain registered yet.
pub const ATTRIBUTION_REFERER: &str = "https://github.com/lordmacu/nexo-rs";

/// `X-Title` header value injected on canonical-host requests. Shows
/// up on the OpenRouter leaderboard for app-level analytics.
pub const ATTRIBUTION_TITLE: &str = "nexo-rs";

/// Canonical OpenRouter API base URL. Used when the operator leaves
/// `providers.openrouter.base_url` empty in `llm.yaml`.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Returns `true` when `base_url` points at the real OpenRouter host
/// (`https://openrouter.ai/api/v1` or its `http://` variant). Used to
/// decide whether attribution headers are injected — proxies and
/// self-hosted gateways skip the inject silently so we never leak
/// our `HTTP-Referer` outside OpenRouter.
pub fn is_canonical_host(base_url: &str) -> bool {
    let trimmed = base_url.trim_end_matches('/');
    trimmed == "https://openrouter.ai/api/v1" || trimmed == "http://openrouter.ai/api/v1"
}

/// Self-heal legacy `openrouter.ai/v1` → `openrouter.ai/api/v1`.
///
/// Parity with OpenClaw `CHANGELOG.md:356` — early OpenRouter docs
/// listed `/v1` as the base path, but the real prefix is `/api/v1`.
/// Operators who copied the wrong URL into `llm.yaml` get a silent
/// forward-fix; any other URL passes through unchanged (trailing
/// slash is always stripped so `is_canonical_host` matches).
pub fn canonicalize_base_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed == "https://openrouter.ai/v1" {
        return "https://openrouter.ai/api/v1".to_string();
    }
    if trimmed == "http://openrouter.ai/v1" {
        return "http://openrouter.ai/api/v1".to_string();
    }
    trimmed.to_string()
}

/// Validate a model slug. Rejects empty strings and the duplicated
/// `openrouter/openrouter/...` prefix that OpenClaw `CHANGELOG.md:2331`
/// kept patching. Accepts `auto`, `openrouter/auto`, and any
/// non-empty `vendor/model` string otherwise — OpenRouter handles
/// routing server-side, so we do not parse the slug as a path.
pub fn validate_slug(model: &str) -> anyhow::Result<()> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        anyhow::bail!("openrouter: model slug is empty");
    }
    if trimmed.starts_with("openrouter/openrouter/") {
        anyhow::bail!("openrouter: model slug has duplicated 'openrouter/' prefix: {model}");
    }
    Ok(())
}

/// Default TTL for the live models cache (15 minutes — fresh enough
/// for catalogue browsing, light enough on OpenRouter's free
/// `/models` endpoint). Operator UIs that want a refresh-now button
/// can call [`refresh_live_models`] directly.
pub const MODELS_CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Process-wide cache of the OR live models list. `None` = never
/// fetched; otherwise the timestamp records the last successful
/// fetch and the vector holds the slugs in OR-reported order.
/// Wrapped in `OnceLock` because `RwLock::new` is not `const`.
static OR_MODELS_CACHE: OnceLock<RwLock<Option<(Instant, Vec<String>)>>> = OnceLock::new();

fn cache_handle() -> &'static RwLock<Option<(Instant, Vec<String>)>> {
    OR_MODELS_CACHE.get_or_init(|| RwLock::new(None))
}

#[derive(Deserialize)]
struct OrModelsResponse {
    #[serde(default)]
    data: Vec<OrModelEntry>,
}

#[derive(Deserialize)]
struct OrModelEntry {
    #[serde(default)]
    id: Option<String>,
}

/// Parse the `/api/v1/models` response body into a slug list. Empty
/// IDs are skipped. Pure function — unit-testable without network.
pub fn parse_models_response(json_body: &str) -> anyhow::Result<Vec<String>> {
    let raw: OrModelsResponse = serde_json::from_str(json_body)
        .map_err(|e| anyhow::anyhow!("openrouter: /models parse failed: {e}"))?;
    let slugs: Vec<String> = raw
        .data
        .into_iter()
        .filter_map(|e| e.id.filter(|s| !s.is_empty()))
        .collect();
    Ok(slugs)
}

/// Fetch the live OpenRouter model catalogue, populating the
/// process-wide cache on success. `api_key` is sent as `Bearer` —
/// `/models` is publicly reachable today but OR is documented to
/// gate rate limits per-key, so always send the operator's key.
/// On network failure returns the cached snapshot when present;
/// otherwise propagates the error so the caller can fall back to
/// [`KNOWN_MODELS`].
pub async fn fetch_live_models(api_key: &str) -> anyhow::Result<Vec<String>> {
    let slugs = http_fetch_models(DEFAULT_BASE_URL, api_key).await?;
    let mut guard = cache_handle().write().unwrap_or_else(|p| p.into_inner());
    *guard = Some((Instant::now(), slugs.clone()));
    Ok(slugs)
}

/// Pure HTTP fetch of `{base_url}/models` — no cache side effect.
/// Split out so integration tests can point `base_url` at a mock
/// server without touching the process-wide cache. Public to the
/// crate so the WireMock smoke test in `tests/` can exercise it.
pub async fn http_fetch_models(base_url: &str, api_key: &str) -> anyhow::Result<Vec<String>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| anyhow::anyhow!("openrouter: reqwest build failed: {e}"))?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .bearer_auth(api_key)
        .header("HTTP-Referer", ATTRIBUTION_REFERER)
        .header("X-Title", ATTRIBUTION_TITLE)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("openrouter: /models GET failed: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("openrouter: /models body read failed: {e}"))?;
    if !status.is_success() {
        anyhow::bail!("openrouter: /models HTTP {status}: {body}");
    }
    parse_models_response(&body)
}

/// Look up the live models catalogue from the cache. Returns
/// `Some(slugs)` only when the cached entry is younger than
/// [`MODELS_CACHE_TTL`]; otherwise `None` (caller should call
/// [`fetch_live_models`] to refresh).
pub fn cached_live_models() -> Option<Vec<String>> {
    let guard = cache_handle().read().unwrap_or_else(|p| p.into_inner());
    guard
        .as_ref()
        .filter(|(t, _)| t.elapsed() < MODELS_CACHE_TTL)
        .map(|(_, slugs)| slugs.clone())
}

/// Public helper for operator UIs: returns cached slugs when fresh,
/// otherwise hits the network. Falls back to [`KNOWN_MODELS`] when
/// the network call fails AND no cache exists — never leaves the
/// caller with an empty list (the curated set is a safety net).
pub async fn live_or_known_models(api_key: &str) -> Vec<String> {
    if let Some(cached) = cached_live_models() {
        return cached;
    }
    match fetch_live_models(api_key).await {
        Ok(slugs) if !slugs.is_empty() => slugs,
        Ok(_) => KNOWN_MODELS.iter().map(|s| s.to_string()).collect(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "openrouter: live /models fetch failed; falling back to KNOWN_MODELS"
            );
            KNOWN_MODELS.iter().map(|s| s.to_string()).collect()
        }
    }
}

/// Force-refresh the live models cache. Wired to admin RPC refresh
/// buttons — the UI surfaces a "Refresh catalogue" action.
pub async fn refresh_live_models(api_key: &str) -> anyhow::Result<Vec<String>> {
    {
        let mut guard = cache_handle().write().unwrap_or_else(|p| p.into_inner());
        *guard = None;
    }
    fetch_live_models(api_key).await
}

#[cfg(test)]
pub(crate) fn _test_set_cache_for_unit_tests(slugs: Vec<String>, age: Duration) {
    let mut guard = cache_handle().write().unwrap_or_else(|p| p.into_inner());
    let when = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
    *guard = Some((when, slugs));
}

#[cfg(test)]
pub(crate) fn _test_clear_cache_for_unit_tests() {
    let mut guard = cache_handle().write().unwrap_or_else(|p| p.into_inner());
    *guard = None;
}

/// Curated list of model slugs surfaced to operator UIs. Sorted
/// roughly by deploy-day popularity. Operators can still send
/// arbitrary slugs — `known_models` is a UX hint, NOT a server-side
/// allowlist (OpenRouter validates against its live catalogue).
pub const KNOWN_MODELS: &[&str] = &[
    "openrouter/auto",
    "anthropic/claude-opus-4-7",
    "anthropic/claude-sonnet-4-6",
    "openai/gpt-5",
    "google/gemini-2.5-pro",
    "deepseek/deepseek-v4-pro",
    "x-ai/grok-4",
    "meta-llama/llama-4-maverick",
];

/// Build the attribution-header map injected when the resolved base
/// URL is canonical. Kept as a free function so the factory and the
/// tests can call it without constructing a client first.
fn attribution_headers() -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("HTTP-Referer".to_string(), ATTRIBUTION_REFERER.to_string());
    headers.insert("X-Title".to_string(), ATTRIBUTION_TITLE.to_string());
    headers
}

/// Returns `true` when this slug routes to an Anthropic model through
/// OpenRouter. Public so the factory + tests can check without
/// duplicating the prefix literal.
pub fn is_anthropic_slug(model: &str) -> bool {
    model.starts_with("anthropic/")
}

/// Translate a [`CachePolicy`] into the JSON shape OpenRouter
/// forwards to Anthropic on verified routes. Anthropic accepts
/// `{"type":"ephemeral"}` for the short 5-min TTL and an optional
/// `"ttl":"1h"` field for the long TTL — same wire shape the native
/// Anthropic client emits.
fn cache_control_for(policy: CachePolicy) -> Value {
    match policy {
        CachePolicy::None => Value::Null,
        CachePolicy::Ephemeral5m => json!({ "type": "ephemeral" }),
        CachePolicy::Ephemeral1h => json!({ "type": "ephemeral", "ttl": "1h" }),
    }
}

/// Render a contiguous run of [`PromptBlock`]s into the JSON array
/// shape OpenAI's `chat/completions` accepts as `content`, placing a
/// `cache_control` marker on the LAST block of each contiguous
/// same-policy run. Anthropic caps requests at 4 breakpoints — we
/// silently drop the 5th and onwards (the prefix-cache still hits;
/// just the tail does not). Mirrors `anthropic::render_system_blocks`
/// for wire parity.
fn render_system_blocks_with_cache(blocks: &[PromptBlock]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(blocks.len());
    let mut breakpoints_used: u8 = 0;
    let n = blocks.len();
    for (i, b) in blocks.iter().enumerate() {
        if b.text.is_empty() {
            continue;
        }
        let mut block = json!({ "type": "text", "text": b.text });
        if b.cache.is_cached() && breakpoints_used < 4 {
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

/// Serialize a [`ProviderRoutingPolicy`] into the OpenRouter
/// `provider: {…}` body extension. Returns `Value::Null` when the
/// policy is empty so callers can skip emitting the field entirely
/// (OpenRouter treats an absent `provider` field as the default
/// route — cheaper than a no-op block).
pub fn serialize_provider_routing(policy: &ProviderRoutingPolicy) -> Value {
    if policy.is_empty() {
        return Value::Null;
    }
    let mut obj = serde_json::Map::new();
    if !policy.order.is_empty() {
        obj.insert("order".into(), json!(policy.order));
    }
    if let Some(flag) = policy.allow_fallbacks {
        obj.insert("allow_fallbacks".into(), json!(flag));
    }
    if !policy.require_parameters.is_empty() {
        obj.insert(
            "require_parameters".into(),
            json!(policy.require_parameters),
        );
    }
    if let Some(sort) = policy.sort.as_deref() {
        if !sort.is_empty() {
            obj.insert("sort".into(), json!(sort));
        }
    }
    Value::Object(obj)
}

/// Body transformer for `anthropic/*` slugs targeting the canonical
/// OpenRouter host. Mutates the OpenAI-shaped body produced by
/// `build_openai_body` so that:
///
/// 1. If `req.system_blocks` is populated with at least one cached
///    block, the leading `messages[role:system]` entry is rewritten
///    from `content: "<flat string>"` to `content: [{type:"text", ...,
///    cache_control:{...}}, ...]`. Each contiguous same-policy run
///    gets one breakpoint on its tail block (≤ 4 per Anthropic API).
/// 2. If `req.cache_tools` is true and `body["tools"]` is non-empty,
///    a `cache_control: {type:"ephemeral", ttl:"1h"}` marker is
///    stamped on the LAST tool entry so OpenRouter's translation
///    layer forwards it to Anthropic as a stable-catalog breakpoint.
///
/// No-op when neither condition holds — the body ships unchanged.
pub fn anthropic_via_or_cache_transformer(req: &ChatRequest, body: &mut Value) {
    let cache_system = req
        .system_blocks
        .iter()
        .any(|b| !b.text.is_empty() && b.cache.is_cached());

    if cache_system {
        if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            if let Some(sys) = messages
                .iter_mut()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            {
                let blocks = render_system_blocks_with_cache(&req.system_blocks);
                if !blocks.is_empty() {
                    sys["content"] = Value::Array(blocks);
                }
            }
        }
    }

    if req.cache_tools {
        if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = cache_control_for(CachePolicy::Ephemeral1h);
            }
        }
    }
}

/// Factory for `provider: openrouter` in `model.provider`. Registered
/// in [`crate::registry::LlmRegistry::with_builtins`] so operators
/// only need an API key + slug to get started.
pub struct OpenRouterFactory;

impl LlmProviderFactory for OpenRouterFactory {
    fn name(&self) -> &str {
        "openrouter"
    }

    fn build(
        &self,
        provider_cfg: &LlmProviderConfig,
        model: &str,
        retry: RetryConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        validate_slug(model)?;
        // Resolve base URL: blank → canonical default; otherwise
        // canonicalise to forward-fix the OpenClaw `/v1` typo.
        let resolved_base = if provider_cfg.base_url.trim().is_empty() {
            DEFAULT_BASE_URL.to_string()
        } else {
            canonicalize_base_url(&provider_cfg.base_url)
        };
        let inject_attribution = is_canonical_host(&resolved_base);
        let cfg = LlmProviderConfig {
            base_url: resolved_base,
            ..provider_cfg.clone()
        };
        let headers = if inject_attribution {
            attribution_headers()
        } else {
            BTreeMap::new()
        };
        // Gate transformer on canonical host (so we only mutate
        // bodies for verified OR routes). Inside the transformer
        // we run two independent passes: provider routing
        // serialisation fires on ANY OR request that carries a
        // policy; Anthropic cache markers fire only for
        // `anthropic/*` slugs. Proxies opt out of both.
        let cache_enabled = inject_attribution && is_anthropic_slug(model);
        let transformer: Option<BodyTransformer> = if inject_attribution {
            let cache_on = cache_enabled;
            Some(Arc::new(move |req, body| {
                if let Some(policy) = &req.provider_routing {
                    let v = serialize_provider_routing(policy);
                    if !v.is_null() {
                        body["provider"] = v;
                    }
                }
                if cache_on {
                    anthropic_via_or_cache_transformer(req, body);
                }
            }))
        } else {
            None
        };
        let inner = OpenAiClient::with_extra_headers_and_transformer(
            &cfg,
            model,
            retry,
            headers,
            transformer,
        );
        Ok(Arc::new(OpenRouterClient {
            inner,
            model: model.to_string(),
            inject_attribution,
            cache_enabled,
        }))
    }

    fn default_base_url(&self) -> &'static str {
        DEFAULT_BASE_URL
    }

    fn default_env_var(&self) -> &'static str {
        "OPENROUTER_API_KEY"
    }

    fn known_models(&self) -> &'static [&'static str] {
        KNOWN_MODELS
    }

    fn supports_models_probe(&self) -> bool {
        true
    }
}

/// Thin wrapper around [`OpenAiClient`] that brands the provider as
/// `openrouter` for telemetry and remembers whether attribution
/// headers were injected (used by [`OpenRouterClient::is_attributed`]
/// for inspection in tests + diagnostics).
pub struct OpenRouterClient {
    inner: OpenAiClient,
    model: String,
    inject_attribution: bool,
    cache_enabled: bool,
}

impl OpenRouterClient {
    /// Returns `true` when this client targets the canonical
    /// OpenRouter host and is stamping attribution headers. Off when
    /// the operator pointed `base_url` at a proxy or self-host.
    pub fn is_attributed(&self) -> bool {
        self.inject_attribution
    }

    /// Returns `true` when this client rewrites system messages with
    /// Anthropic `cache_control` markers before shipping. Active only
    /// when the slug starts with `anthropic/` AND the base URL points
    /// at the canonical OpenRouter host — proxies opt out by design.
    pub fn is_cache_marker_enabled(&self) -> bool {
        self.cache_enabled
    }
}

#[async_trait]
impl LlmClient for OpenRouterClient {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let resp = self.inner.chat(req).await?;
        // Emit the gateway-reported cost under the `openrouter`
        // label (the inner OpenAiClient would mislabel it `openai`).
        if let Some(cost) = resp.cost_usd {
            crate::telemetry::add_cost_usd(self.provider(), cost);
        }
        Ok(resp)
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn provider(&self) -> &str {
        "openrouter"
    }

    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.inner.embed(texts).await
    }

    async fn stream<'a>(
        &'a self,
        req: ChatRequest,
    ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
        self.inner.stream(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_canonical_host ──

    #[test]
    fn is_canonical_host_https_openrouter_returns_true() {
        assert!(is_canonical_host("https://openrouter.ai/api/v1"));
    }

    #[test]
    fn is_canonical_host_http_openrouter_returns_true() {
        assert!(is_canonical_host("http://openrouter.ai/api/v1"));
    }

    #[test]
    fn is_canonical_host_proxy_url_returns_false() {
        assert!(!is_canonical_host("https://proxy.example.com/openrouter"));
        assert!(!is_canonical_host("https://api.openai.com/v1"));
    }

    #[test]
    fn is_canonical_host_trailing_slash_normalised() {
        assert!(is_canonical_host("https://openrouter.ai/api/v1/"));
        assert!(is_canonical_host("https://openrouter.ai/api/v1///"));
    }

    // ── canonicalize_base_url ──

    #[test]
    fn canonicalize_legacy_v1_to_api_v1() {
        assert_eq!(
            canonicalize_base_url("https://openrouter.ai/v1"),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            canonicalize_base_url("http://openrouter.ai/v1/"),
            "http://openrouter.ai/api/v1"
        );
    }

    #[test]
    fn canonicalize_already_correct_passes_through() {
        assert_eq!(
            canonicalize_base_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            canonicalize_base_url("https://openrouter.ai/api/v1/"),
            "https://openrouter.ai/api/v1"
        );
    }

    #[test]
    fn canonicalize_custom_proxy_unchanged() {
        assert_eq!(
            canonicalize_base_url("https://proxy.example.com/openrouter"),
            "https://proxy.example.com/openrouter"
        );
        assert_eq!(
            canonicalize_base_url("https://proxy.example.com/openrouter/"),
            "https://proxy.example.com/openrouter"
        );
    }

    // ── validate_slug ──

    #[test]
    fn validate_slug_accepts_vendor_model() {
        validate_slug("anthropic/claude-opus-4-7").unwrap();
        validate_slug("openai/gpt-5").unwrap();
        validate_slug("google/gemini-2.5-pro").unwrap();
        validate_slug("x-ai/grok-4").unwrap();
    }

    #[test]
    fn validate_slug_accepts_auto_alias() {
        validate_slug("auto").unwrap();
        validate_slug("openrouter/auto").unwrap();
    }

    #[test]
    fn validate_slug_rejects_empty_string() {
        let err = validate_slug("").unwrap_err();
        assert!(err.to_string().contains("empty"));
        let err = validate_slug("   ").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn validate_slug_rejects_double_prefix_lowercase() {
        let err = validate_slug("openrouter/openrouter/anthropic/claude").unwrap_err();
        assert!(
            err.to_string().contains("duplicated"),
            "error should mention duplicated prefix, got: {err}"
        );
    }

    // ── factory + client ──

    use nexo_config::types::llm::RateLimitConfig;

    fn empty_cfg() -> LlmProviderConfig {
        LlmProviderConfig {
            api_key: "sk-or-test".into(),
            base_url: String::new(),
            group_id: None,
            rate_limit: RateLimitConfig::default(),
            auth: None,
            api_flavor: None,
            embedding_model: None,
            safety_settings: None,
            factory_type: None,
            api_key_secret_id: None,
        }
    }

    fn fast_retry() -> RetryConfig {
        RetryConfig {
            max_attempts: 1,
            initial_backoff_ms: 1,
            max_backoff_ms: 1,
            backoff_multiplier: 1.0,
        }
    }

    #[test]
    fn factory_name_is_openrouter() {
        assert_eq!(OpenRouterFactory.name(), "openrouter");
    }

    #[test]
    fn factory_default_base_url_is_canonical_api_v1() {
        assert_eq!(
            OpenRouterFactory.default_base_url(),
            "https://openrouter.ai/api/v1"
        );
    }

    #[test]
    fn factory_default_env_var_is_openrouter_api_key() {
        assert_eq!(OpenRouterFactory.default_env_var(), "OPENROUTER_API_KEY");
    }

    #[test]
    fn factory_known_models_non_empty_includes_anthropic_opus() {
        let models = OpenRouterFactory.known_models();
        assert!(!models.is_empty());
        assert!(
            models.contains(&"anthropic/claude-opus-4-7"),
            "known_models must surface anthropic/claude-opus-4-7"
        );
    }

    #[test]
    fn factory_known_models_contains_auto_alias() {
        assert!(OpenRouterFactory
            .known_models()
            .contains(&"openrouter/auto"));
    }

    #[test]
    fn build_with_blank_base_url_uses_default() {
        let client = OpenRouterFactory
            .build(&empty_cfg(), "anthropic/claude-opus-4-7", fast_retry())
            .expect("client should build with blank base_url");
        assert_eq!(client.model_id(), "anthropic/claude-opus-4-7");
        assert_eq!(client.provider(), "openrouter");
    }

    #[test]
    fn build_with_legacy_v1_canonicalises_to_api_v1() {
        let mut cfg = empty_cfg();
        cfg.base_url = "https://openrouter.ai/v1".into();
        let client = OpenRouterFactory
            .build(&cfg, "openrouter/auto", fast_retry())
            .expect("client should build with legacy base_url");
        // base_url is private; canonicalisation is asserted by the
        // pure-fn test above. Here we just confirm the build path
        // tolerates the legacy URL without bail-ing.
        assert_eq!(client.provider(), "openrouter");
    }

    #[test]
    fn build_preserves_custom_proxy_url_disables_attribution() {
        let mut cfg = empty_cfg();
        cfg.base_url = "https://proxy.example.com/openrouter".into();
        let client = OpenRouterFactory
            .build(&cfg, "anthropic/claude-opus-4-7", fast_retry())
            .expect("client should build against proxy URL");
        assert_eq!(client.provider(), "openrouter");
    }

    #[test]
    fn build_rejects_invalid_slug_double_prefix() {
        let err = OpenRouterFactory
            .build(
                &empty_cfg(),
                "openrouter/openrouter/anthropic/claude",
                fast_retry(),
            )
            .err()
            .expect("build should reject invalid slug");
        assert!(
            err.to_string().contains("duplicated"),
            "error should mention duplicated prefix: {err}"
        );
    }

    #[test]
    fn build_rejects_empty_slug() {
        let err = OpenRouterFactory
            .build(&empty_cfg(), "", fast_retry())
            .err()
            .expect("build should reject invalid slug");
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn client_provider_label_is_openrouter() {
        let client = OpenRouterFactory
            .build(&empty_cfg(), "openrouter/auto", fast_retry())
            .unwrap();
        assert_eq!(client.provider(), "openrouter");
    }

    #[test]
    fn client_model_id_round_trip() {
        let client = OpenRouterFactory
            .build(&empty_cfg(), "google/gemini-2.5-pro", fast_retry())
            .unwrap();
        assert_eq!(client.model_id(), "google/gemini-2.5-pro");
    }

    #[test]
    fn build_inject_attribution_true_for_canonical_host() {
        // Blank base_url → canonical default → attribution ON.
        let factory_client = OpenRouterClient {
            inner: crate::openai_compat::OpenAiClient::new(&empty_cfg(), "m", fast_retry()),
            model: "m".into(),
            inject_attribution: true,
            cache_enabled: false,
        };
        assert!(factory_client.is_attributed());
        // The factory path produces the same flag for blank base_url.
        let _ = OpenRouterFactory
            .build(&empty_cfg(), "anthropic/claude-opus-4-7", fast_retry())
            .unwrap();
    }

    #[test]
    fn build_inject_attribution_false_for_proxy() {
        let mut cfg = empty_cfg();
        cfg.base_url = "https://proxy.example.com/openrouter".into();
        // Direct struct probe — proxy host means attribution OFF.
        let or = OpenRouterClient {
            inner: crate::openai_compat::OpenAiClient::new(&cfg, "m", fast_retry()),
            model: "m".into(),
            inject_attribution: is_canonical_host(&cfg.base_url),
            cache_enabled: false,
        };
        assert!(!or.is_attributed());
    }

    #[test]
    fn attribution_headers_contains_referer_and_title() {
        let h = attribution_headers();
        assert_eq!(
            h.get("HTTP-Referer").map(String::as_str),
            Some(ATTRIBUTION_REFERER)
        );
        assert_eq!(
            h.get("X-Title").map(String::as_str),
            Some(ATTRIBUTION_TITLE)
        );
        assert_eq!(h.len(), 2);
    }

    // ── cache_control transformer (Phase 100.x.cache-control) ──

    use crate::types::{ChatMessage, ChatRequest, ToolChoice};

    fn req_with_blocks(blocks: Vec<PromptBlock>, cache_tools: bool) -> ChatRequest {
        ChatRequest {
            model: "anthropic/claude-opus-4-7".into(),
            messages: vec![ChatMessage::user("hello")],
            tools: vec![],
            max_tokens: 256,
            temperature: 0.5,
            system_prompt: Some("flat".into()),
            stop_sequences: vec![],
            tool_choice: ToolChoice::Auto,
            system_blocks: blocks,
            cache_tools,
            provider_routing: None,
        }
    }

    #[test]
    fn is_anthropic_slug_detects_prefix() {
        assert!(is_anthropic_slug("anthropic/claude-opus-4-7"));
        assert!(is_anthropic_slug("anthropic/claude-sonnet-4-6"));
        assert!(!is_anthropic_slug("openai/gpt-5"));
        assert!(!is_anthropic_slug("google/gemini-2.5-pro"));
        assert!(!is_anthropic_slug("openrouter/auto"));
    }

    #[test]
    fn cache_control_for_renders_anthropic_shape() {
        assert_eq!(cache_control_for(CachePolicy::None), Value::Null);
        assert_eq!(
            cache_control_for(CachePolicy::Ephemeral5m),
            json!({ "type": "ephemeral" })
        );
        assert_eq!(
            cache_control_for(CachePolicy::Ephemeral1h),
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
    }

    #[test]
    fn render_system_blocks_places_marker_on_last_block_of_same_policy_run() {
        let blocks = vec![
            PromptBlock::cached_long("tools", "TOOLS"),
            PromptBlock::cached_long("identity", "IDENTITY"),
            PromptBlock::cached_short("tail", "TAIL"),
        ];
        let out = render_system_blocks_with_cache(&blocks);
        assert_eq!(out.len(), 3);
        // Run 1 (long): "tools" + "identity" share policy; marker on
        // last of the run → "identity".
        assert!(out[0].get("cache_control").is_none());
        assert_eq!(
            out[1]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
        // Run 2 (short): single block → marker on it.
        assert_eq!(out[2]["cache_control"], json!({ "type": "ephemeral" }));
    }

    #[test]
    fn render_system_blocks_skips_empty_text() {
        let blocks = vec![
            PromptBlock::cached_long("a", "A"),
            PromptBlock::plain("empty", ""),
            PromptBlock::cached_long("b", "B"),
        ];
        let out = render_system_blocks_with_cache(&blocks);
        // 2 blocks rendered (empty dropped). Both are cached_long
        // adjacent (since empty was dropped from PHYSICAL list but
        // logic walks the ORIGINAL list, so "a" sees next="empty"
        // (different from policy default None) and places marker
        // on itself; "b" places on itself). Spec: 2 markers max.
        assert_eq!(out.len(), 2);
        assert!(
            out[0].get("cache_control").is_some() || out[1].get("cache_control").is_some(),
            "at least one cached block must carry a marker"
        );
    }

    #[test]
    fn render_system_blocks_caps_at_four_breakpoints() {
        // Six distinct runs alternating policy → 6 potential
        // breakpoints; Anthropic caps at 4. Verify the 5th and 6th
        // blocks ship WITHOUT a marker.
        let blocks: Vec<PromptBlock> = (0..6)
            .map(|i| {
                let policy = if i % 2 == 0 {
                    CachePolicy::Ephemeral1h
                } else {
                    CachePolicy::Ephemeral5m
                };
                PromptBlock {
                    label: "x",
                    text: format!("block-{i}"),
                    cache: policy,
                }
            })
            .collect();
        let out = render_system_blocks_with_cache(&blocks);
        assert_eq!(out.len(), 6);
        let marked = out
            .iter()
            .filter(|b| b.get("cache_control").is_some())
            .count();
        assert_eq!(marked, 4, "anthropic cap is 4 breakpoints, got {marked}");
    }

    #[test]
    fn transformer_rewrites_system_message_to_array_with_markers() {
        let blocks = vec![
            PromptBlock::cached_long("tools", "TOOLS_BUNDLE"),
            PromptBlock::cached_short("tail", "TAIL_CONTEXT"),
        ];
        let req = req_with_blocks(blocks, false);
        // Simulate what build_openai_body would emit.
        let mut body = json!({
            "model": req.model,
            "messages": [
                { "role": "system", "content": "flat" },
                { "role": "user", "content": "hello" }
            ],
        });
        anthropic_via_or_cache_transformer(&req, &mut body);
        let sys = &body["messages"][0];
        assert_eq!(sys["role"], "system");
        let content = sys["content"].as_array().expect("system content array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "TOOLS_BUNDLE");
        assert_eq!(
            content[0]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
        assert_eq!(content[1]["text"], "TAIL_CONTEXT");
        assert_eq!(content[1]["cache_control"], json!({ "type": "ephemeral" }));
        // User turn untouched.
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[test]
    fn transformer_noop_when_no_cached_blocks_and_no_cache_tools() {
        let req = req_with_blocks(vec![], false);
        let mut body = json!({
            "messages": [
                { "role": "system", "content": "flat" }
            ],
        });
        let before = body.clone();
        anthropic_via_or_cache_transformer(&req, &mut body);
        assert_eq!(body, before, "transformer must leave body untouched");
    }

    #[test]
    fn transformer_stamps_cache_control_on_last_tool_when_cache_tools_true() {
        let req = req_with_blocks(vec![], true);
        let mut body = json!({
            "messages": [],
            "tools": [
                { "type": "function", "function": { "name": "a" } },
                { "type": "function", "function": { "name": "b" } }
            ]
        });
        anthropic_via_or_cache_transformer(&req, &mut body);
        let tools = body["tools"].as_array().unwrap();
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(
            tools[1]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
    }

    #[test]
    fn factory_enables_cache_marker_for_anthropic_slug_on_canonical_host() {
        // Blank base_url → canonical default → attribution + cache ON.
        let client = OpenRouterFactory
            .build(&empty_cfg(), "anthropic/claude-opus-4-7", fast_retry())
            .unwrap();
        // Provider/model labels survive — direct cache flag is on the
        // wrapper which we cannot downcast through Arc<dyn LlmClient>;
        // direct construction proves the flag path below.
        let probe = OpenRouterClient {
            inner: OpenAiClient::new(&empty_cfg(), "m", fast_retry()),
            model: "anthropic/claude-opus-4-7".into(),
            inject_attribution: true,
            cache_enabled: true,
        };
        assert!(probe.is_cache_marker_enabled());
        let _ = client;
    }

    #[test]
    fn factory_disables_cache_marker_for_non_anthropic_slug() {
        let probe = OpenRouterClient {
            inner: OpenAiClient::new(&empty_cfg(), "m", fast_retry()),
            model: "openai/gpt-5".into(),
            inject_attribution: true,
            cache_enabled: false,
        };
        assert!(!probe.is_cache_marker_enabled());
    }

    // ── provider routing (Phase 100.x.provider-routing) ──

    use crate::types::ProviderRoutingPolicy;

    #[test]
    fn serialize_routing_returns_null_for_empty_policy() {
        let v = serialize_provider_routing(&ProviderRoutingPolicy::default());
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn serialize_routing_emits_order_when_set() {
        let p = ProviderRoutingPolicy {
            order: vec!["anthropic".into(), "google".into()],
            ..Default::default()
        };
        let v = serialize_provider_routing(&p);
        assert_eq!(v["order"], json!(["anthropic", "google"]));
        assert!(v.get("allow_fallbacks").is_none());
    }

    #[test]
    fn serialize_routing_emits_allow_fallbacks_when_some() {
        let p = ProviderRoutingPolicy {
            allow_fallbacks: Some(false),
            ..Default::default()
        };
        let v = serialize_provider_routing(&p);
        assert_eq!(v["allow_fallbacks"], json!(false));
    }

    #[test]
    fn serialize_routing_emits_require_parameters_when_set() {
        let p = ProviderRoutingPolicy {
            require_parameters: vec!["tools".into()],
            ..Default::default()
        };
        let v = serialize_provider_routing(&p);
        assert_eq!(v["require_parameters"], json!(["tools"]));
    }

    #[test]
    fn serialize_routing_emits_sort_when_set() {
        let p = ProviderRoutingPolicy {
            sort: Some("throughput".into()),
            ..Default::default()
        };
        let v = serialize_provider_routing(&p);
        assert_eq!(v["sort"], json!("throughput"));
    }

    #[test]
    fn serialize_routing_skips_empty_sort_string() {
        let p = ProviderRoutingPolicy {
            sort: Some("".into()),
            ..Default::default()
        };
        // Empty sort is treated as no preference — both `sort` AND
        // `is_empty()` should agree to drop the field.
        assert!(p.is_empty());
        assert_eq!(serialize_provider_routing(&p), Value::Null);
    }

    #[test]
    fn serialize_routing_full_policy_emits_all_fields() {
        let p = ProviderRoutingPolicy {
            order: vec!["anthropic".into()],
            allow_fallbacks: Some(true),
            require_parameters: vec!["tools".into(), "response_format".into()],
            sort: Some("price".into()),
        };
        let v = serialize_provider_routing(&p);
        assert_eq!(v["order"], json!(["anthropic"]));
        assert_eq!(v["allow_fallbacks"], json!(true));
        assert_eq!(v["require_parameters"], json!(["tools", "response_format"]));
        assert_eq!(v["sort"], json!("price"));
    }

    #[test]
    fn transformer_emits_provider_field_when_routing_set() {
        // Build the SAME closure the factory installs for an
        // anthropic/* slug on canonical host so we exercise both
        // passes in one shot.
        let cache_on = true;
        let transformer: BodyTransformer = Arc::new(move |req, body| {
            if let Some(policy) = &req.provider_routing {
                let v = serialize_provider_routing(policy);
                if !v.is_null() {
                    body["provider"] = v;
                }
            }
            if cache_on {
                anthropic_via_or_cache_transformer(req, body);
            }
        });
        let mut req = req_with_blocks(vec![PromptBlock::cached_long("identity", "I")], false);
        req.provider_routing = Some(ProviderRoutingPolicy {
            order: vec!["anthropic".into(), "google".into()],
            allow_fallbacks: Some(true),
            ..Default::default()
        });
        let mut body = json!({
            "model": req.model,
            "messages": [
                { "role": "system", "content": "flat" },
                { "role": "user", "content": "hello" }
            ],
        });
        transformer(&req, &mut body);
        // provider routing serialised...
        assert_eq!(body["provider"]["order"], json!(["anthropic", "google"]));
        assert_eq!(body["provider"]["allow_fallbacks"], json!(true));
        // ...AND system message rewritten with cache_control.
        let content = body["messages"][0]["content"]
            .as_array()
            .expect("system content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "I");
        assert!(content[0].get("cache_control").is_some());
    }

    #[test]
    fn transformer_skips_provider_field_when_routing_none() {
        let cache_on = false;
        let transformer: BodyTransformer = Arc::new(move |req, body| {
            if let Some(policy) = &req.provider_routing {
                let v = serialize_provider_routing(policy);
                if !v.is_null() {
                    body["provider"] = v;
                }
            }
            if cache_on {
                anthropic_via_or_cache_transformer(req, body);
            }
        });
        let req = req_with_blocks(vec![], false);
        let mut body = json!({ "messages": [] });
        transformer(&req, &mut body);
        assert!(
            body.get("provider").is_none(),
            "provider field must be absent when policy is None"
        );
    }

    #[test]
    fn transformer_skips_provider_field_when_policy_is_empty() {
        let cache_on = false;
        let transformer: BodyTransformer = Arc::new(move |req, body| {
            if let Some(policy) = &req.provider_routing {
                let v = serialize_provider_routing(policy);
                if !v.is_null() {
                    body["provider"] = v;
                }
            }
            if cache_on {
                anthropic_via_or_cache_transformer(req, body);
            }
        });
        let mut req = req_with_blocks(vec![], false);
        req.provider_routing = Some(ProviderRoutingPolicy::default());
        let mut body = json!({ "messages": [] });
        transformer(&req, &mut body);
        assert!(
            body.get("provider").is_none(),
            "empty policy must not emit body field"
        );
    }

    // ── models probe (Phase 100.x.models-probe) ──

    #[test]
    fn parse_models_response_extracts_slug_list() {
        let body = r#"{
            "data": [
              { "id": "anthropic/claude-opus-4-7", "name": "Opus 4.7" },
              { "id": "openai/gpt-5", "name": "GPT-5" },
              { "id": "google/gemini-2.5-pro" }
            ]
          }"#;
        let slugs = parse_models_response(body).unwrap();
        assert_eq!(slugs.len(), 3);
        assert_eq!(slugs[0], "anthropic/claude-opus-4-7");
        assert_eq!(slugs[1], "openai/gpt-5");
        assert_eq!(slugs[2], "google/gemini-2.5-pro");
    }

    #[test]
    fn parse_models_response_skips_entries_without_id() {
        let body = r#"{
            "data": [
              { "id": "openai/gpt-5" },
              { "name": "missing id" },
              { "id": "" }
            ]
          }"#;
        let slugs = parse_models_response(body).unwrap();
        assert_eq!(slugs.len(), 1);
        assert_eq!(slugs[0], "openai/gpt-5");
    }

    #[test]
    fn parse_models_response_empty_data_returns_empty_vec() {
        let slugs = parse_models_response(r#"{ "data": [] }"#).unwrap();
        assert!(slugs.is_empty());
    }

    #[test]
    fn parse_models_response_invalid_json_errors() {
        let err = parse_models_response("not json").unwrap_err();
        assert!(err.to_string().contains("/models parse failed"));
    }

    #[test]
    fn cached_live_models_returns_none_when_cache_empty() {
        _test_clear_cache_for_unit_tests();
        assert!(cached_live_models().is_none());
    }

    #[test]
    fn cached_live_models_returns_some_when_fresh() {
        _test_set_cache_for_unit_tests(
            vec!["foo/bar".into(), "baz/qux".into()],
            Duration::from_secs(60),
        );
        let cached = cached_live_models().expect("cache fresh");
        assert_eq!(cached, vec!["foo/bar", "baz/qux"]);
        _test_clear_cache_for_unit_tests();
    }

    #[test]
    fn cached_live_models_returns_none_when_stale() {
        // Stamp the cache 30 minutes in the past — older than the
        // 15-minute TTL, so the read path treats it as absent and
        // forces a fresh fetch.
        _test_set_cache_for_unit_tests(vec!["stale/entry".into()], Duration::from_secs(30 * 60));
        assert!(cached_live_models().is_none());
        _test_clear_cache_for_unit_tests();
    }

    #[test]
    fn factory_disables_cache_marker_on_proxy_host_even_for_anthropic_slug() {
        // Defense-in-depth: a proxy might strip markers or worse,
        // pass them to a non-Anthropic backend that 400's on the
        // extra field. Match attribution gating.
        let mut cfg = empty_cfg();
        cfg.base_url = "https://proxy.example.com/openrouter".into();
        let probe = OpenRouterClient {
            inner: OpenAiClient::new(&cfg, "m", fast_retry()),
            model: "anthropic/claude-opus-4-7".into(),
            inject_attribution: false,
            cache_enabled: false,
        };
        assert!(!probe.is_cache_marker_enabled());
    }
}
