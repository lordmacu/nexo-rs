use serde::Deserialize;
use std::collections::HashMap;

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
    /// Path to a setup-token file (Anthropic `sk-ant-oat01-...`).
    /// Used when `mode = setup_token`. The file contents are loaded
    /// verbatim and sent as a Bearer token.
    #[serde(default)]
    pub setup_token_file: Option<String>,
    /// OAuth token endpoint for `mode = oauth_bundle` / `cli_import`.
    /// Falls back to Anthropic's default when empty.
    #[serde(default)]
    pub refresh_endpoint: Option<String>,
    /// OAuth client_id for the refresh grant. Not a secret — matches
    /// the public Claude Code CLI client_id when omitted.
    #[serde(default)]
    pub client_id: Option<String>,
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

fn default_rps() -> f32 {
    2.0
}

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

fn default_max_attempts() -> u32 {
    5
}
fn default_initial_backoff() -> u64 {
    1000
}
fn default_max_backoff() -> u64 {
    60_000
}
fn default_multiplier() -> f32 {
    2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_anthropic_full_shape_parses() {
        let yaml = r#"
api_key: ""
base_url: https://api.anthropic.com
auth:
  mode: oauth_bundle
  bundle: ./secrets/anthropic_oauth.json
  setup_token_file: ./secrets/anthropic_setup_token.txt
  refresh_endpoint: https://console.anthropic.com/v1/oauth/token
  client_id: 9d1c250a-e61b-44d9-88ed-5944d1962f5e
"#;
        let cfg: LlmProviderConfig = serde_yaml::from_str(yaml).unwrap();
        let a = cfg.auth.unwrap();
        assert_eq!(a.mode, "oauth_bundle");
        assert_eq!(a.bundle.as_deref(), Some("./secrets/anthropic_oauth.json"));
        assert_eq!(
            a.setup_token_file.as_deref(),
            Some("./secrets/anthropic_setup_token.txt")
        );
        assert_eq!(
            a.refresh_endpoint.as_deref(),
            Some("https://console.anthropic.com/v1/oauth/token")
        );
        assert!(a.client_id.is_some());
    }

    #[test]
    fn auth_defaults_to_auto_with_optional_fields_absent() {
        let yaml = r#"
api_key: key
base_url: ""
auth:
  bundle: ./x.json
"#;
        let cfg: LlmProviderConfig = serde_yaml::from_str(yaml).unwrap();
        let a = cfg.auth.unwrap();
        assert_eq!(a.mode, "auto");
        assert!(a.setup_token_file.is_none());
        assert!(a.refresh_endpoint.is_none());
        assert!(a.client_id.is_none());
    }

    #[test]
    fn auth_section_is_optional() {
        let yaml = r#"
api_key: sk
base_url: ""
"#;
        let cfg: LlmProviderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.auth.is_none());
    }
}
