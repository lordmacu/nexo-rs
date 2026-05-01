//! Phase 82.10.b — admin RPC capability gates.
//!
//! Layered grant model:
//! - **plugin.toml** declares `[capabilities.admin] required +
//!   optional` — what the microapp NEEDS to function.
//! - **extensions.yaml** `entries.<id>.capabilities_grant: [...]`
//!   — what the operator ALLOWS that microapp to do.
//!
//! Boot diff:
//! - Required missing → fail-fast boot error.
//! - Optional missing → warn log; runtime `-32004` on call.
//! - Orphan grant (granted but not declared) → warn log; allowed
//!   for forward-compat (operator may pre-grant before plugin
//!   updates).
//!
//! Runtime: `CapabilitySet::check(microapp_id, capability)` is a
//! synchronous lock-free lookup invoked before each admin RPC
//! handler dispatch.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Resolved per-microapp capability grants. Built once at boot
/// from the [validate] diff and held read-only inside the
/// dispatcher behind an `Arc`.
#[derive(Debug, Clone, Default)]
pub struct CapabilitySet {
    granted: HashMap<String, HashSet<String>>,
}

impl CapabilitySet {
    /// Build from a fully-validated `microapp_id → granted
    /// capabilities` map. Production callers go through
    /// [`validate_capabilities_at_boot`] which produces this map
    /// alongside the boot report.
    pub fn from_grants(granted: HashMap<String, HashSet<String>>) -> Arc<Self> {
        Arc::new(Self { granted })
    }

    /// Empty set — no microapp has any capability. Useful for
    /// tests + as a safe default before boot validation runs.
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Lock-free check on the hot path. `false` means the runtime
    /// must return `-32004 capability_not_granted`.
    pub fn check(&self, microapp_id: &str, capability: &str) -> bool {
        self.granted
            .get(microapp_id)
            .is_some_and(|set| set.contains(capability))
    }

    /// All capabilities granted to a microapp. Operator-facing
    /// diagnostic; not used on the hot path.
    pub fn granted_for(&self, microapp_id: &str) -> Option<&HashSet<String>> {
        self.granted.get(microapp_id)
    }
}

/// Boot-time diff between plugin manifests + operator grants.
#[derive(Debug, Default)]
pub struct CapabilityBootReport {
    /// Fail-fast errors. Caller (boot supervisor) MUST treat any
    /// non-empty `errors` as boot failure.
    pub errors: Vec<CapabilityBootError>,
    /// Operator-facing warnings. Caller logs at WARN level.
    pub warns: Vec<CapabilityBootWarn>,
    /// Resolved grants ready to feed into [`CapabilitySet::from_grants`].
    pub grants: HashMap<String, HashSet<String>>,
}

/// Boot-fatal capability mismatch.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum CapabilityBootError {
    /// `plugin.toml` lists capabilities under `required` that the
    /// operator did not grant in `extensions.yaml`. Microapp cannot
    /// run.
    RequiredNotGranted {
        /// Microapp identifier.
        microapp_id: String,
        /// Required capabilities the operator did not grant.
        missing: Vec<String>,
    },
}

/// Boot-time warning — operator-facing diagnostic, not fatal.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum CapabilityBootWarn {
    /// `plugin.toml` lists optional capabilities the operator did
    /// not grant. The microapp boots; runtime calls to those
    /// capabilities return `-32004`.
    OptionalNotGranted {
        /// Microapp identifier.
        microapp_id: String,
        /// Optional capabilities the operator did not grant.
        missing: Vec<String>,
    },
    /// Operator granted capabilities the plugin manifest does not
    /// declare. Allowed (forward-compat for upgrades) but warned.
    OrphanGrant {
        /// Microapp identifier.
        microapp_id: String,
        /// Capabilities granted but not declared.
        orphan: Vec<String>,
    },
}

/// Boot-time validator. Diffs each microapp's `plugin.toml`
/// declared admin capabilities against the operator's
/// `extensions.yaml` grants and produces a [`CapabilityBootReport`].
///
/// Caller wires:
/// 1. Read all discovered plugin manifests (already done by Phase
///    81 plugin discovery / current Phase 11 boot).
/// 2. Read `extensions.yaml.entries` (Phase 82.10 — new field).
/// 3. Call this fn → `CapabilityBootReport`.
/// 4. Treat `errors` as fail-fast. Log `warns` at WARN level.
/// 5. Feed `grants` into `CapabilitySet::from_grants(...)`.
pub fn validate_capabilities_at_boot(
    declarations: &[(String, AdminCapabilityDecl)],
    grants: &HashMap<String, Vec<String>>,
) -> CapabilityBootReport {
    let mut report = CapabilityBootReport::default();

    for (microapp_id, decl) in declarations {
        let granted: HashSet<String> = grants
            .get(microapp_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();

        let required: HashSet<String> = decl.required.iter().cloned().collect();
        let optional: HashSet<String> = decl.optional.iter().cloned().collect();
        let declared: HashSet<String> = required.union(&optional).cloned().collect();

        let missing_required: Vec<String> =
            required.difference(&granted).cloned().collect();
        if !missing_required.is_empty() {
            let mut sorted = missing_required;
            sorted.sort();
            report.errors.push(CapabilityBootError::RequiredNotGranted {
                microapp_id: microapp_id.clone(),
                missing: sorted,
            });
        }

        let missing_optional: Vec<String> =
            optional.difference(&granted).cloned().collect();
        if !missing_optional.is_empty() {
            let mut sorted = missing_optional;
            sorted.sort();
            report.warns.push(CapabilityBootWarn::OptionalNotGranted {
                microapp_id: microapp_id.clone(),
                missing: sorted,
            });
        }

        let orphan: Vec<String> = granted.difference(&declared).cloned().collect();
        if !orphan.is_empty() {
            let mut sorted = orphan;
            sorted.sort();
            report.warns.push(CapabilityBootWarn::OrphanGrant {
                microapp_id: microapp_id.clone(),
                orphan: sorted,
            });
        }

        report.grants.insert(microapp_id.clone(), granted);
    }

    report
}

/// Local mirror of [`nexo_plugin_manifest::AdminCapabilities`] —
/// the capability check layer is in `nexo-core` and we don't want
/// it to depend on the manifest crate (manifest depends on
/// nothing core-side; keeping the inversion is healthier).
/// Boot wiring fills this from the parsed manifest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AdminCapabilityDecl {
    /// Capabilities the microapp cannot run without.
    pub required: Vec<String>,
    /// Capabilities the microapp can run without.
    pub optional: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(required: &[&str], optional: &[&str]) -> AdminCapabilityDecl {
        AdminCapabilityDecl {
            required: required.iter().map(|s| s.to_string()).collect(),
            optional: optional.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn grant(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn required_missing_returns_boot_error() {
        let decls = vec![(
            "agent-creator".into(),
            decl(&["agents_crud", "credentials_crud"], &[]),
        )];
        let mut grants_map = HashMap::new();
        grants_map.insert("agent-creator".into(), grant(&["agents_crud"]));

        let report = validate_capabilities_at_boot(&decls, &grants_map);
        assert_eq!(report.errors.len(), 1);
        match &report.errors[0] {
            CapabilityBootError::RequiredNotGranted {
                microapp_id,
                missing,
            } => {
                assert_eq!(microapp_id, "agent-creator");
                assert_eq!(missing, &vec!["credentials_crud".to_string()]);
            }
        }
    }

    #[test]
    fn optional_missing_returns_warn_not_error() {
        let decls = vec![(
            "agent-creator".into(),
            decl(&["agents_crud"], &["llm_keys_crud"]),
        )];
        let mut grants_map = HashMap::new();
        grants_map.insert("agent-creator".into(), grant(&["agents_crud"]));

        let report = validate_capabilities_at_boot(&decls, &grants_map);
        assert!(report.errors.is_empty());
        assert_eq!(report.warns.len(), 1);
        match &report.warns[0] {
            CapabilityBootWarn::OptionalNotGranted { missing, .. } => {
                assert_eq!(missing, &vec!["llm_keys_crud".to_string()]);
            }
            other => panic!("expected OptionalNotGranted, got {other:?}"),
        }
    }

    #[test]
    fn orphan_grant_returns_warn() {
        let decls = vec![("agent-creator".into(), decl(&["agents_crud"], &[]))];
        let mut grants_map = HashMap::new();
        grants_map.insert(
            "agent-creator".into(),
            grant(&["agents_crud", "future_capability"]),
        );

        let report = validate_capabilities_at_boot(&decls, &grants_map);
        assert!(report.errors.is_empty());
        // Just orphan warn, no missing.
        assert_eq!(report.warns.len(), 1);
        match &report.warns[0] {
            CapabilityBootWarn::OrphanGrant { orphan, .. } => {
                assert_eq!(orphan, &vec!["future_capability".to_string()]);
            }
            other => panic!("expected OrphanGrant, got {other:?}"),
        }
    }

    #[test]
    fn all_satisfied_no_errors_no_warns() {
        let decls = vec![("agent-creator".into(), decl(&["agents_crud"], &["llm_keys_crud"]))];
        let mut grants_map = HashMap::new();
        grants_map.insert(
            "agent-creator".into(),
            grant(&["agents_crud", "llm_keys_crud"]),
        );

        let report = validate_capabilities_at_boot(&decls, &grants_map);
        assert!(report.errors.is_empty());
        assert!(report.warns.is_empty());
    }

    #[test]
    fn capability_set_check_lookup() {
        let mut grants = HashMap::new();
        grants.insert(
            "agent-creator".to_string(),
            HashSet::from(["agents_crud".to_string()]),
        );
        let set = CapabilitySet::from_grants(grants);
        assert!(set.check("agent-creator", "agents_crud"));
        assert!(!set.check("agent-creator", "credentials_crud"));
        assert!(!set.check("unknown-app", "agents_crud"));
    }

    #[test]
    fn capability_set_empty_denies_everything() {
        let set = CapabilitySet::empty();
        assert!(!set.check("any-app", "any_capability"));
    }
}
