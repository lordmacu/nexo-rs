//! Lifecycle event types emitted on persona install / remove.
//! Mirrors the plugin lifecycle event shape from Phase 81.21.b
//! so subscribers writing schema-strict consumers can share
//! decoder code via a discriminator field. Default subjects
//! sit under `nexo.persona.*`; configurable by the daemon at
//! wire-up time (Phase F5).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Default NATS subject prefix used by lifecycle publishes.
/// Daemons can override via the broker config to land
/// persona events on a different bus path (matches the
/// override pattern Phase 81.21.b shipped for plugin
/// lifecycle events).
pub const DEFAULT_LIFECYCLE_SUBJECT_PREFIX: &str = "nexo.persona";

/// Discriminator + payload for every lifecycle wire event.
/// Encoded as `{"kind":"...", ...}` — the discriminator lives
/// at the top of the JSON object so naive `jq '.kind'`
/// consumers work without descending into the payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleEvent {
    /// Persona install pipeline completed successfully and
    /// the pack is on disk under `install_root`.
    Installed {
        /// Persona id from the manifest.
        id: String,
        /// Installed semver version (string-encoded).
        version: String,
        /// Absolute path of the install dir
        /// (`<state_root>/personas/<id>-<version>/`).
        install_root: String,
        /// Wall-clock timestamp of the successful install.
        installed_at: DateTime<Utc>,
        /// `<owner>/<repo>` the pack was sourced from. Useful
        /// for audit trails.
        source_repo: String,
    },
    /// Persona was removed (state dir deleted) by an admin
    /// action.
    Removed {
        /// Persona id removed.
        id: String,
        /// Version removed.
        version: String,
        /// Wall-clock timestamp of the removal.
        removed_at: DateTime<Utc>,
    },
    /// Install pipeline failed before the pack reached its
    /// final on-disk location. Carries enough context to
    /// retry / triage from the bus event without scraping
    /// daemon logs.
    InstallFailed {
        /// Persona id (from coords if manifest hadn't parsed
        /// yet, e.g. resolver failures echoed via the repo
        /// path).
        id: String,
        /// `<owner>/<repo>[@<tag>]` the operator passed.
        coords: String,
        /// One-line failure description.
        reason: String,
        /// Wall-clock timestamp of the failure.
        failed_at: DateTime<Utc>,
    },
}

impl LifecycleEvent {
    /// Sub-subject the event lands on, joined under the
    /// caller's chosen prefix. Avoids leaking the full
    /// subject string into the variant when the prefix is
    /// configurable.
    pub const fn sub_subject(&self) -> &'static str {
        match self {
            LifecycleEvent::Installed { .. } => "installed",
            LifecycleEvent::Removed { .. } => "removed",
            LifecycleEvent::InstallFailed { .. } => "install_failed",
        }
    }

    /// Concrete subject under [`DEFAULT_LIFECYCLE_SUBJECT_PREFIX`].
    /// Use [`subject_with_prefix`](Self::subject_with_prefix)
    /// when the daemon wants a different bus path.
    pub fn subject(&self) -> String {
        self.subject_with_prefix(DEFAULT_LIFECYCLE_SUBJECT_PREFIX)
    }

    /// Concrete subject under a caller-supplied prefix
    /// (matches Phase 81.21.b's override pattern for plugin
    /// lifecycle events).
    pub fn subject_with_prefix(&self, prefix: &str) -> String {
        format!("{}.{}", prefix.trim_end_matches('.'), self.sub_subject())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap()
    }

    #[test]
    fn installed_serializes_with_kind_discriminator_at_top() {
        let ev = LifecycleEvent::Installed {
            id: "cody".into(),
            version: "0.2.0".into(),
            install_root: "/var/lib/nexo/personas/cody-0.2.0".into(),
            installed_at: fixed_ts(),
            source_repo: "lordmacu/nexo-persona-cody".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "installed");
        assert_eq!(json["id"], "cody");
        assert_eq!(json["version"], "0.2.0");
    }

    #[test]
    fn subject_with_prefix_strips_trailing_dot_and_appends_sub() {
        let ev = LifecycleEvent::Removed {
            id: "x".into(),
            version: "0.1.0".into(),
            removed_at: fixed_ts(),
        };
        assert_eq!(
            ev.subject_with_prefix("nexo.persona."),
            "nexo.persona.removed"
        );
        assert_eq!(
            ev.subject_with_prefix("nexo.persona"),
            "nexo.persona.removed"
        );
        assert_eq!(ev.subject(), "nexo.persona.removed");
    }

    #[test]
    fn install_failed_subject_uses_install_failed_sub() {
        let ev = LifecycleEvent::InstallFailed {
            id: "x".into(),
            coords: "alice/x@v0.1.0".into(),
            reason: "release tag not semver".into(),
            failed_at: fixed_ts(),
        };
        assert_eq!(ev.sub_subject(), "install_failed");
        assert_eq!(ev.subject(), "nexo.persona.install_failed");
    }
}
