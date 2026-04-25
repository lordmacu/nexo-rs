use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    pub providers: HashMap<String, LlmProviderConfig>,
    #[serde(default)]
    pub retry: RetryConfig,
    /// Per-request input-token reduction knobs. All four mechanisms are
    /// independent: prompt_cache and workspace_cache default on (safe),
    /// compaction defaults off (rollout gradual), token_counter on.
    #[serde(default)]
    pub context_optimization: ContextOptimizationConfig,
}

/// Aggregated kill-switches and tunables for the four context-size
/// optimization mechanisms. Each subsystem can be flipped independently
/// without recompiling. Per-agent overrides live in `AgentConfig`.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ContextOptimizationConfig {
    #[serde(default)]
    pub prompt_cache: PromptCacheConfig,
    #[serde(default)]
    pub compaction: CompactionConfig,
    #[serde(default)]
    pub token_counter: TokenCounterConfig,
    #[serde(default)]
    pub workspace_cache: WorkspaceCacheConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PromptCacheConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Providers that get the 1h TTL marker on stable blocks. Others
    /// fall back to the short (5min) TTL.
    #[serde(default = "default_long_ttl_providers")]
    pub long_ttl_providers: Vec<String>,
    /// Override of the `anthropic-beta` header. Anthropic occasionally
    /// promotes betas — operators can swap the header without a release.
    #[serde(default)]
    pub beta_header: Option<String>,
}

impl Default for PromptCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            long_ttl_providers: default_long_ttl_providers(),
            beta_header: None,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CompactionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_compact_at_pct")]
    pub compact_at_pct: f32,
    #[serde(default = "default_tail_keep_tokens")]
    pub tail_keep_tokens: u32,
    #[serde(default = "default_tool_result_max_pct")]
    pub tool_result_max_pct: f32,
    /// Empty = reuse the agent's main model for the summary call.
    #[serde(default)]
    pub summarizer_model: String,
    #[serde(default = "default_lock_ttl_seconds")]
    pub lock_ttl_seconds: u32,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            compact_at_pct: 0.75,
            tail_keep_tokens: 20_000,
            tool_result_max_pct: 0.30,
            summarizer_model: String::new(),
            lock_ttl_seconds: 300,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TokenCounterConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `auto` picks `anthropic_api` when provider is anthropic and an
    /// API key is available, else `tiktoken`.
    #[serde(default = "default_token_counter_backend")]
    pub backend: String,
    #[serde(default = "default_token_cache_capacity")]
    pub cache_capacity: u32,
}

impl Default for TokenCounterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: default_token_counter_backend(),
            cache_capacity: default_token_cache_capacity(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCacheConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_watch_debounce_ms")]
    pub watch_debounce_ms: u32,
    /// Optional absolute TTL for filesystems where notify(7) drops
    /// events (NFS, FUSE). 0 = disabled (default).
    #[serde(default)]
    pub max_age_seconds: u32,
}

impl Default for WorkspaceCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            watch_debounce_ms: default_watch_debounce_ms(),
            max_age_seconds: 0,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_long_ttl_providers() -> Vec<String> {
    vec!["anthropic".to_string(), "vertex".to_string()]
}
fn default_compact_at_pct() -> f32 {
    0.75
}
fn default_tail_keep_tokens() -> u32 {
    20_000
}
fn default_tool_result_max_pct() -> f32 {
    0.30
}
fn default_lock_ttl_seconds() -> u32 {
    300
}
fn default_token_counter_backend() -> String {
    "auto".to_string()
}
fn default_token_cache_capacity() -> u32 {
    1024
}
fn default_watch_debounce_ms() -> u32 {
    500
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
    fn context_optimization_defaults_match_spec() {
        let cfg = ContextOptimizationConfig::default();
        assert!(cfg.prompt_cache.enabled, "prompt_cache safe by default");
        assert!(!cfg.compaction.enabled, "compaction off by default (rollout)");
        assert!(cfg.token_counter.enabled, "token_counter safe by default");
        assert!(cfg.workspace_cache.enabled, "workspace_cache safe by default");
        assert_eq!(cfg.compaction.compact_at_pct, 0.75);
        assert_eq!(cfg.compaction.tail_keep_tokens, 20_000);
        assert_eq!(cfg.compaction.tool_result_max_pct, 0.30);
        assert_eq!(cfg.token_counter.backend, "auto");
        assert_eq!(cfg.token_counter.cache_capacity, 1024);
        assert_eq!(cfg.workspace_cache.watch_debounce_ms, 500);
        assert_eq!(
            cfg.prompt_cache.long_ttl_providers,
            vec!["anthropic".to_string(), "vertex".to_string()]
        );
    }

    #[test]
    fn context_optimization_yaml_round_trip() {
        let yaml = r#"
providers: {}
context_optimization:
  prompt_cache:
    enabled: false
    long_ttl_providers: ["anthropic"]
  compaction:
    enabled: true
    compact_at_pct: 0.6
    tail_keep_tokens: 15000
    summarizer_model: "claude-haiku-4-5"
"#;
        let cfg: LlmConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(!cfg.context_optimization.prompt_cache.enabled);
        assert!(cfg.context_optimization.compaction.enabled);
        assert_eq!(cfg.context_optimization.compaction.compact_at_pct, 0.6);
        assert_eq!(cfg.context_optimization.compaction.tail_keep_tokens, 15_000);
        // Non-overridden fields take defaults.
        assert_eq!(cfg.context_optimization.compaction.tool_result_max_pct, 0.30);
        assert!(cfg.context_optimization.token_counter.enabled);
    }

    #[test]
    fn context_optimization_section_is_optional() {
        let yaml = r#"
providers: {}
"#;
        let cfg: LlmConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.context_optimization.prompt_cache.enabled);
        assert!(!cfg.context_optimization.compaction.enabled);
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
