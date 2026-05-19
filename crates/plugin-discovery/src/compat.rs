//! SDK / daemon version compat gate.
//!
//! Compares the manifest's `[plugin] min_nexo_version` semver
//! requirement against the running daemon's version. UI uses the
//! result to enable / disable the install button on each
//! `<PluginCard>`.

use semver::{Version, VersionReq};

use crate::types::CompatStatus;

/// Run the compat check. The manifest's `min_nexo_version` is a
/// `VersionReq` (e.g. `>=0.2, <0.3`); the daemon's running
/// `nexo-rs` `CARGO_PKG_VERSION` is the `Version`.
///
/// Returns:
///   - `Compatible` when the req admits the daemon's version.
///   - `NeedsUpgrade` when the req's lowest-admitted version is
///     strictly newer than the daemon — the operator needs to
///     upgrade the host. Display strings keep the original input
///     so UI tooltips render the exact required range.
///   - `Incompatible` when the daemon's version exceeds the req's
///     upper bound (the plugin pinned to an older daemon line and
///     wasn't bumped for the current breaking release).
///
/// Edge cases:
///   - When `require` is the wildcard `*` (matches everything) the
///     result is `Compatible` without any other branching.
pub fn compat_check(require: Option<&VersionReq>, daemon: &Version) -> CompatStatus {
    let Some(req) = require else {
        return CompatStatus::Unknown;
    };
    if req.matches(daemon) {
        return CompatStatus::Compatible;
    }
    // Distinguish "daemon too OLD" from "daemon too NEW" by
    // checking if a version > daemon exists that the req would
    // admit. We don't have direct access to the inner comparators
    // so use a synthetic probe: walk a few candidate semver bumps
    // and see if any of them satisfy. Cheap + good enough for the
    // UI tooltip — the discovery layer doesn't need a full
    // VersionReq solver.
    let probes = [
        Version::new(daemon.major + 1, 0, 0),
        Version::new(daemon.major, daemon.minor + 1, 0),
        Version::new(daemon.major, daemon.minor, daemon.patch + 1),
    ];
    let needs_upgrade = probes.iter().any(|p| req.matches(p));
    if needs_upgrade {
        CompatStatus::NeedsUpgrade {
            required: req.to_string(),
            current: daemon.to_string(),
        }
    } else {
        CompatStatus::Incompatible {
            reason: format!(
                "plugin requires {} but daemon is {} (no forward-compatible probe matches)",
                req, daemon
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::VersionReq;

    fn req(s: &str) -> VersionReq {
        VersionReq::parse(s).unwrap()
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn unknown_when_no_requirement_declared() {
        assert_eq!(compat_check(None, &v("0.1.0")), CompatStatus::Unknown);
    }

    #[test]
    fn compatible_for_caret_range_within_minor() {
        // `^0.2` admits 0.2.x — daemon 0.2.5 matches.
        assert_eq!(
            compat_check(Some(&req("^0.2")), &v("0.2.5")),
            CompatStatus::Compatible
        );
    }

    #[test]
    fn needs_upgrade_when_daemon_below_minimum() {
        // `>=0.3` needs at least 0.3; daemon at 0.2 → needs upgrade.
        match compat_check(Some(&req(">=0.3")), &v("0.2.0")) {
            CompatStatus::NeedsUpgrade { required, current } => {
                assert!(required.contains("0.3"), "required: {required}");
                assert_eq!(current, "0.2.0");
            }
            other => panic!("expected NeedsUpgrade, got {other:?}"),
        }
    }

    #[test]
    fn incompatible_when_daemon_above_pinned_upper_bound() {
        // `<0.2` requires daemon below 0.2; daemon 1.0 is way past.
        // No forward probe (next minor / major / patch from 1.0)
        // satisfies `<0.2`, so we land in Incompatible.
        match compat_check(Some(&req("<0.2")), &v("1.0.0")) {
            CompatStatus::Incompatible { reason } => {
                assert!(reason.contains("daemon is 1.0.0"), "{reason}");
            }
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn wildcard_matches_anything() {
        assert_eq!(
            compat_check(Some(&req("*")), &v("99.0.0")),
            CompatStatus::Compatible
        );
    }

    #[test]
    fn exact_boundary_match_compatible() {
        // `>=0.2.0` admits 0.2.0 exactly.
        assert_eq!(
            compat_check(Some(&req(">=0.2.0")), &v("0.2.0")),
            CompatStatus::Compatible
        );
    }
}
