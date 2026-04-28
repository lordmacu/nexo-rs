//! Phase 79.5 — per-binding `lsp:` policy.
//!
//! YAML schema operators use to opt-in to LSP per binding. Disabled
//! by default — the `Lsp` tool is only registered for bindings
//! that explicitly turn it on (`lsp.enabled: true`).
//!
//! Cross-reference:
//!   * `claude-code-leak/src/services/lsp/manager.ts:100-110`
//!     `isLspConnected()` gates the leak's tool catalogue. We use
//!     `enabled` to gate registration AND a per-binding language
//!     whitelist on top.
//!   * Spec: `proyecto/PHASES.md::79.5` — `lsp.enabled` +
//!     `lsp.languages` + `lsp.prewarm` + `lsp.idle_teardown_secs`.

use serde::{Deserialize, Serialize};

/// Wire representation of an LSP language. Kept distinct from
/// `nexo_lsp::LspLanguage` so this crate does not need a dep on
/// `nexo-lsp`. The runtime translates one to the other in
/// `nexo-core::agent::lsp_tool` via [`LspLanguageWire::matrix_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LspLanguageWire {
    Rust,
    Python,
    TypeScript,
    Go,
}

impl LspLanguageWire {
    /// Stable lowercase name matching the matrix (`rust`, `python`,
    /// etc.). The runtime cross-walks against this string.
    pub fn matrix_name(self) -> &'static str {
        match self {
            LspLanguageWire::Rust => "rust",
            LspLanguageWire::Python => "python",
            LspLanguageWire::TypeScript => "typescript",
            LspLanguageWire::Go => "go",
        }
    }

    /// Iterate all variants in matrix order.
    pub fn all() -> &'static [LspLanguageWire] {
        &[
            LspLanguageWire::Rust,
            LspLanguageWire::Python,
            LspLanguageWire::TypeScript,
            LspLanguageWire::Go,
        ]
    }
}

const DEFAULT_IDLE_TEARDOWN_SECS: u64 = 600;

fn default_lsp_enabled() -> bool {
    false
}

fn default_idle_secs() -> u64 {
    DEFAULT_IDLE_TEARDOWN_SECS
}

fn default_prewarm() -> Vec<LspLanguageWire> {
    // We're a Rust shop — pre-warm rust-analyzer when the policy
    // is enabled so the first call is not a 500 ms cold start.
    vec![LspLanguageWire::Rust]
}

/// Per-binding LSP policy.
///
/// Defaults applied when the operator omits the block entirely:
///   * `enabled: false` — opt-in. The `Lsp` tool is not even
///     registered for this binding.
///   * `languages: []` — empty whitelist means "all discovered
///     languages permitted" once `enabled` is true.
///   * `prewarm: [rust]` — rust-analyzer warmed at boot when
///     enabled.
///   * `idle_teardown_secs: 600` — 10 min.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LspPolicy {
    /// Master switch. The runtime registers the `Lsp` tool for
    /// this binding only when `enabled = true`.
    #[serde(default = "default_lsp_enabled")]
    pub enabled: bool,

    /// Whitelist of languages this binding may invoke. Empty list
    /// = all languages the launcher discovered. Use this to
    /// sandbox a binding to a single language (e.g. `email-bot`
    /// gets `[python]` only).
    #[serde(default)]
    pub languages: Vec<LspLanguageWire>,

    /// Languages to warm at boot when this policy is `enabled`.
    /// Empty = no pre-warm (lazy-on-first-call). Default `[rust]`
    /// reflects our workspace.
    #[serde(default = "default_prewarm")]
    pub prewarm: Vec<LspLanguageWire>,

    /// Seconds of inactivity before the manager tears down a
    /// session. Default 600 (10 min). Phase 19/20 synthetic
    /// pollers do not reset this counter.
    #[serde(default = "default_idle_secs")]
    pub idle_teardown_secs: u64,
}

impl Default for LspPolicy {
    fn default() -> Self {
        LspPolicy {
            enabled: default_lsp_enabled(),
            languages: Vec::new(),
            prewarm: default_prewarm(),
            idle_teardown_secs: default_idle_secs(),
        }
    }
}

impl LspPolicy {
    /// Whether the `Lsp` tool should be registered for goals on
    /// this binding. The runtime calls this when computing the
    /// per-binding tool catalogue.
    pub fn tool_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether the given language is permitted by this binding's
    /// whitelist. An empty whitelist permits every discovered
    /// language. Used by the runtime when a tool call's target
    /// file resolves to a specific language.
    pub fn language_permitted(&self, lang: LspLanguageWire) -> bool {
        self.languages.is_empty() || self.languages.contains(&lang)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_yaml(yaml: &str) -> LspPolicy {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn default_is_disabled_with_rust_prewarm() {
        let p = LspPolicy::default();
        assert!(!p.enabled);
        assert!(p.languages.is_empty());
        assert_eq!(p.prewarm, vec![LspLanguageWire::Rust]);
        assert_eq!(p.idle_teardown_secs, DEFAULT_IDLE_TEARDOWN_SECS);
    }

    #[test]
    fn empty_yaml_uses_defaults() {
        let p = from_yaml("{}");
        assert_eq!(p, LspPolicy::default());
    }

    #[test]
    fn full_yaml_deserialises() {
        let yaml = r#"
            enabled: true
            languages: [rust, python]
            prewarm: [rust]
            idle_teardown_secs: 1200
        "#;
        let p = from_yaml(yaml);
        assert!(p.enabled);
        assert_eq!(
            p.languages,
            vec![LspLanguageWire::Rust, LspLanguageWire::Python]
        );
        assert_eq!(p.prewarm, vec![LspLanguageWire::Rust]);
        assert_eq!(p.idle_teardown_secs, 1200);
    }

    #[test]
    fn language_permitted_empty_whitelist_means_all() {
        let p = LspPolicy::default();
        for lang in LspLanguageWire::all() {
            assert!(p.language_permitted(*lang));
        }
    }

    #[test]
    fn language_permitted_explicit_whitelist_filters() {
        let p = LspPolicy {
            languages: vec![LspLanguageWire::Rust],
            ..LspPolicy::default()
        };
        assert!(p.language_permitted(LspLanguageWire::Rust));
        assert!(!p.language_permitted(LspLanguageWire::Python));
        assert!(!p.language_permitted(LspLanguageWire::TypeScript));
        assert!(!p.language_permitted(LspLanguageWire::Go));
    }

    #[test]
    fn tool_enabled_matches_enabled_field() {
        let p = LspPolicy {
            enabled: true,
            ..LspPolicy::default()
        };
        assert!(p.tool_enabled());
        let p2 = LspPolicy {
            enabled: false,
            ..LspPolicy::default()
        };
        assert!(!p2.tool_enabled());
    }

    #[test]
    fn matrix_name_is_stable() {
        assert_eq!(LspLanguageWire::Rust.matrix_name(), "rust");
        assert_eq!(LspLanguageWire::Python.matrix_name(), "python");
        assert_eq!(LspLanguageWire::TypeScript.matrix_name(), "typescript");
        assert_eq!(LspLanguageWire::Go.matrix_name(), "go");
    }

    #[test]
    fn unknown_field_is_rejected() {
        let yaml = r#"
            enabled: true
            mystery: 1
        "#;
        let err = serde_yaml::from_str::<LspPolicy>(yaml)
            .unwrap_err()
            .to_string();
        assert!(err.contains("mystery"), "error was: {err}");
    }
}
