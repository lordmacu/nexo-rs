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
    /// Phase 83.8.12.5 — per-tenant LLM provider sub-namespaces.
    /// SaaS deployments isolate one tenant's API keys + endpoints
    /// from another's; agents resolve their provider via
    /// `LlmConfig::resolve_provider(tenant_id, name)` which checks
    /// `tenants.<id>.providers.<name>` first, then falls back to
    /// the global `providers.<name>`. Single-tenant deployments
    /// leave this empty and behaviour is unchanged.
    #[serde(default)]
    pub tenants: HashMap<String, TenantLlmConfig>,
}

/// Phase 83.8.12.5 — per-tenant LLM config under
/// `llm.yaml.tenants.<tenant_id>`. Currently only carries
/// providers; future tenant-scoped knobs (rate limits, retry
/// overrides) plug in additively.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TenantLlmConfig {
    /// Tenant-scoped provider table. Same shape as the global
    /// `LlmConfig.providers` so an operator can copy-paste a
    /// provider def into a tenant block without changes.
    #[serde(default)]
    pub providers: HashMap<String, LlmProviderConfig>,
}

impl LlmConfig {
    /// Phase 83.8.12.5 — resolve a provider with tenant-first /
    /// global-fallback semantics. Returns the tenant-scoped
    /// definition when present, falling back to the global
    /// `providers` table; `None` when neither exists.
    ///
    /// Defense-in-depth: passing a `tenant_id` that does not
    /// exist in `tenants` falls through to the global table —
    /// the alternative ("strict tenant isolation, refuse if
    /// tenant has no providers") was rejected because most
    /// providers are operator-shared (e.g. an operator's own
    /// MiniMax key) and forcing every tenant to redeclare them
    /// makes the YAML noisy. Tenants that need strict
    /// isolation declare an empty `providers: {}` block under
    /// their tenant key — that wins over the fallback.
    ///
    /// Wait: `providers: {}` empty would still fall through
    /// because `get(name)` on an empty map returns `None`. To
    /// truly isolate, an operator can put a sentinel provider
    /// `{name: "_blocked", ...}` per tenant. Practical default
    /// is: tenants supplement, not replace, the global pool.
    pub fn resolve_provider(
        &self,
        tenant_id: Option<&str>,
        name: &str,
    ) -> Option<&LlmProviderConfig> {
        if let Some(tid) = tenant_id {
            if let Some(t) = self.tenants.get(tid) {
                if let Some(p) = t.providers.get(name) {
                    return Some(p);
                }
            }
        }
        self.providers.get(name)
    }
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
    #[serde(default)]
    pub micro: MicroCompactionConfig,
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
    /// Phase 77.2 — auto-compact token + age triggers. `None` disables
    /// the age trigger and falls back to the legacy `compact_at_pct`.
    #[serde(default)]
    pub auto: Option<AutoCompactionConfig>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            micro: MicroCompactionConfig::default(),
            compact_at_pct: 0.75,
            tail_keep_tokens: 20_000,
            tool_result_max_pct: 0.30,
            summarizer_model: String::new(),
            lock_ttl_seconds: 300,
            auto: None,
        }
    }
}

/// Phase 77.2 — auto-compact trigger knobs.
///
/// When `auto` is `Some`, both token-percentage and session-age triggers
/// are armed. `token_pct` replaces the legacy `compact_at_pct` (if you
/// set both, `token_pct` wins). `max_age_minutes` is the only age
/// trigger — there is no separate "gap since last message" trigger.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AutoCompactionConfig {
    /// Fire when estimated tokens >= `token_pct * context_window`.
    /// 0.0 disables the token trigger (age-only).
    #[serde(default = "default_auto_token_pct")]
    pub token_pct: f32,
    /// Fire when `Utc::now() - session.created_at >= max_age_minutes`.
    /// 0 disables the age trigger (token-only).
    #[serde(default = "default_auto_max_age_minutes")]
    pub max_age_minutes: u64,
    /// Safety margin below the effective context window (context_window
    /// minus max-output-tokens). Mirrors the leak's 13 000 token buffer.
    #[serde(default = "default_auto_buffer_tokens")]
    pub buffer_tokens: u64,
    /// Minimum turns between consecutive auto-compactions. Anti-storm.
    #[serde(default = "default_auto_min_turns_between")]
    pub min_turns_between: u32,
    /// Consecutive compaction failures that trip the circuit breaker.
    #[serde(default = "default_auto_max_consecutive_failures")]
    pub max_consecutive_failures: u32,
}

impl Default for AutoCompactionConfig {
    fn default() -> Self {
        Self {
            token_pct: default_auto_token_pct(),
            max_age_minutes: default_auto_max_age_minutes(),
            buffer_tokens: default_auto_buffer_tokens(),
            min_turns_between: default_auto_min_turns_between(),
            max_consecutive_failures: default_auto_max_consecutive_failures(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct MicroCompactionConfig {
    #[serde(default = "default_micro_threshold_bytes")]
    pub threshold_bytes: usize,
    /// Empty = reuse `summarizer_model`; if that is empty, reuse the
    /// agent's current model on the already-wired provider.
    #[serde(default)]
    pub provider: String,
    #[serde(default = "default_micro_summary_max_chars")]
    pub summary_max_chars: usize,
}

impl Default for MicroCompactionConfig {
    fn default() -> Self {
        Self {
            threshold_bytes: default_micro_threshold_bytes(),
            provider: String::new(),
            summary_max_chars: default_micro_summary_max_chars(),
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
fn default_micro_threshold_bytes() -> usize {
    16 * 1024
}
fn default_micro_summary_max_chars() -> usize {
    2048
}
fn default_lock_ttl_seconds() -> u32 {
    300
}
fn default_auto_token_pct() -> f32 {
    0.80
}
fn default_auto_max_age_minutes() -> u64 {
    120
}
fn default_auto_buffer_tokens() -> u64 {
    13_000
}
fn default_auto_min_turns_between() -> u32 {
    5
}
fn default_auto_max_consecutive_failures() -> u32 {
    3
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

/// Per-agent override of the four `context_optimization` enables.
/// Lives under `agents.<id>.context_optimization` in `agents.yaml`.
/// Only the booleans are overridable per-agent — operators rarely
/// flip the numeric knobs (`compact_at_pct`, `tail_keep_tokens`)
/// per agent, so those stay global to keep the surface narrow.
/// `None` on a field means "inherit from llm.context_optimization".
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentContextOptimizationOverride {
    #[serde(default)]
    pub prompt_cache: Option<bool>,
    #[serde(default)]
    pub compaction: Option<bool>,
    #[serde(default)]
    pub token_counter: Option<bool>,
    #[serde(default)]
    pub workspace_cache: Option<bool>,
}

/// Snapshot of the four kill-switches resolved against an agent's
/// override + the global config. Cheap to copy; the runtime takes a
/// fresh one per turn so a hot-reload that swaps `RuntimeSnapshot`
/// (Phase 18) is observed on the next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedContextOptimization {
    pub prompt_cache: bool,
    pub compaction: bool,
    pub token_counter: bool,
    pub workspace_cache: bool,
}

impl ResolvedContextOptimization {
    /// Apply the precedence rule: a `Some(b)` on the override wins
    /// over the global flag. `None` (default) inherits.
    pub fn resolve(
        global: &ContextOptimizationConfig,
        agent_override: Option<&AgentContextOptimizationOverride>,
    ) -> Self {
        let pick = |sub: Option<bool>, g: bool| sub.unwrap_or(g);
        Self {
            prompt_cache: pick(
                agent_override.and_then(|o| o.prompt_cache),
                global.prompt_cache.enabled,
            ),
            compaction: pick(
                agent_override.and_then(|o| o.compaction),
                global.compaction.enabled,
            ),
            token_counter: pick(
                agent_override.and_then(|o| o.token_counter),
                global.token_counter.enabled,
            ),
            workspace_cache: pick(
                agent_override.and_then(|o| o.workspace_cache),
                global.workspace_cache.enabled,
            ),
        }
    }
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

    // ── Phase 83.8.12.5 — resolve_provider semantics ──

    fn provider(api_key: &str) -> LlmProviderConfig {
        let yaml = format!(
            "api_key: {api_key}\nbase_url: https://api.example.com\n"
        );
        serde_yaml::from_str(&yaml).unwrap()
    }

    fn cfg_with(global: &[(&str, &str)], tenants: &[(&str, &[(&str, &str)])]) -> LlmConfig {
        let providers: HashMap<String, LlmProviderConfig> = global
            .iter()
            .map(|(k, v)| (k.to_string(), provider(v)))
            .collect();
        let tenants_map: HashMap<String, TenantLlmConfig> = tenants
            .iter()
            .map(|(tid, plist)| {
                let providers: HashMap<String, LlmProviderConfig> = plist
                    .iter()
                    .map(|(k, v)| (k.to_string(), provider(v)))
                    .collect();
                (tid.to_string(), TenantLlmConfig { providers })
            })
            .collect();
        LlmConfig {
            providers,
            retry: RetryConfig {
                max_attempts: 1,
                initial_backoff_ms: 1,
                max_backoff_ms: 1,
                backoff_multiplier: 1.0,
            },
            context_optimization: ContextOptimizationConfig::default(),
            tenants: tenants_map,
        }
    }

    #[test]
    fn resolve_provider_tenant_scoped_present_returns_tenant_version() {
        let cfg = cfg_with(
            &[("openai", "global-key")],
            &[("acme", &[("openai", "tenant-key")])],
        );
        let p = cfg.resolve_provider(Some("acme"), "openai").unwrap();
        assert_eq!(p.api_key, "tenant-key");
    }

    #[test]
    fn resolve_provider_tenant_scoped_missing_falls_back_to_global() {
        let cfg = cfg_with(&[("openai", "global-key")], &[("acme", &[])]);
        let p = cfg.resolve_provider(Some("acme"), "openai").unwrap();
        assert_eq!(p.api_key, "global-key");
    }

    #[test]
    fn resolve_provider_no_tenant_id_uses_only_global() {
        let cfg = cfg_with(
            &[("openai", "global-key")],
            &[("acme", &[("openai", "tenant-key")])],
        );
        let p = cfg.resolve_provider(None, "openai").unwrap();
        assert_eq!(p.api_key, "global-key");
    }

    #[test]
    fn resolve_provider_unknown_tenant_falls_through_to_global() {
        let cfg = cfg_with(
            &[("openai", "global-key")],
            &[("acme", &[("openai", "tenant-key")])],
        );
        // `globex` tenant doesn't exist in config → fall through.
        let p = cfg.resolve_provider(Some("globex"), "openai").unwrap();
        assert_eq!(p.api_key, "global-key");
    }

    #[test]
    fn resolve_provider_both_missing_returns_none() {
        let cfg = cfg_with(&[], &[("acme", &[])]);
        assert!(cfg.resolve_provider(Some("acme"), "openai").is_none());
        assert!(cfg.resolve_provider(None, "openai").is_none());
    }

    #[test]
    fn tenants_field_defaults_to_empty_when_absent_from_yaml() {
        let yaml = r#"
providers:
  openai:
    api_key: x
    base_url: https://api.openai.com
"#;
        let cfg: LlmConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.tenants.is_empty());
        assert!(cfg.providers.contains_key("openai"));
    }

    #[test]
    fn tenants_block_round_trips_from_yaml() {
        let yaml = r#"
providers:
  openai:
    api_key: global
    base_url: https://api.openai.com
tenants:
  acme:
    providers:
      openai:
        api_key: acme-key
        base_url: https://api.openai.com
"#;
        let cfg: LlmConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.tenants.len(), 1);
        let acme = cfg.tenants.get("acme").unwrap();
        assert_eq!(acme.providers["openai"].api_key, "acme-key");
        // Resolve respects precedence.
        assert_eq!(
            cfg.resolve_provider(Some("acme"), "openai").unwrap().api_key,
            "acme-key"
        );
        assert_eq!(
            cfg.resolve_provider(None, "openai").unwrap().api_key,
            "global"
        );
    }

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
        assert!(
            !cfg.compaction.enabled,
            "compaction off by default (rollout)"
        );
        assert!(cfg.token_counter.enabled, "token_counter safe by default");
        assert!(
            cfg.workspace_cache.enabled,
            "workspace_cache safe by default"
        );
        assert_eq!(cfg.compaction.compact_at_pct, 0.75);
        assert_eq!(cfg.compaction.micro.threshold_bytes, 16 * 1024);
        assert_eq!(cfg.compaction.micro.provider, "");
        assert_eq!(cfg.compaction.micro.summary_max_chars, 2048);
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
    micro:
      threshold_bytes: 4096
      provider: "claude-haiku-4-5"
      summary_max_chars: 1024
    compact_at_pct: 0.6
    tail_keep_tokens: 15000
    summarizer_model: "claude-haiku-4-5"
"#;
        let cfg: LlmConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(!cfg.context_optimization.prompt_cache.enabled);
        assert!(cfg.context_optimization.compaction.enabled);
        assert_eq!(
            cfg.context_optimization.compaction.micro.threshold_bytes,
            4096
        );
        assert_eq!(
            cfg.context_optimization.compaction.micro.provider,
            "claude-haiku-4-5"
        );
        assert_eq!(
            cfg.context_optimization.compaction.micro.summary_max_chars,
            1024
        );
        assert_eq!(cfg.context_optimization.compaction.compact_at_pct, 0.6);
        assert_eq!(cfg.context_optimization.compaction.tail_keep_tokens, 15_000);
        // Non-overridden fields take defaults.
        assert_eq!(
            cfg.context_optimization.compaction.tool_result_max_pct,
            0.30
        );
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
    fn agent_override_resolves_with_precedence() {
        let global = ContextOptimizationConfig::default();
        // Default: prompt_cache=true, compaction=false, token=true, ws=true.
        let none = ResolvedContextOptimization::resolve(&global, None);
        assert!(none.prompt_cache);
        assert!(!none.compaction);
        assert!(none.token_counter);
        assert!(none.workspace_cache);
        // Per-agent: opt INTO compaction, opt OUT of cache.
        let ov = AgentContextOptimizationOverride {
            prompt_cache: Some(false),
            compaction: Some(true),
            token_counter: None,
            workspace_cache: None,
        };
        let resolved = ResolvedContextOptimization::resolve(&global, Some(&ov));
        assert!(!resolved.prompt_cache);
        assert!(resolved.compaction);
        assert!(resolved.token_counter); // inherited
        assert!(resolved.workspace_cache); // inherited
    }

    #[test]
    fn agent_override_yaml_round_trip() {
        let yaml = r#"
prompt_cache: false
compaction: true
"#;
        let ov: AgentContextOptimizationOverride = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(ov.prompt_cache, Some(false));
        assert_eq!(ov.compaction, Some(true));
        assert!(ov.token_counter.is_none());
        assert!(ov.workspace_cache.is_none());
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

    #[test]
    fn auto_config_deserializes_with_defaults() {
        let yaml = r#"
enabled: true
auto: {}
"#;
        let cfg: CompactionConfig = serde_yaml::from_str(yaml).unwrap();
        let auto = cfg.auto.unwrap();
        assert!((auto.token_pct - 0.80).abs() < f32::EPSILON);
        assert_eq!(auto.max_age_minutes, 120);
        assert_eq!(auto.buffer_tokens, 13_000);
        assert_eq!(auto.min_turns_between, 5);
        assert_eq!(auto.max_consecutive_failures, 3);
    }

    #[test]
    fn auto_config_none_when_missing() {
        let yaml = r#"
enabled: false
"#;
        let cfg: CompactionConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.auto.is_none());
    }

    #[test]
    fn auto_config_full_override_parses() {
        let yaml = r#"
enabled: true
auto:
  token_pct: 0.90
  max_age_minutes: 240
  buffer_tokens: 8000
  min_turns_between: 3
  max_consecutive_failures: 2
"#;
        let cfg: CompactionConfig = serde_yaml::from_str(yaml).unwrap();
        let auto = cfg.auto.unwrap();
        assert!((auto.token_pct - 0.90).abs() < f32::EPSILON);
        assert_eq!(auto.max_age_minutes, 240);
        assert_eq!(auto.buffer_tokens, 8000);
        assert_eq!(auto.min_turns_between, 3);
        assert_eq!(auto.max_consecutive_failures, 2);
    }

    #[test]
    fn auto_config_token_only() {
        let yaml = r#"
enabled: true
auto:
  token_pct: 0.85
  max_age_minutes: 0
"#;
        let cfg: CompactionConfig = serde_yaml::from_str(yaml).unwrap();
        let auto = cfg.auto.unwrap();
        assert!((auto.token_pct - 0.85).abs() < f32::EPSILON);
        assert_eq!(auto.max_age_minutes, 0);
    }

    #[test]
    fn auto_config_age_only() {
        let yaml = r#"
enabled: true
auto:
  token_pct: 0.0
  max_age_minutes: 60
"#;
        let cfg: CompactionConfig = serde_yaml::from_str(yaml).unwrap();
        let auto = cfg.auto.unwrap();
        assert!((auto.token_pct - 0.0).abs() < f32::EPSILON);
        assert_eq!(auto.max_age_minutes, 60);
    }
}
