//! Per-binding assistant-mode configuration. Phase 80.15.
//!
//! Behavioural toggle for proactive-agent posture. When enabled,
//! the binding's effective system prompt picks up an addendum and
//! (later, via 80.15.b) auto-spawns an initial team. Default is
//! disabled — bindings without the block keep current behaviour.

use serde::{Deserialize, Serialize};

/// YAML schema for `agents.<id>.assistant_mode`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AssistantConfig {
    /// Master toggle. `false` (default) → behaviour identical to no
    /// block at all.
    #[serde(default)]
    pub enabled: bool,

    /// Operator override of the system-prompt addendum text. When
    /// `None` and `enabled = true`, the bundled default (shipped in
    /// `nexo-assistant`) is appended to the binding's effective
    /// system prompt.
    #[serde(default)]
    pub system_prompt_addendum: Option<String>,

    /// Initial teammates auto-spawned at boot. Empty (default) → no
    /// spawn. Wired by Phase 80.15.b follow-up; the field is
    /// accepted at parse time so YAML doesn't need migration when
    /// the spawn code lands.
    #[serde(default)]
    pub initial_team: Vec<String>,
}

impl AssistantConfig {
    /// Validate at boot. Empty string for the addendum is rejected
    /// (use `None` to fall back to the default); team-name shapes
    /// are checked structurally only — actual teammate resolution
    /// lands in the follow-up.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(text) = &self.system_prompt_addendum {
            if text.trim().is_empty() {
                return Err(
                    "assistant_mode.system_prompt_addendum is empty; \
                     omit the field to use the default, or write a \
                     non-empty override"
                        .into(),
                );
            }
        }
        for name in &self.initial_team {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Err(format!(
                    "assistant_mode.initial_team entry `{name}` is not \
                     a valid teammate name (alphanumeric + - / _ only)"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let cfg = AssistantConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.system_prompt_addendum.is_none());
        assert!(cfg.initial_team.is_empty());
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_empty_addendum() {
        let cfg = AssistantConfig {
            enabled: true,
            system_prompt_addendum: Some(String::new()),
            initial_team: vec![],
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_whitespace_only_addendum() {
        let cfg = AssistantConfig {
            enabled: true,
            system_prompt_addendum: Some("   \n  ".into()),
            initial_team: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_team_name() {
        let cfg = AssistantConfig {
            enabled: true,
            system_prompt_addendum: None,
            initial_team: vec!["bad name!".into()],
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("bad name!"));
    }

    #[test]
    fn validate_accepts_well_formed_team_names() {
        let cfg = AssistantConfig {
            enabled: true,
            system_prompt_addendum: None,
            initial_team: vec![
                "researcher".into(),
                "code-writer".into(),
                "qa_lead".into(),
                "team42".into(),
            ],
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn yaml_round_trip_disabled() {
        let yaml = "enabled: false\n";
        let parsed: AssistantConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(!parsed.enabled);
        let back = serde_yaml::to_string(&parsed).unwrap();
        assert!(back.contains("enabled: false"));
    }

    #[test]
    fn yaml_round_trip_full() {
        let yaml = "\
enabled: true
system_prompt_addendum: \"custom proactive nudge\"
initial_team:
  - researcher
  - coder
";
        let parsed: AssistantConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(parsed.enabled);
        assert_eq!(
            parsed.system_prompt_addendum.as_deref(),
            Some("custom proactive nudge")
        );
        assert_eq!(parsed.initial_team, vec!["researcher", "coder"]);
        parsed.validate().unwrap();
    }
}
