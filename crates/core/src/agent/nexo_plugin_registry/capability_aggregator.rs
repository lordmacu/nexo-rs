//! Phase 81.11 — aggregate plugin-declared capability gates +
//! check unmet `requires.nexo_capabilities` entries. The bundled
//! `nexo_setup::capabilities::INVENTORY` const is borrowed read-
//! only — drift-prevention contract preserved.
//!
//! Conflicts vs INVENTORY → Error diagnostics; cross-plugin
//! conflicts → Error diagnostics; unmet-capability → Warn.
//! `wire_plugin_registry` (Phase 81.9 / 81.11) folds the result
//! into `PluginDiscoveryReport` so doctor CLI + admin-ui see one
//! coherent view.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use nexo_plugin_manifest::{GateKind, GateRisk};

use super::report::{
    DiagnosticLevel, DiscoveryDiagnostic, DiscoveryDiagnosticKind,
};
use super::NexoPluginRegistrySnapshot;

/// Phase 81.11 — runtime view of one plugin-declared capability
/// gate. Carries the manifest's static fields plus a runtime
/// evaluation (`state` + `raw_value`) using the same env::var
/// logic as `nexo_setup::capabilities::evaluate_one`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AggregatedGate {
    pub plugin_id: String,
    pub env_var: String,
    pub kind: GateKind,
    pub risk: GateRisk,
    pub effect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub state: AggregatedGateState,
    /// Raw env var value when set + non-empty. `None` when the
    /// var is unset OR the gate is a `CargoFeature` (Cargo
    /// features are compile-time; runtime read is meaningless).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_value: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregatedGateState {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnmetRequirement {
    pub plugin_id: String,
    pub capability_name: String,
}

/// Result of [`aggregate_plugin_gates`]. Caller folds into
/// `PluginDiscoveryReport` via `fold_capability_aggregation`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PluginCapabilityAggregation {
    pub gates: BTreeMap<String, AggregatedGate>,
    pub unmet_required: Vec<UnmetRequirement>,
    pub conflicts: Vec<DiscoveryDiagnostic>,
}

/// Phase 81.11 — collect plugin-declared capability gates +
/// required-capability mismatches.
///
/// Caller-provided arguments:
/// - `core_env_vars`: list of `(env_var, core_extension)` pairs
///   borrowed from `nexo_setup::capabilities::INVENTORY` by the
///   bridge layer (main.rs / doctor handler). Lives outside this
///   crate because `nexo-core` cannot depend on `nexo-setup` (the
///   dep direction is `nexo-setup → nexo-core` per workspace topology).
/// - `available`: capabilities the running daemon has wired
///   (e.g., `"broker"` always; `"long_term_memory"` only when
///   the long-term memory backend is configured).
///
/// Conflict semantics:
/// - Plugin gate `env_var` matches a core entry → drop the plugin
///   gate, emit `CapabilityGateConflictsCore` Error.
/// - Plugin gate `env_var` matches an earlier plugin's gate →
///   drop the new gate, emit `CapabilityGateConflictsPlugin`
///   Error (first-plugin-wins).
/// - Plugin's `requires.nexo_capabilities` entry not in
///   `available` → emit `RequiredCapabilityNotGranted` Warn +
///   record `UnmetRequirement`.
pub fn aggregate_plugin_gates(
    snapshot: &NexoPluginRegistrySnapshot,
    core_env_vars: &[(&str, &str)],
    available: &BTreeSet<String>,
) -> PluginCapabilityAggregation {
    let mut report = PluginCapabilityAggregation::default();

    for plugin in &snapshot.plugins {
        let plugin_id = plugin.manifest.plugin.id.clone();

        // 1. Capability gates.
        for gate in &plugin.manifest.plugin.capability_gates.gates {
            // Conflict vs core INVENTORY (caller-supplied list).
            if let Some((_, core_ext)) =
                core_env_vars.iter().find(|(ev, _)| *ev == gate.env_var)
            {
                report.conflicts.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Error,
                    path: plugin.manifest_path.clone(),
                    kind: DiscoveryDiagnosticKind::CapabilityGateConflictsCore {
                        env_var: gate.env_var.clone(),
                        plugin_id: plugin_id.clone(),
                        core_extension: core_ext.to_string(),
                    },
                });
                continue;
            }

            // Conflict vs another plugin already aggregated.
            if let Some(prior) = report.gates.get(&gate.env_var) {
                report.conflicts.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Error,
                    path: plugin.manifest_path.clone(),
                    kind: DiscoveryDiagnosticKind::CapabilityGateConflictsPlugin {
                        env_var: gate.env_var.clone(),
                        plugin_a: prior.plugin_id.clone(),
                        plugin_b: plugin_id.clone(),
                    },
                });
                continue;
            }

            // Compute state + raw_value.
            let (state, raw_value) = evaluate_gate(&gate.env_var, gate.kind);

            report.gates.insert(
                gate.env_var.clone(),
                AggregatedGate {
                    plugin_id: plugin_id.clone(),
                    env_var: gate.env_var.clone(),
                    kind: gate.kind,
                    risk: gate.risk,
                    effect: gate.effect.clone(),
                    hint: gate.hint.clone(),
                    state,
                    raw_value,
                },
            );
        }

        // 2. Required capabilities.
        for cap in &plugin.manifest.plugin.requires.nexo_capabilities {
            if !available.contains(cap) {
                report.conflicts.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: plugin.manifest_path.clone(),
                    kind: DiscoveryDiagnosticKind::RequiredCapabilityNotGranted {
                        plugin_id: plugin_id.clone(),
                        capability_name: cap.clone(),
                    },
                });
                report.unmet_required.push(UnmetRequirement {
                    plugin_id: plugin_id.clone(),
                    capability_name: cap.clone(),
                });
            }
        }
    }

    report
}

/// Resolve a plugin gate's runtime state. Mirrors
/// `nexo_setup::capabilities::evaluate_one` for `Boolean` /
/// `Allowlist`; `CargoFeature` always reports `Disabled` because
/// runtime env::var lookup is meaningless for compile-time gates.
fn evaluate_gate(env_var: &str, kind: GateKind) -> (AggregatedGateState, Option<String>) {
    match kind {
        GateKind::Boolean => match std::env::var(env_var) {
            Ok(v) if is_truthy(&v) => (AggregatedGateState::Enabled, Some(v)),
            Ok(v) if !v.is_empty() => (AggregatedGateState::Disabled, Some(v)),
            _ => (AggregatedGateState::Disabled, None),
        },
        GateKind::Allowlist => match std::env::var(env_var) {
            Ok(v) if !v.trim().is_empty()
                && v.split(',').any(|s| !s.trim().is_empty()) =>
            {
                (AggregatedGateState::Enabled, Some(v))
            }
            Ok(v) if !v.is_empty() => (AggregatedGateState::Disabled, Some(v)),
            _ => (AggregatedGateState::Disabled, None),
        },
        GateKind::CargoFeature => (AggregatedGateState::Disabled, None),
    }
}

fn is_truthy(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    use nexo_plugin_manifest::PluginManifest;

    use super::super::report::PluginDiscoveryReport;
    use super::super::DiscoveredPlugin;

    /// Serialize env::var mutation across tests so they don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn discovered(plugin_id: &str, manifest_extra: &str) -> (DiscoveredPlugin, TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_str = format!(
            "[plugin]\n\
             id = \"{plugin_id}\"\n\
             version = \"0.1.0\"\n\
             name = \"{plugin_id}\"\n\
             description = \"fixture\"\n\
             min_nexo_version = \">=0.0.1\"\n\
             {manifest_extra}",
        );
        std::fs::write(tmp.path().join("nexo-plugin.toml"), &manifest_str).unwrap();
        let manifest: PluginManifest = toml::from_str(&manifest_str).unwrap();
        (
            DiscoveredPlugin {
                manifest,
                root_dir: tmp.path().to_path_buf(),
                manifest_path: tmp.path().join("nexo-plugin.toml"),
            },
            tmp,
        )
    }

    fn snapshot_with(plugins: Vec<DiscoveredPlugin>) -> NexoPluginRegistrySnapshot {
        NexoPluginRegistrySnapshot {
            plugins,
            last_report: PluginDiscoveryReport::default(),
            skill_roots: BTreeMap::new(),
        }
    }

    #[test]
    fn single_plugin_gate_added_to_aggregator() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Ensure the gate's env var is unset.
        unsafe { std::env::remove_var("NEXO_FIXTURE_GATE_A") };

        let (p, _tmp) = discovered(
            "fixture_a",
            "[[plugin.capability_gates.gate]]\n\
             extension = \"fixture_a\"\n\
             env_var = \"NEXO_FIXTURE_GATE_A\"\n\
             kind = \"Boolean\"\n\
             risk = \"Low\"\n\
             effect = \"toggle thing\"\n",
        );
        let snap = snapshot_with(vec![p]);
        let agg = aggregate_plugin_gates(&snap, &[], &BTreeSet::new());
        assert_eq!(agg.gates.len(), 1);
        let gate = agg.gates.get("NEXO_FIXTURE_GATE_A").unwrap();
        assert_eq!(gate.plugin_id, "fixture_a");
        assert_eq!(gate.state, AggregatedGateState::Disabled);
        assert!(gate.raw_value.is_none());
        assert!(agg.conflicts.is_empty());
        assert!(agg.unmet_required.is_empty());
    }

    #[test]
    fn manifest_gate_conflicts_with_core_inventory_emits_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Caller supplies the core list — we use a fake fixture
        // entry so the test does not depend on the bundled
        // INVENTORY's exact contents (which evolve over time).
        let core: &[(&str, &str)] = &[("CHAT_AUTH_SKIP_PERM_CHECK", "auth")];

        let (p, _tmp) = discovered(
            "shadow",
            "[[plugin.capability_gates.gate]]\n\
             extension = \"shadow\"\n\
             env_var = \"CHAT_AUTH_SKIP_PERM_CHECK\"\n\
             kind = \"Boolean\"\n\
             risk = \"Low\"\n\
             effect = \"naughty shadow\"\n",
        );
        let snap = snapshot_with(vec![p]);
        let agg = aggregate_plugin_gates(&snap, core, &BTreeSet::new());
        assert_eq!(agg.gates.len(), 0, "plugin gate must be dropped");
        assert!(
            agg.conflicts.iter().any(|d| matches!(
                &d.kind,
                DiscoveryDiagnosticKind::CapabilityGateConflictsCore { env_var, core_extension, .. }
                    if env_var == "CHAT_AUTH_SKIP_PERM_CHECK" && core_extension == "auth"
            )),
            "must emit CapabilityGateConflictsCore"
        );
    }

    #[test]
    fn two_plugins_same_env_var_emits_cross_plugin_conflict() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let extra = |id: &str| {
            format!(
                "[[plugin.capability_gates.gate]]\n\
                 extension = \"{id}\"\n\
                 env_var = \"NEXO_FIXTURE_GATE_DUP\"\n\
                 kind = \"Boolean\"\n\
                 risk = \"Low\"\n\
                 effect = \"dup gate\"\n"
            )
        };
        let (a, _ta) = discovered("alpha", &extra("alpha"));
        let (b, _tb) = discovered("beta", &extra("beta"));
        let snap = snapshot_with(vec![a, b]);
        let agg = aggregate_plugin_gates(&snap, &[], &BTreeSet::new());
        assert_eq!(agg.gates.len(), 1, "first-plugin-wins");
        assert_eq!(
            agg.gates.get("NEXO_FIXTURE_GATE_DUP").unwrap().plugin_id,
            "alpha"
        );
        assert!(
            agg.conflicts.iter().any(|d| matches!(
                &d.kind,
                DiscoveryDiagnosticKind::CapabilityGateConflictsPlugin { plugin_a, plugin_b, .. }
                    if plugin_a == "alpha" && plugin_b == "beta"
            ))
        );
    }

    #[test]
    fn unmet_required_capability_emits_warn_diagnostic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (p, _tmp) = discovered(
            "needy",
            "[plugin.requires]\n\
             nexo_capabilities = [\"long_term_memory\"]\n",
        );
        let snap = snapshot_with(vec![p]);
        let agg = aggregate_plugin_gates(&snap, &[], &BTreeSet::new());
        assert_eq!(agg.unmet_required.len(), 1);
        assert_eq!(agg.unmet_required[0].plugin_id, "needy");
        assert_eq!(agg.unmet_required[0].capability_name, "long_term_memory");
        let diag = agg
            .conflicts
            .iter()
            .find(|d| matches!(d.kind, DiscoveryDiagnosticKind::RequiredCapabilityNotGranted { .. }))
            .expect("RequiredCapabilityNotGranted diagnostic");
        assert_eq!(diag.level, DiagnosticLevel::Warn);

        // When the capability IS available, no diagnostic emitted.
        let mut available = BTreeSet::new();
        available.insert("long_term_memory".to_string());
        let agg_ok = aggregate_plugin_gates(&snap, &[], &available);
        assert!(agg_ok.unmet_required.is_empty());
        assert!(agg_ok.conflicts.is_empty());
    }
}
