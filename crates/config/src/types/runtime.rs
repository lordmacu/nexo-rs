//! Runtime-level knobs for the agent process: hot-reload settings plus
//! cron-runner policy. Loaded from `config/runtime.yaml` when present;
//! an absent file yields defaults (`reload.enabled=true`,
//! `reload.debounce_ms=500`, one-shot cron retries enabled).

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub reload: RuntimeReloadConfig,
    #[serde(default)]
    pub migrations: RuntimeMigrationsConfig,
    #[serde(default)]
    pub cron: RuntimeCronConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMigrationsConfig {
    #[serde(default)]
    pub auto_apply: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReloadConfig {
    /// Enable the file watcher + broker `control.reload` listener.
    /// `false` turns off automatic reload; operators can still force a
    /// reload via the `agent reload` CLI (which publishes on the
    /// broker topic).
    #[serde(default = "default_reload_enabled")]
    pub enabled: bool,
    /// Debounce window fed to `notify-debouncer-full`. Atomic-save
    /// editors (vim, VSCode) generate several filesystem events per
    /// logical write; the debouncer coalesces them so we only fire
    /// one reload.
    #[serde(default = "default_reload_debounce_ms")]
    pub debounce_ms: u64,
    /// Extra paths (relative to the config directory) to watch in
    /// addition to the built-in set (`agents.yaml`, `agents.d/`,
    /// `llm.yaml`). Empty = defaults only.
    #[serde(default)]
    pub extra_watch_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCronConfig {
    #[serde(default)]
    pub one_shot_retry: RuntimeCronOneShotRetryConfig,
    /// Opt-in tool-call execution in cron LLM dispatcher.
    #[serde(default)]
    pub tool_calls: RuntimeCronToolCallsConfig,
}

impl Default for RuntimeCronConfig {
    fn default() -> Self {
        Self {
            one_shot_retry: RuntimeCronOneShotRetryConfig::default(),
            tool_calls: RuntimeCronToolCallsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCronToolCallsConfig {
    /// When false (default), cron LLM responses with tool-calls are
    /// logged as text only and no tool is executed.
    #[serde(default)]
    pub enabled: bool,
    /// Hard cap on tool-call/LLM iterations per fire when tool-call
    /// execution is enabled.
    #[serde(default = "default_cron_tool_calls_max_iterations")]
    pub max_iterations: usize,
    /// Extra process-level allowlist (glob syntax like `allowed_tools`).
    /// Empty = no extra narrowing beyond the binding effective policy.
    #[serde(default)]
    pub allowlist: Vec<String>,
}

impl Default for RuntimeCronToolCallsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_iterations: default_cron_tool_calls_max_iterations(),
            allowlist: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCronOneShotRetryConfig {
    /// Maximum number of retry attempts after a one-shot dispatch
    /// failure before the entry is dropped. `0` keeps the historical
    /// at-most-once behavior (delete on first failure).
    #[serde(default = "default_cron_one_shot_max_retries")]
    pub max_retries: u32,
    /// Base delay (seconds) for attempt #1. Later attempts use
    /// exponential backoff (x2, capped by `max_backoff_secs`).
    #[serde(default = "default_cron_one_shot_base_backoff_secs")]
    pub base_backoff_secs: u64,
    /// Upper bound for the exponential retry delay.
    #[serde(default = "default_cron_one_shot_max_backoff_secs")]
    pub max_backoff_secs: u64,
}

impl Default for RuntimeCronOneShotRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_cron_one_shot_max_retries(),
            base_backoff_secs: default_cron_one_shot_base_backoff_secs(),
            max_backoff_secs: default_cron_one_shot_max_backoff_secs(),
        }
    }
}

impl Default for RuntimeReloadConfig {
    fn default() -> Self {
        Self {
            enabled: default_reload_enabled(),
            debounce_ms: default_reload_debounce_ms(),
            extra_watch_paths: Vec::new(),
        }
    }
}

fn default_reload_enabled() -> bool {
    true
}

fn default_reload_debounce_ms() -> u64 {
    500
}

fn default_cron_one_shot_max_retries() -> u32 {
    3
}

fn default_cron_one_shot_base_backoff_secs() -> u64 {
    30
}

fn default_cron_one_shot_max_backoff_secs() -> u64 {
    1800
}

fn default_cron_tool_calls_max_iterations() -> usize {
    6
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> RuntimeConfig {
        serde_yaml::from_str(yaml).expect("valid runtime.yaml")
    }

    #[test]
    fn absent_file_uses_defaults() {
        let cfg = RuntimeConfig::default();
        assert!(cfg.reload.enabled);
        assert_eq!(cfg.reload.debounce_ms, 500);
        assert!(cfg.reload.extra_watch_paths.is_empty());
        assert!(!cfg.migrations.auto_apply);
        assert_eq!(cfg.cron.one_shot_retry.max_retries, 3);
        assert_eq!(cfg.cron.one_shot_retry.base_backoff_secs, 30);
        assert_eq!(cfg.cron.one_shot_retry.max_backoff_secs, 1800);
        assert!(!cfg.cron.tool_calls.enabled);
        assert_eq!(cfg.cron.tool_calls.max_iterations, 6);
        assert!(cfg.cron.tool_calls.allowlist.is_empty());
    }

    #[test]
    fn empty_body_uses_defaults() {
        let cfg = parse("reload: {}\n");
        assert!(cfg.reload.enabled);
        assert_eq!(cfg.reload.debounce_ms, 500);
        assert!(!cfg.migrations.auto_apply);
        assert_eq!(cfg.cron.one_shot_retry.max_retries, 3);
        assert!(!cfg.cron.tool_calls.enabled);
    }

    #[test]
    fn custom_values_round_trip() {
        let cfg = parse(
            r#"
reload:
  enabled: false
  debounce_ms: 1000
  extra_watch_paths:
    - custom.yaml
migrations:
  auto_apply: true
cron:
  one_shot_retry:
    max_retries: 5
    base_backoff_secs: 10
    max_backoff_secs: 300
  tool_calls:
    enabled: true
    max_iterations: 4
    allowlist:
      - email_*
      - memory_recall
"#,
        );
        assert!(!cfg.reload.enabled);
        assert_eq!(cfg.reload.debounce_ms, 1000);
        assert_eq!(
            cfg.reload.extra_watch_paths,
            vec!["custom.yaml".to_string()]
        );
        assert!(cfg.migrations.auto_apply);
        assert_eq!(cfg.cron.one_shot_retry.max_retries, 5);
        assert_eq!(cfg.cron.one_shot_retry.base_backoff_secs, 10);
        assert_eq!(cfg.cron.one_shot_retry.max_backoff_secs, 300);
        assert!(cfg.cron.tool_calls.enabled);
        assert_eq!(cfg.cron.tool_calls.max_iterations, 4);
        assert_eq!(
            cfg.cron.tool_calls.allowlist,
            vec!["email_*".to_string(), "memory_recall".to_string()]
        );
    }

    #[test]
    fn unknown_field_rejected() {
        let err = serde_yaml::from_str::<RuntimeConfig>("reload:\n  bogus: 1\n")
            .expect_err("deny_unknown_fields rejects typos");
        assert!(err.to_string().contains("bogus"));
    }
}
