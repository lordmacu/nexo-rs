//! Stable identifiers for snapshots and agents.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Per-agent stable id. The framework reuses string ids across crates
/// (`agents.yaml::id`, NATS subjects) so we keep the alias rather than
/// a newtype to avoid forcing every caller through a `.into()`.
pub type AgentId = String;

/// Globally unique id for a snapshot bundle. Encoded as a v4 UUID; the
/// on-disk filename is `<id>.tar.zst[.age]` and the manifest carries
/// the same value verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotId(pub Uuid);

impl SnapshotId {
    /// Generate a fresh id. Called once per `snapshot()` invocation.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Hyphenated lowercase form, suitable for filenames.
    pub fn as_filename(&self) -> String {
        self.0.as_hyphenated().to_string()
    }
}

impl Default for SnapshotId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.as_hyphenated())
    }
}

impl FromStr for SnapshotId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(SnapshotId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_yields_unique_ids() {
        let a = SnapshotId::new();
        let b = SnapshotId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn round_trip_via_filename_and_from_str() {
        let id = SnapshotId::new();
        let s = id.as_filename();
        let back: SnapshotId = s.parse().unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn display_matches_filename() {
        let id = SnapshotId::new();
        assert_eq!(id.to_string(), id.as_filename());
    }

    #[test]
    fn json_round_trip_uses_transparent_repr() {
        let id = SnapshotId::new();
        let json = serde_json::to_string(&id).unwrap();
        // No nested object — just a quoted UUID string.
        assert!(json.starts_with('"') && json.ends_with('"'));
        let back: SnapshotId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
