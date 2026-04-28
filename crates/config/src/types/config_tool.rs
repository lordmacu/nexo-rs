//! Phase 79.10 — `ConfigTool` per-agent policy + supported-settings
//! registry.
//!
//! `ConfigToolPolicy` lives on `AgentConfig::config` and is the YAML
//! surface operators flip to enable the chat-driven self-config flow.
//! Default is `self_edit: false` (opt-in per agent).
//!
//! `SUPPORTED_SETTINGS` is the orthogonal whitelist — the list of
//! dotted paths the model MAY propose to mutate (subject to the
//! denylist in `crates/setup/src/capabilities.rs::CONFIG_SELF_EDIT_DENYLIST`).
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/ConfigTool/supportedSettings.ts:13-186`
//!     — `SUPPORTED_SETTINGS` registry shape (key, type, options,
//!     getOptions, validateOnWrite, formatOnRead). We collapse the
//!     `source: 'global' | 'settings'` distinction (leak persists in
//!     two files) into "always agent YAML" because every nexo setting
//!     lives in a single per-agent block.
//!   * `claude-code-leak/src/tools/ConfigTool/supportedSettings.ts:188-211`
//!     — `isSupported` / `getConfig` / `getAllKeys` lookup helpers.

use serde::{Deserialize, Serialize};

const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 86_400;

fn default_self_edit() -> bool {
    false
}

fn default_timeout_secs() -> u64 {
    DEFAULT_APPROVAL_TIMEOUT_SECS
}

/// YAML schema for `agents[].config:`.
///
/// Defaults applied when the operator omits the block entirely:
///   * `self_edit: false` — opt-in. The `Config` tool is not
///     registered for this agent.
///   * `allowed_paths: []` — empty list = every key in
///     [`SUPPORTED_SETTINGS`] is permitted (subject to denylist).
///   * `approval_timeout_secs: 86_400` — 24 h, mirrors the
///     `PlanModePolicy` pattern from Phase 79.1.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct ConfigToolPolicy {
    /// Master switch. The runtime registers the `Config` tool for
    /// this agent only when `true`.
    #[serde(default = "default_self_edit")]
    pub self_edit: bool,

    /// Whitelist intersection with [`SUPPORTED_SETTINGS`]. Empty =
    /// every supported key permitted. Provide an explicit list to
    /// sandbox an agent to e.g. only `model.model` + `language`.
    #[serde(default)]
    pub allowed_paths: Vec<String>,

    /// How long a `propose` may wait for operator approval before
    /// expiring. Default 24 h.
    #[serde(default = "default_timeout_secs")]
    pub approval_timeout_secs: u64,
}

impl Default for ConfigToolPolicy {
    fn default() -> Self {
        Self {
            self_edit: default_self_edit(),
            allowed_paths: Vec::new(),
            approval_timeout_secs: default_timeout_secs(),
        }
    }
}

impl ConfigToolPolicy {
    /// Whether the `Config` tool should be registered for goals on
    /// this agent.
    pub fn tool_enabled(&self) -> bool {
        self.self_edit
    }

    /// Whether `path` is permitted by this agent's whitelist
    /// intersection. Empty whitelist means "all supported".
    pub fn path_permitted(&self, path: &str) -> bool {
        if self.allowed_paths.is_empty() {
            return true;
        }
        self.allowed_paths.iter().any(|p| p == path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    String,
    Number,
    StringArray,
}

/// One whitelist entry. Renders into the dynamic tool description
/// at registration time (mirror of leak's
/// `prompt.ts:14-93::generatePrompt`).
#[derive(Debug, Clone, Copy)]
pub struct SupportedSetting {
    pub key: &'static str,
    pub kind: SettingKind,
    pub describe: &'static str,
    /// Optional finite enum of accepted string values.
    pub options: Option<&'static [&'static str]>,
    /// Optional synchronous validator. Returns `Err(message)` to
    /// refuse a value with a clear human-readable reason. Lift
    /// from leak's `validateOnWrite: async fn`
    /// (`supportedSettings.ts:23-24`); we make it sync because
    /// the MVP validators are pure shape checks.
    pub validate: Option<fn(&serde_json::Value) -> Result<(), String>>,
}

/// MVP whitelist (12 keys). Order is stable; the dynamic tool
/// description renders them in this order.
///
/// Resolution callout from spec: `dispatch_policy.mode` was
/// considered but excluded — the denylist `dispatch_policy.*` glob
/// (in `crates/setup/src/capabilities.rs::CONFIG_SELF_EDIT_DENYLIST`)
/// would block it anyway. Refining the glob to a narrower form is a
/// 79.10.b follow-up.
pub static SUPPORTED_SETTINGS: &[SupportedSetting] = &[
    SupportedSetting {
        key: "model.provider",
        kind: SettingKind::String,
        describe: "LLM provider id (anthropic, minimax, openai, gemini, ...).",
        options: None,
        validate: None,
    },
    SupportedSetting {
        key: "model.model",
        kind: SettingKind::String,
        describe: "LLM model id (provider-specific).",
        options: None,
        validate: None,
    },
    SupportedSetting {
        key: "language",
        kind: SettingKind::String,
        describe: "Reply language ISO code (e.g. `en`, `es`).",
        options: None,
        validate: None,
    },
    SupportedSetting {
        key: "system_prompt",
        kind: SettingKind::String,
        describe: "Agent system prompt (workspace file path or literal).",
        options: None,
        validate: None,
    },
    SupportedSetting {
        key: "heartbeat.enabled",
        kind: SettingKind::Bool,
        describe: "Whether heartbeat ticks fire.",
        options: None,
        validate: None,
    },
    SupportedSetting {
        key: "heartbeat.interval_secs",
        kind: SettingKind::Number,
        describe: "Seconds between heartbeat ticks.",
        options: None,
        validate: Some(positive_int),
    },
    SupportedSetting {
        key: "link_understanding.enabled",
        kind: SettingKind::Bool,
        describe: "Auto-extract link content from inbound messages.",
        options: None,
        validate: None,
    },
    SupportedSetting {
        key: "web_search.enabled",
        kind: SettingKind::Bool,
        describe: "Whether the agent may invoke the `WebSearch` tool.",
        options: None,
        validate: None,
    },
    SupportedSetting {
        key: "lsp.enabled",
        kind: SettingKind::Bool,
        describe: "Whether the `Lsp` tool is registered for this agent.",
        options: None,
        validate: None,
    },
    SupportedSetting {
        key: "lsp.languages",
        kind: SettingKind::StringArray,
        describe: "LSP language whitelist; subset of [rust, python, typescript, go]. Empty = all discovered.",
        options: None,
        validate: Some(lsp_languages),
    },
    SupportedSetting {
        key: "lsp.idle_teardown_secs",
        kind: SettingKind::Number,
        describe: "Seconds of LSP-session inactivity before teardown.",
        options: None,
        validate: Some(positive_int),
    },
    SupportedSetting {
        key: "lsp.prewarm",
        kind: SettingKind::StringArray,
        describe: "Languages to warm at boot. Subset of lsp.languages.",
        options: None,
        validate: Some(lsp_languages),
    },
];

pub fn is_supported(key: &str) -> bool {
    SUPPORTED_SETTINGS.iter().any(|s| s.key == key)
}

pub fn lookup(key: &str) -> Option<&'static SupportedSetting> {
    SUPPORTED_SETTINGS.iter().find(|s| s.key == key)
}

pub fn all_keys() -> Vec<&'static str> {
    SUPPORTED_SETTINGS.iter().map(|s| s.key).collect()
}

// ---------- Validators ----------

fn positive_int(v: &serde_json::Value) -> Result<(), String> {
    let n = v
        .as_i64()
        .ok_or_else(|| format!("expected integer, got {v}"))?;
    if n < 1 {
        return Err(format!("must be ≥ 1, got {n}"));
    }
    Ok(())
}

fn lsp_languages(v: &serde_json::Value) -> Result<(), String> {
    const VALID: &[&str] = &["rust", "python", "typescript", "go"];
    let arr = v
        .as_array()
        .ok_or_else(|| format!("expected array of strings, got {v}"))?;
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| format!("expected string, got {item}"))?;
        if !VALID.contains(&s) {
            return Err(format!(
                "unknown language `{s}`; valid: {VALID:?}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_yaml(yaml: &str) -> ConfigToolPolicy {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn default_disabled_with_empty_whitelist() {
        let p = ConfigToolPolicy::default();
        assert!(!p.self_edit);
        assert!(p.allowed_paths.is_empty());
        assert_eq!(p.approval_timeout_secs, 86_400);
    }

    #[test]
    fn yaml_roundtrip_full_block() {
        let yaml = r#"
            self_edit: true
            allowed_paths:
              - "model.model"
              - "language"
            approval_timeout_secs: 1200
        "#;
        let p = from_yaml(yaml);
        assert!(p.self_edit);
        assert_eq!(
            p.allowed_paths,
            vec!["model.model".to_string(), "language".to_string()]
        );
        assert_eq!(p.approval_timeout_secs, 1200);
    }

    #[test]
    fn empty_yaml_uses_defaults() {
        let p = from_yaml("{}");
        assert_eq!(p, ConfigToolPolicy::default());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let yaml = r#"
            self_edit: true
            mystery: 1
        "#;
        let err = serde_yaml::from_str::<ConfigToolPolicy>(yaml)
            .unwrap_err()
            .to_string();
        assert!(err.contains("mystery"), "error was: {err}");
    }

    #[test]
    fn path_permitted_empty_whitelist_allows_anything() {
        let p = ConfigToolPolicy::default();
        assert!(p.path_permitted("model.model"));
        assert!(p.path_permitted("language"));
    }

    #[test]
    fn path_permitted_explicit_whitelist_filters() {
        let p = ConfigToolPolicy {
            self_edit: true,
            allowed_paths: vec!["language".into()],
            approval_timeout_secs: 86_400,
        };
        assert!(p.path_permitted("language"));
        assert!(!p.path_permitted("model.model"));
    }

    #[test]
    fn supported_lookup_total_for_advertised_keys() {
        for key in all_keys() {
            assert!(lookup(key).is_some());
            assert!(is_supported(key));
        }
        assert!(!is_supported("unknown.key"));
    }

    #[test]
    fn lsp_languages_validator_subset_check() {
        let ok = serde_json::json!(["rust", "python"]);
        assert!(lsp_languages(&ok).is_ok());

        let bad = serde_json::json!(["rust", "kotlin"]);
        let err = lsp_languages(&bad).unwrap_err();
        assert!(err.contains("kotlin"), "got: {err}");

        let not_array = serde_json::json!("rust");
        assert!(lsp_languages(&not_array).is_err());
    }

    #[test]
    fn positive_int_validator_rejects_zero_and_negative() {
        assert!(positive_int(&serde_json::json!(1)).is_ok());
        assert!(positive_int(&serde_json::json!(86_400)).is_ok());
        assert!(positive_int(&serde_json::json!(0)).is_err());
        assert!(positive_int(&serde_json::json!(-1)).is_err());
        assert!(positive_int(&serde_json::json!("1")).is_err());
    }
}
