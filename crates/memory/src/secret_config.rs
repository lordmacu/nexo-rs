//! Config schema for the secret guard. Deserialized from YAML:
//! `memory.secret_guard { enabled, on_secret, rules, exclude_rules }`.
//!
//! Default is secure-by-default: enabled=true, on_secret=Block, rules=All,
//! exclude_rules=[].

use serde::Deserialize;

use crate::secret_scanner::{OnSecret, SecretGuard, SecretScanner};

/// Selection of scanner rules.
///
/// Deserializes from either the string `"all"` or a list of rule IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleSelection {
    /// Use all available rules.
    All,
    /// Use only the named rules.
    List(Vec<String>),
}

impl Default for RuleSelection {
    fn default() -> Self {
        Self::All
    }
}

impl<'de> Deserialize<'de> for RuleSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct RuleSelectionVisitor;
        impl<'de> de::Visitor<'de> for RuleSelectionVisitor {
            type Value = RuleSelection;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, r#"the string "all" or a list of rule IDs"#)
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                if v == "all" {
                    Ok(RuleSelection::All)
                } else {
                    Err(de::Error::invalid_value(de::Unexpected::Str(v), &self))
                }
            }

            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut ids = Vec::new();
                while let Some(id) = seq.next_element::<String>()? {
                    ids.push(id);
                }
                Ok(RuleSelection::List(ids))
            }
        }

        deserializer.deserialize_any(RuleSelectionVisitor)
    }
}

/// Config block for the secret guard.
///
/// ```yaml
/// memory:
///   secret_guard:
///     enabled: true
///     on_secret: "block"
///     rules: "all"
///     exclude_rules: []
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SecretGuardConfig {
    /// Master switch. When false, all checks are no-ops.
    pub enabled: bool,
    /// Policy for handling detected secrets.
    #[serde(alias = "on_secret")]
    pub on_secret: OnSecret,
    /// Rule selection: "all" or list of rule_ids.
    pub rules: RuleSelection,
    /// Rule IDs to skip (e.g., known false positives).
    #[serde(default)]
    pub exclude_rules: Vec<String>,
}

impl Default for SecretGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            on_secret: OnSecret::Block,
            rules: RuleSelection::All,
            exclude_rules: Vec::new(),
        }
    }
}

impl SecretGuardConfig {
    /// Build a `SecretGuard` from this config.
    pub fn build_guard(&self) -> SecretGuard {
        if !self.enabled {
            return SecretGuard::disabled();
        }

        let scanner = match &self.rules {
            RuleSelection::All if self.exclude_rules.is_empty() => SecretScanner::new(),
            RuleSelection::All => {
                let excludes: Vec<&str> =
                    self.exclude_rules.iter().map(|s| s.as_str()).collect();
                SecretScanner::without_rules(&excludes)
            }
            RuleSelection::List(ids) => {
                let ids: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
                let scanner = SecretScanner::with_rules(&ids);
                if self.exclude_rules.is_empty() {
                    scanner
                } else {
                    let excludes: Vec<&str> =
                        self.exclude_rules.iter().map(|s| s.as_str()).collect();
                    SecretScanner::without_rules(&excludes)
                }
            }
        };

        SecretGuard::new(scanner, self.on_secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_secure() {
        let cfg = SecretGuardConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.on_secret, OnSecret::Block);
        assert_eq!(cfg.rules, RuleSelection::All);
        assert!(cfg.exclude_rules.is_empty());
    }

    #[test]
    fn default_config_guard_is_enabled() {
        let guard = SecretGuardConfig::default().build_guard();
        assert!(guard.is_enabled());
        assert_eq!(guard.on_secret(), OnSecret::Block);
    }

    #[test]
    fn disabled_config_builds_disabled_guard() {
        let cfg = SecretGuardConfig {
            enabled: false,
            ..SecretGuardConfig::default()
        };
        let guard = cfg.build_guard();
        assert!(!guard.is_enabled());
        // Disabled guard passes everything
        assert!(guard.check("ghp_abcdefghijklmnopqrstuvwxyz1234567890").is_ok());
    }

    #[test]
    fn deserialize_from_yaml_all_rules() {
        let yaml = r"
enabled: true
on_secret: redact
rules: all
exclude_rules: [github-pat]
";
        let cfg: SecretGuardConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.on_secret, OnSecret::Redact);
        assert_eq!(cfg.rules, RuleSelection::All);
        assert_eq!(cfg.exclude_rules, vec!["github-pat".to_string()]);
    }

    #[test]
    fn deserialize_from_yaml_rule_list() {
        let yaml = r"
enabled: true
on_secret: warn
rules:
  - github-pat
  - aws-access-token
";
        let cfg: SecretGuardConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.on_secret, OnSecret::Warn);
        assert_eq!(
            cfg.rules,
            RuleSelection::List(vec![
                "github-pat".to_string(),
                "aws-access-token".to_string()
            ])
        );
    }

    #[test]
    fn build_guard_with_excludes() {
        let cfg = SecretGuardConfig {
            enabled: true,
            on_secret: OnSecret::Block,
            rules: RuleSelection::All,
            exclude_rules: vec!["github-pat".to_string()],
        };
        let guard = cfg.build_guard();
        // github-pat should be excluded
        assert!(guard.check("ghp_abcdefghijklmnopqrstuvwxyz1234567890").is_ok());
        // But aws should still be detected
        assert!(guard.check("AKIAIOSFODNN7EXAMPLE").is_err());
    }

    #[test]
    fn build_guard_with_specific_rules() {
        let cfg = SecretGuardConfig {
            enabled: true,
            on_secret: OnSecret::Warn,
            rules: RuleSelection::List(vec!["aws-access-token".to_string()]),
            exclude_rules: vec![],
        };
        let guard = cfg.build_guard();
        // Only aws rule active
        assert!(guard.check("ghp_abcdefghijklmnopqrstuvwxyz1234567890").is_ok());
        assert!(guard.check("AKIAIOSFODNN7EXAMPLE").is_ok()); // Warn passes
    }

    #[test]
    fn deserialize_defaults_when_empty() {
        let yaml = "{}";
        let cfg: SecretGuardConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.on_secret, OnSecret::Block);
    }

    #[test]
    fn deserialize_enabled_false() {
        let yaml = "enabled: false";
        let cfg: SecretGuardConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(!cfg.enabled);
    }
}
