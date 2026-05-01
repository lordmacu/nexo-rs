//! Operator-facing YAML schema for the snapshot subsystem.
//!
//! Lives here as a self-contained `serde::Deserialize` struct so the
//! boot wire (in the binary crate) can construct a
//! [`crate::local_fs::LocalFsSnapshotter`] +
//! [`crate::retention::RetentionConfig`] from a single YAML block
//! without dragging the schema into every consumer.
//!
//! Intended placement in `config/memory.yaml`:
//!
//! ```yaml
//! memory:
//!   snapshot:
//!     enabled: true
//!     root: ${NEXO_HOME}/state
//!     auto_pre_dream: false
//!     auto_pre_restore: true
//!     auto_pre_mutating_tool: false
//!     lock_timeout_secs: 60
//!     redact_secrets_default: true
//!     encryption:
//!       enabled: false
//!       recipients: []
//!       identity_path: ${NEXO_HOME}/secret/snapshot-identity.txt
//!     retention:
//!       keep_count: 30
//!       max_age_days: 90
//!       gc_interval_secs: 3600
//!     events:
//!       mutation_subject_prefix: "nexo.memory.mutated"
//!       lifecycle_subject_prefix: "nexo.memory.snapshot"
//!       mutation_publish_enabled: true
//! ```

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::retention::RetentionConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySnapshotConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_root")]
    pub root: PathBuf,
    #[serde(default)]
    pub auto_pre_dream: bool,
    #[serde(default = "default_true")]
    pub auto_pre_restore: bool,
    #[serde(default)]
    pub auto_pre_mutating_tool: bool,
    #[serde(default = "default_lock_timeout_secs")]
    pub lock_timeout_secs: u64,
    #[serde(default = "default_true")]
    pub redact_secrets_default: bool,
    #[serde(default)]
    pub encryption: EncryptionSection,
    #[serde(default)]
    pub retention: RetentionSection,
    #[serde(default)]
    pub events: EventsSection,
}

impl Default for MemorySnapshotConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            root: default_root(),
            auto_pre_dream: false,
            auto_pre_restore: true,
            auto_pre_mutating_tool: false,
            lock_timeout_secs: default_lock_timeout_secs(),
            redact_secrets_default: true,
            encryption: EncryptionSection::default(),
            retention: RetentionSection::default(),
            events: EventsSection::default(),
        }
    }
}

impl MemorySnapshotConfig {
    pub fn lock_timeout(&self) -> Duration {
        Duration::from_secs(self.lock_timeout_secs)
    }

    pub fn retention_runtime(&self) -> RetentionConfig {
        RetentionConfig {
            keep_count: self.retention.keep_count,
            max_age_days: self.retention.max_age_days,
            gc_interval_secs: self.retention.gc_interval_secs,
        }
    }

    /// Reject combinations that would violate the operator's intent
    /// well before the runtime hits them. The boot wire calls this
    /// after YAML deserialization and before constructing the
    /// snapshotter so a malformed config fails loudly at startup.
    pub fn validate(&self) -> Result<(), String> {
        if self.lock_timeout_secs == 0 {
            return Err("memory.snapshot.lock_timeout_secs must be >= 1".into());
        }
        if self.retention.gc_interval_secs == 0 {
            return Err("memory.snapshot.retention.gc_interval_secs must be >= 1".into());
        }
        if self.encryption.enabled && self.encryption.recipients.is_empty() {
            return Err(
                "memory.snapshot.encryption.enabled = true requires at least one recipient".into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EncryptionSection {
    pub enabled: bool,
    pub recipients: Vec<String>,
    pub identity_path: Option<PathBuf>,
}

impl Default for EncryptionSection {
    fn default() -> Self {
        Self {
            enabled: false,
            recipients: Vec::new(),
            identity_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RetentionSection {
    pub keep_count: u32,
    pub max_age_days: u32,
    pub gc_interval_secs: u64,
}

impl Default for RetentionSection {
    fn default() -> Self {
        let r = RetentionConfig::default();
        Self {
            keep_count: r.keep_count,
            max_age_days: r.max_age_days,
            gc_interval_secs: r.gc_interval_secs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EventsSection {
    pub mutation_subject_prefix: String,
    pub lifecycle_subject_prefix: String,
    pub mutation_publish_enabled: bool,
}

impl Default for EventsSection {
    fn default() -> Self {
        Self {
            mutation_subject_prefix: crate::events::MUTATION_SUBJECT_PREFIX.to_string(),
            lifecycle_subject_prefix: crate::events::LIFECYCLE_SUBJECT_PREFIX.to_string(),
            mutation_publish_enabled: true,
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
}

fn default_root() -> PathBuf {
    PathBuf::from("./state")
}

fn default_lock_timeout_secs() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let c = MemorySnapshotConfig::default();
        assert!(c.enabled);
        assert!(c.auto_pre_restore);
        assert!(!c.auto_pre_dream);
        assert!(!c.auto_pre_mutating_tool);
        assert_eq!(c.lock_timeout_secs, 60);
        assert!(c.redact_secrets_default);
        assert_eq!(c.retention.keep_count, 30);
        assert_eq!(c.retention.max_age_days, 90);
        assert_eq!(c.retention.gc_interval_secs, 3600);
        assert!(c.events.mutation_publish_enabled);
        assert!(!c.encryption.enabled);
    }

    #[test]
    fn parses_minimal_yaml_with_defaults() {
        let c: MemorySnapshotConfig = serde_yaml::from_str("enabled: true\n").unwrap();
        assert!(c.enabled);
        assert_eq!(c.retention.keep_count, 30);
    }

    #[test]
    fn parses_full_yaml_block() {
        let yaml = r#"
enabled: true
root: /var/lib/nexo
auto_pre_dream: true
auto_pre_restore: false
auto_pre_mutating_tool: true
lock_timeout_secs: 30
redact_secrets_default: false
encryption:
  enabled: true
  recipients:
    - age1abc
  identity_path: /etc/nexo/snapshot.key
retention:
  keep_count: 5
  max_age_days: 7
  gc_interval_secs: 600
events:
  mutation_subject_prefix: "x.memory.mutated"
  lifecycle_subject_prefix: "x.memory.snapshot"
  mutation_publish_enabled: false
"#;
        let c: MemorySnapshotConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.lock_timeout_secs, 30);
        assert!(c.encryption.enabled);
        assert_eq!(c.encryption.recipients, vec!["age1abc"]);
        assert_eq!(c.retention.keep_count, 5);
        assert_eq!(c.events.mutation_subject_prefix, "x.memory.mutated");
        c.validate().unwrap();
    }

    #[test]
    fn rejects_unknown_fields() {
        let yaml = "enabled: true\nbogus_key: 1\n";
        let r: Result<MemorySnapshotConfig, _> = serde_yaml::from_str(yaml);
        assert!(r.is_err());
    }

    #[test]
    fn validate_rejects_zero_lock_timeout() {
        let c = MemorySnapshotConfig {
            lock_timeout_secs: 0,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_gc_interval() {
        let c = MemorySnapshotConfig {
            retention: RetentionSection {
                gc_interval_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_encryption_enabled_without_recipients() {
        let c = MemorySnapshotConfig {
            encryption: EncryptionSection {
                enabled: true,
                recipients: Vec::new(),
                identity_path: None,
            },
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn retention_runtime_round_trips_to_runtime_struct() {
        let mut c = MemorySnapshotConfig::default();
        c.retention.keep_count = 7;
        c.retention.max_age_days = 14;
        let r = c.retention_runtime();
        assert_eq!(r.keep_count, 7);
        assert_eq!(r.max_age_days, 14);
    }
}
