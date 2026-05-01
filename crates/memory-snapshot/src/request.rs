//! Inputs to [`crate::snapshotter::MemorySnapshotter::snapshot`] and
//! [`crate::snapshotter::MemorySnapshotter::restore`].

use std::path::PathBuf;

use crate::id::AgentId;

/// Encryption recipient supplied at snapshot time.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EncryptionKey {
    /// `age` recipient string in the canonical bech32 form
    /// (`age1...`). Wraps the bundle body; manifest stays plaintext.
    AgePublicKey(String),
}

/// Identity supplied at restore time to decrypt an age-wrapped bundle.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DecryptionIdentity {
    /// Path to a file containing one or more age identities. Loaded
    /// once per restore call; the file's bytes do not leave this
    /// process.
    AgeIdentityFile(PathBuf),
}

#[derive(Debug, Clone)]
pub struct SnapshotRequest {
    pub agent_id: AgentId,
    pub tenant: String,
    pub label: Option<String>,
    /// When `true`, secret-guard scanner runs over the staged bundle
    /// before packing and the manifest carries a `RedactionReport`.
    pub redact_secrets: bool,
    pub encrypt: Option<EncryptionKey>,
    /// Free-form provenance string surfaced in the manifest's
    /// `created_by` column. Caller picks the value (`cli`, `tool`,
    /// `auto-pre-restore`, …); no validation here.
    pub created_by: String,
}

impl SnapshotRequest {
    /// Operator-driven snapshot via the CLI. Defaults secrets-on
    /// because operators rarely want to ship secrets into a portable
    /// bundle by accident.
    pub fn cli(agent_id: impl Into<AgentId>, tenant: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            tenant: tenant.into(),
            label: None,
            redact_secrets: true,
            encrypt: None,
            created_by: "cli".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RestoreRequest {
    pub agent_id: AgentId,
    pub tenant: String,
    pub bundle: PathBuf,
    /// `true` reports the diff that would be applied without mutating
    /// the live agent.
    pub dry_run: bool,
    /// `true` (default) snapshots the live state before applying the
    /// restore so the operation can be reversed.
    pub auto_pre_snapshot: bool,
    /// Required when the bundle's manifest has an `encryption` block.
    pub decrypt: Option<DecryptionIdentity>,
}

impl RestoreRequest {
    /// Sensible default: dry-run off, auto-pre-snapshot on. Callers
    /// flip the booleans explicitly when they want destructive behavior.
    pub fn new(
        agent_id: impl Into<AgentId>,
        tenant: impl Into<String>,
        bundle: impl Into<PathBuf>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            tenant: tenant.into(),
            bundle: bundle.into(),
            dry_run: false,
            auto_pre_snapshot: true,
            decrypt: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_request_cli_defaults_redact_on() {
        let r = SnapshotRequest::cli("ana", "default");
        assert!(r.redact_secrets);
        assert!(r.encrypt.is_none());
        assert_eq!(r.created_by, "cli");
    }

    #[test]
    fn restore_request_defaults_to_destructive_with_pre_snapshot() {
        let r = RestoreRequest::new("ana", "default", "/tmp/x.tar.zst");
        assert!(!r.dry_run);
        assert!(r.auto_pre_snapshot);
    }

    #[test]
    fn encryption_key_age_round_trip_via_clone() {
        let k = EncryptionKey::AgePublicKey("age1abc".into());
        let cloned = k.clone();
        match cloned {
            EncryptionKey::AgePublicKey(s) => assert_eq!(s, "age1abc"),
        }
    }
}
