//! Phase 81.5 — typed report shape produced by [`super::discover`].
//!
//! Every diagnostic kind is its own enum variant so admin-ui + the
//! `nexo agent doctor plugins` CLI can render localized messages
//! without parsing free-form strings.
//!
//! Phase 81.6 extends this with three fields tracking plugin-
//! contributed agents, merge conflicts, and `NexoPlugin::init()`
//! outcomes. All three are `#[serde(default, skip_serializing_if)]`
//! so the wire format stays backward-compatible with 81.5 consumers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use semver::Version;
use serde::Serialize;

use nexo_plugin_manifest::PluginManifest;

use super::capability_aggregator::{
    AggregatedGate, PluginCapabilityAggregation, UnmetRequirement,
};
use super::contributes::{AgentMergeConflict, AgentMergeReport};
use super::contributes_skills::{SkillConflict, SkillsMergeReport};
use super::init_loop::InitOutcome;

/// One validated plugin manifest plus the on-disk paths used to find
/// it. The `manifest` field carries the full schema parsed by
/// `nexo-plugin-manifest`; consumers (Phase 81.6 onward) instantiate
/// the plugin from this record.
#[derive(Clone, Debug, Serialize)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub root_dir: PathBuf,
    pub manifest_path: PathBuf,
}

/// Audit summary returned by [`super::discover`]. Lightweight + serde
/// so an admin-ui caller can fetch it over wire later (Phase 81.11).
#[derive(Clone, Debug, Default, Serialize)]
pub struct PluginDiscoveryReport {
    pub loaded_ids: Vec<String>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    pub scanned: usize,
    pub invalid: usize,
    pub disabled: usize,
    pub duplicates: usize,
    /// Phase 81.6 — `plugin_id -> [agent_id, ...]` populated by the
    /// merge step. Empty when no plugins contribute agents.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contributed_agents_per_plugin: BTreeMap<String, Vec<String>>,
    /// Phase 81.6 — collisions detected during the merge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_merge_conflicts: Vec<AgentMergeConflict>,
    /// Phase 81.6 — `plugin_id -> outcome` populated by the
    /// `NexoPlugin::init()` driver.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub init_outcomes: BTreeMap<String, InitOutcome>,
    /// Phase 81.7 — `plugin_id -> [skill_name, ...]` populated by
    /// the skill merge step.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contributed_skills_per_plugin: BTreeMap<String, Vec<String>>,
    /// Phase 81.7 — same-skill-name collisions across plugins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skill_conflicts: Vec<SkillConflict>,
    /// Phase 81.11 — plugin-declared capability gates aggregated
    /// at boot. Keyed by `env_var`. Drift-prevention contract
    /// preserved: `nexo_setup::capabilities::INVENTORY` stays
    /// immutable; this map is the parallel plugin view.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_capability_gates: BTreeMap<String, AggregatedGate>,
    /// Phase 81.11 — `requires.nexo_capabilities` entries that the
    /// running daemon does not provide. Warn-level (graceful
    /// degradation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmet_required_capabilities: Vec<UnmetRequirement>,
}

impl PluginDiscoveryReport {
    /// Phase 81.6 — fold a [`super::contributes::AgentMergeReport`]
    /// into this report. Diagnostics are appended; the
    /// `contributed_agents_per_plugin` map is replaced verbatim;
    /// conflicts are appended.
    pub fn fold_agent_merge(&mut self, m: AgentMergeReport) {
        self.diagnostics.extend(m.diagnostics);
        self.contributed_agents_per_plugin = m.contributed_agents_per_plugin;
        self.agent_merge_conflicts.extend(m.conflicts);
    }

    /// Phase 81.6 — fold the result of
    /// [`super::init_loop::run_plugin_init_loop`] into this report.
    pub fn fold_init_outcomes(&mut self, outcomes: BTreeMap<String, InitOutcome>) {
        self.init_outcomes = outcomes;
    }

    /// Phase 81.7 — fold a [`super::contributes_skills::SkillsMergeReport`]
    /// into this report. Diagnostics are appended; the
    /// `contributed_skills_per_plugin` map is replaced verbatim;
    /// conflicts are appended. The `skill_roots` + `attribution`
    /// maps live on `NexoPluginRegistrySnapshot.skill_roots` (set by
    /// the caller), not in this audit report.
    pub fn fold_skill_merge(&mut self, m: SkillsMergeReport) {
        self.diagnostics.extend(m.diagnostics);
        self.contributed_skills_per_plugin = m.contributed_per_plugin;
        self.skill_conflicts.extend(m.conflicts);
    }

    /// Phase 81.11 — fold a [`PluginCapabilityAggregation`] into
    /// this report. Diagnostics appended; `plugin_capability_gates`
    /// replaced verbatim; `unmet_required_capabilities` extended.
    pub fn fold_capability_aggregation(&mut self, agg: PluginCapabilityAggregation) {
        self.diagnostics.extend(agg.conflicts);
        self.plugin_capability_gates = agg.gates;
        self.unmet_required_capabilities.extend(agg.unmet_required);
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DiscoveryDiagnostic {
    pub level: DiagnosticLevel,
    pub path: PathBuf,
    pub kind: DiscoveryDiagnosticKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscoveryDiagnosticKind {
    SearchPathMissing { reason: String },
    ManifestParseError { error: String },
    ValidationFailed { errors: Vec<String> },
    SymlinkEscape { target: PathBuf },
    PermissionDenied,
    DuplicateId { id: String, kept_path: PathBuf },
    VersionMismatch { id: String, required: String, current: Version },
    Disabled { id: String },
    AllowlistRejected { id: String },
    UnresolvedEnvVar { var_name: String, in_path: PathBuf },
    /// Phase 81.8 — two plugins both tried to register the same
    /// channel kind. The first plugin's adapter is live; the
    /// later plugin's adapter is rejected. Other registrations
    /// (tools / advisors / hooks) by the rejected plugin are not
    /// affected. Field is `channel_kind` (not `kind`) because the
    /// outer enum already uses `kind` as its serde discriminator.
    ChannelKindAlreadyRegistered {
        channel_kind: String,
        prior_registered_by: String,
        attempted_by: String,
    },
    /// Phase 81.11 — plugin's manifest declares a `[plugin.capability_gates.gate]`
    /// whose `env_var` is already in `nexo_setup::capabilities::INVENTORY`.
    /// Plugin gate dropped (operator must disable one).
    CapabilityGateConflictsCore {
        env_var: String,
        plugin_id: String,
        core_extension: String,
    },
    /// Phase 81.11 — two plugins declare the same `env_var` in
    /// their `[plugin.capability_gates.gate]` arrays.
    /// First-plugin-wins; second's gate dropped.
    CapabilityGateConflictsPlugin {
        env_var: String,
        plugin_a: String,
        plugin_b: String,
    },
    /// Phase 81.11 — plugin declares a `requires.nexo_capabilities`
    /// entry that the running daemon does not provide. Warn-level:
    /// operator may run degraded; plugin's own `init()` may still
    /// reject when manifest-driven factory ships.
    RequiredCapabilityNotGranted {
        plugin_id: String,
        capability_name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_serializes_to_json() {
        let report = PluginDiscoveryReport {
            loaded_ids: vec!["browser".into(), "email".into()],
            diagnostics: vec![DiscoveryDiagnostic {
                level: DiagnosticLevel::Warn,
                path: PathBuf::from("/tmp/plugins/whatsapp/nexo-plugin.toml"),
                kind: DiscoveryDiagnosticKind::Disabled {
                    id: "whatsapp".into(),
                },
            }],
            scanned: 3,
            invalid: 0,
            disabled: 1,
            duplicates: 0,
            ..Default::default()
        };
        let s = serde_json::to_string(&report).expect("serialize");
        assert!(s.contains("\"loaded_ids\""));
        assert!(s.contains("\"browser\""));
        assert!(s.contains("\"kind\":\"disabled\""));
        assert!(s.contains("\"level\":\"warn\""));
    }
}
