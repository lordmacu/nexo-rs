//! Phase 80.8 — brief mode config.
//!
//! Brief mode tells the agent that its visible output channel is
//! the new `send_user_message` tool. When `enabled = true`:
//!
//! 1. The tool is registered for the binding.
//! 2. A short system-prompt section reminds the model to route
//!    user-visible replies through the tool instead of free text.
//! 3. (Deferred — 80.8.b) channel adapters can opt in to *hide*
//!    free-text output and only render `send_user_message` calls.
//!
//! Activation composes with Phase 80.15 `assistant_mode`: when the
//! agent is in assistant mode, brief is implicitly entitled even if
//! the operator did not flip `brief.enabled` — assistant mode's
//! system addendum already tells the model to use the tool.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct BriefConfig {
    /// Master switch. `false` (default) keeps current behaviour: no
    /// tool, no system-prompt section. `true` registers the tool
    /// and appends the section.
    #[serde(default)]
    pub enabled: bool,

    /// When `true`, the tool's `status` field is required at the
    /// schema level (must be `"normal"` or `"proactive"`). When
    /// `false`, the field is optional and defaults to `"normal"`
    /// — useful for piloting brief mode with models that aren't
    /// yet trained on the proactive flow.
    #[serde(default = "default_status_required")]
    pub status_required: bool,

    /// Maximum number of attachment paths the tool will accept in
    /// a single call. Hard upper bound to keep payloads bounded.
    /// `0` disables attachments entirely (tool rejects any
    /// non-empty list).
    #[serde(default = "default_max_attachments")]
    pub max_attachments: u32,
}

fn default_status_required() -> bool {
    true
}
fn default_max_attachments() -> u32 {
    8
}

impl Default for BriefConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            status_required: default_status_required(),
            max_attachments: default_max_attachments(),
        }
    }
}

impl BriefConfig {
    /// Validate at boot. Mirrors the defensive validation the rest
    /// of the Phase 80 cluster ships (`cron_jitter`, `away_summary`).
    pub fn validate(&self) -> Result<(), String> {
        // Sanity cap — no-one needs 1 000 attachments per turn, and
        // the tool's worst-case path-validation pass is O(N).
        if self.max_attachments > 64 {
            return Err(format!(
                "brief.max_attachments {} exceeds hard cap of 64",
                self.max_attachments
            ));
        }
        Ok(())
    }

    /// Convenience used by the boot path to decide whether the
    /// tool should be registered for a binding. Composes with
    /// Phase 80.15 `assistant_mode` — assistant-mode bindings are
    /// implicitly entitled even when the operator did not opt in.
    pub fn is_active_with_assistant_mode(&self, assistant_mode_active: bool) -> bool {
        self.enabled || assistant_mode_active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_with_sensible_values() {
        let c = BriefConfig::default();
        assert!(!c.enabled);
        assert!(c.status_required);
        assert_eq!(c.max_attachments, 8);
        c.validate().unwrap();
    }

    #[test]
    fn validate_rejects_excess_max_attachments() {
        let c = BriefConfig {
            max_attachments: 100,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn is_active_with_assistant_mode_short_circuits_on_assistant() {
        let c = BriefConfig::default(); // enabled = false
        assert!(c.is_active_with_assistant_mode(true));
        assert!(!c.is_active_with_assistant_mode(false));
    }

    #[test]
    fn is_active_with_assistant_mode_honours_explicit_enabled() {
        let c = BriefConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(c.is_active_with_assistant_mode(false));
        assert!(c.is_active_with_assistant_mode(true));
    }

    #[test]
    fn yaml_round_trip_full() {
        let yaml = "\
enabled: true
status_required: false
max_attachments: 16
";
        let parsed: BriefConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(parsed.enabled);
        assert!(!parsed.status_required);
        assert_eq!(parsed.max_attachments, 16);
        parsed.validate().unwrap();
    }

    #[test]
    fn yaml_partial_uses_defaults() {
        let yaml = "enabled: true\n";
        let parsed: BriefConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(parsed.enabled);
        assert!(parsed.status_required);
        assert_eq!(parsed.max_attachments, 8);
    }
}
