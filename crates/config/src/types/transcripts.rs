//! Transcripts subsystem knobs. Loaded from `config/transcripts.yaml`
//! when present; absent file → [`TranscriptsConfig::default`] (FTS on,
//! redaction off — backwards compatible).

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptsConfig {
    #[serde(default)]
    pub fts: TranscriptsFtsConfig,
    #[serde(default)]
    pub redaction: RedactionConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptsFtsConfig {
    #[serde(default = "default_fts_enabled")]
    pub enabled: bool,
    #[serde(default = "default_fts_db_path")]
    pub db_path: String,
}

impl Default for TranscriptsFtsConfig {
    fn default() -> Self {
        Self {
            enabled: default_fts_enabled(),
            db_path: default_fts_db_path(),
        }
    }
}

fn default_fts_enabled() -> bool {
    true
}

fn default_fts_db_path() -> String {
    "./data/transcripts.db".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionConfig {
    /// Master toggle. Default false — redaction must be opted in.
    #[serde(default)]
    pub enabled: bool,
    /// Apply the built-in pattern set (Bearer JWT, sk- keys, AWS, etc.).
    #[serde(default = "default_use_builtins")]
    pub use_builtins: bool,
    /// Operator-defined patterns appended after the built-ins.
    #[serde(default)]
    pub extra_patterns: Vec<RedactionPattern>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            use_builtins: default_use_builtins(),
            extra_patterns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionPattern {
    pub regex: String,
    pub label: String,
}

fn default_use_builtins() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_fts_on_redaction_off() {
        let c = TranscriptsConfig::default();
        assert!(c.fts.enabled);
        assert_eq!(c.fts.db_path, "./data/transcripts.db");
        assert!(!c.redaction.enabled);
        assert!(c.redaction.use_builtins);
        assert!(c.redaction.extra_patterns.is_empty());
    }

    #[test]
    fn parses_full_yaml() {
        let c: TranscriptsConfig = serde_yaml::from_str(
            r#"
fts:
  enabled: false
  db_path: /var/lib/t.db
redaction:
  enabled: true
  use_builtins: false
  extra_patterns:
    - regex: "TENANT-[0-9]+"
      label: tenant_id
"#,
        )
        .unwrap();
        assert!(!c.fts.enabled);
        assert_eq!(c.fts.db_path, "/var/lib/t.db");
        assert!(c.redaction.enabled);
        assert!(!c.redaction.use_builtins);
        assert_eq!(c.redaction.extra_patterns.len(), 1);
        assert_eq!(c.redaction.extra_patterns[0].label, "tenant_id");
    }

    #[test]
    fn unknown_field_rejected() {
        let err = serde_yaml::from_str::<TranscriptsConfig>("bogus: 1\n").expect_err("deny");
        assert!(err.to_string().contains("bogus"));
    }
}
