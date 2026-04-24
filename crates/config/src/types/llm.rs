use std::collections::HashMap;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    pub providers: HashMap<String, LlmProviderConfig>,
    #[serde(default)]
    pub retry: RetryConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LlmProviderConfig {
    pub api_key: String,
    pub base_url: String,
    pub group_id: Option<String>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    /// Optional auth override. When omitted, `api_key` is used as a
    /// static bearer token (back-compat). When present with
    /// `mode: token_plan` the client reads an OAuth bundle from
    /// `bundle` and self-refreshes on expiry.
    #[serde(default)]
    pub auth: Option<LlmAuthConfig>,
    /// Wire format the client should speak. Only meaningful for the
    /// MiniMax provider today:
    ///
    /// * `openai_compat` (default) — POST
    ///   `{base_url}/text/chatcompletion_v2` with OpenAI-shaped JSON.
    ///   Matches the public MiniMax API docs for regular API keys.
    /// * `anthropic_messages` — POST `{base_url}/v1/messages` with
    ///   Anthropic Messages JSON. Required for Coding / Token Plan
    ///   keys (OpenClaw's `minimax`/`minimax-portal` providers both
    ///   use this path via `api.minimax.io/anthropic`).
    #[serde(default)]
    pub api_flavor: Option<String>,
    /// Model ID used by `LlmClient::embed()`. Gemini has a separate
    /// embeddings model (e.g. `text-embedding-004`). When omitted the
    /// client falls back to a per-provider default or errors out.
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// Provider-specific safety / harm-category filter override.
    /// Currently only Gemini honours this — attach its
    /// `safetySettings: [...]` array verbatim.
    #[serde(default)]
    pub safety_settings: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LlmAuthConfig {
    /// `auto` picks `token_plan` when `bundle` exists on disk and falls
    /// back to `static` otherwise. `static` forces the legacy
    /// `api_key` path. `token_plan` hard-fails if the bundle is
    /// missing/unreadable.
    #[serde(default = "default_auth_mode")]
    pub mode: String,
    /// Path to the JSON bundle persisted by the setup wizard
    /// (`secrets/minimax_portal.json`). Ignored when `mode=static`.
    #[serde(default)]
    pub bundle: Option<String>,
}

fn default_auth_mode() -> String {
    "auto".to_string()
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    #[serde(default = "default_rps")]
    pub requests_per_second: f32,
    pub quota_alert_threshold: Option<u64>,
}

fn default_rps() -> f32 { 2.0 }

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_initial_backoff")]
    pub initial_backoff_ms: u64,
    #[serde(default = "default_max_backoff")]
    pub max_backoff_ms: u64,
    #[serde(default = "default_multiplier")]
    pub backoff_multiplier: f32,
}

fn default_max_attempts() -> u32 { 5 }
fn default_initial_backoff() -> u64 { 1000 }
fn default_max_backoff() -> u64 { 60_000 }
fn default_multiplier() -> f32 { 2.0 }
