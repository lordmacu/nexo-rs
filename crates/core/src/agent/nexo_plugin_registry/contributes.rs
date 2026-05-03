//! Phase 81.6 — merge plugin-contributed agents into the runtime
//! `AgentsConfig`. Walks each loaded plugin's `agents.contributes_dir`,
//! parses the YAML files, and folds them in honoring operator-priority
//! by load order plus the per-plugin `allow_override` opt-in.
//!
//! Attribution (which plugin contributed which agent_id) lives in
//! [`AgentMergeReport::attribution`] — a sidecar map rather than a
//! field on `AgentConfig` to avoid touching the YAML-facing schema.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use nexo_config::AgentsConfig;

use super::report::{
    DiagnosticLevel, DiscoveryDiagnostic, DiscoveryDiagnosticKind,
};
use super::NexoPluginRegistrySnapshot;

/// Outcome of [`merge_plugin_contributed_agents`]. The caller folds
/// this into the registry's [`super::report::PluginDiscoveryReport`]
/// so the doctor CLI sees a single coherent view.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AgentMergeReport {
    /// `plugin_id -> [agent_id, ...]` — every agent this plugin
    /// successfully contributed to the merged `AgentsConfig`.
    pub contributed_agents_per_plugin: BTreeMap<String, Vec<String>>,
    /// `agent_id -> plugin_id` — reverse lookup so callers can answer
    /// "who contributed this agent?" without re-walking.
    pub attribution: BTreeMap<String, String>,
    pub conflicts: Vec<AgentMergeConflict>,
    /// Diagnostics surfaced during the merge (parse errors, missing
    /// `contributes_dir` on disk). Folded into the registry's
    /// `last_report.diagnostics` by the caller.
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentMergeConflict {
    pub agent_id: String,
    /// Plugin ids in load order (first-seen first). When
    /// `resolution = OperatorWins` only the plugin id appears; the
    /// operator's contribution is implicit (already in `base`).
    pub plugin_ids: Vec<String>,
    pub resolution: MergeResolution,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeResolution {
    /// An entry already existed in `base` (operator-supplied via
    /// `agents.yaml` or `agents.d/`) and the plugin's
    /// `allow_override = false`. Plugin contribution dropped.
    OperatorWins,
    /// Plugin's `agents.allow_override = true`; plugin contribution
    /// replaces any prior entry (operator OR earlier plugin).
    PluginOverrideAccepted,
    /// Two plugins (neither with override) tried to contribute the
    /// same id. Load order wins (last_plugin_wins). Diagnostic
    /// emitted so operator notices the namespace collision.
    LastPluginWins,
}

/// Walk every loaded plugin's `agents.contributes_dir`, parse YAMLs,
/// merge into `base` honoring the operator-priority + per-plugin
/// `allow_override` flag. Caller invokes this AFTER `agents.yaml` +
/// `agents.d/` are already loaded so operator entries pre-populate
/// `base`.
pub fn merge_plugin_contributed_agents(
    snapshot: &NexoPluginRegistrySnapshot,
    base: &mut AgentsConfig,
) -> AgentMergeReport {
    let mut report = AgentMergeReport::default();
    // Track agents introduced by THIS merge pass so we can detect
    // plugin-vs-plugin collisions distinct from plugin-vs-operator.
    let mut plugin_origin: BTreeMap<String, String> = BTreeMap::new();

    for plugin in &snapshot.plugins {
        let plugin_id = plugin.manifest.plugin.id.clone();
        let allow_override = plugin.manifest.plugin.agents.allow_override;
        let contributes_dir = match &plugin.manifest.plugin.agents.contributes_dir
        {
            Some(p) => p.clone(),
            None => continue,
        };

        let abs_dir = plugin.root_dir.join(&contributes_dir);
        if !abs_dir.exists() {
            report.diagnostics.push(DiscoveryDiagnostic {
                level: DiagnosticLevel::Warn,
                path: abs_dir.clone(),
                kind: DiscoveryDiagnosticKind::SearchPathMissing {
                    reason: format!(
                        "plugin '{plugin_id}' declared agents.contributes_dir but path does not exist"
                    ),
                },
            });
            continue;
        }

        let mut yamls: Vec<PathBuf> = match std::fs::read_dir(&abs_dir) {
            Ok(rd) => rd
                .filter_map(|r| r.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yaml"))
                .collect(),
            Err(e) => {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: abs_dir.clone(),
                    kind: DiscoveryDiagnosticKind::PermissionDenied,
                });
                let _ = e;
                continue;
            }
        };
        yamls.sort();

        for yaml_path in yamls {
            let raw = match std::fs::read_to_string(&yaml_path) {
                Ok(s) => s,
                Err(e) => {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Error,
                        path: yaml_path.clone(),
                        kind: DiscoveryDiagnosticKind::ManifestParseError {
                            error: e.to_string(),
                        },
                    });
                    continue;
                }
            };
            let parsed: AgentsConfig = match serde_yaml::from_str(&raw) {
                Ok(v) => v,
                Err(e) => {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Error,
                        path: yaml_path.clone(),
                        kind: DiscoveryDiagnosticKind::ManifestParseError {
                            error: e.to_string(),
                        },
                    });
                    continue;
                }
            };

            for agent in parsed.agents {
                let agent_id = agent.id.clone();
                let prior_idx = base.agents.iter().position(|a| a.id == agent_id);
                let prior_was_plugin = plugin_origin.contains_key(&agent_id);

                match prior_idx {
                    Some(idx) if !prior_was_plugin => {
                        // Operator pre-populated this agent from
                        // agents.yaml / agents.d/.
                        if allow_override {
                            base.agents[idx] = agent;
                            plugin_origin
                                .insert(agent_id.clone(), plugin_id.clone());
                            record_attribution(
                                &mut report,
                                &plugin_id,
                                &agent_id,
                            );
                            report.conflicts.push(AgentMergeConflict {
                                agent_id,
                                plugin_ids: vec![plugin_id.clone()],
                                resolution: MergeResolution::PluginOverrideAccepted,
                            });
                        } else {
                            report.conflicts.push(AgentMergeConflict {
                                agent_id,
                                plugin_ids: vec![plugin_id.clone()],
                                resolution: MergeResolution::OperatorWins,
                            });
                        }
                    }
                    Some(idx) => {
                        // Earlier plugin already contributed this id.
                        let earlier =
                            plugin_origin.get(&agent_id).cloned().unwrap_or_default();
                        base.agents[idx] = agent;
                        plugin_origin.insert(agent_id.clone(), plugin_id.clone());
                        // Re-attribute: agent_id now belongs to current plugin.
                        // Remove from earlier plugin's contributed list.
                        if let Some(list) =
                            report.contributed_agents_per_plugin.get_mut(&earlier)
                        {
                            list.retain(|a| a != &agent_id);
                        }
                        record_attribution(&mut report, &plugin_id, &agent_id);
                        report.conflicts.push(AgentMergeConflict {
                            agent_id,
                            plugin_ids: vec![earlier, plugin_id.clone()],
                            resolution: MergeResolution::LastPluginWins,
                        });
                    }
                    None => {
                        // Fresh contribution — append.
                        base.agents.push(agent);
                        plugin_origin.insert(agent_id.clone(), plugin_id.clone());
                        record_attribution(&mut report, &plugin_id, &agent_id);
                    }
                }
            }
        }
    }

    report
}

fn record_attribution(report: &mut AgentMergeReport, plugin_id: &str, agent_id: &str) {
    report
        .contributed_agents_per_plugin
        .entry(plugin_id.to_string())
        .or_default()
        .push(agent_id.to_string());
    report
        .attribution
        .insert(agent_id.to_string(), plugin_id.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    use nexo_plugin_manifest::PluginManifest;

    use super::super::report::PluginDiscoveryReport;
    use super::super::DiscoveredPlugin;

    /// Helper to build a fixture plugin tree on disk: returns the
    /// `DiscoveredPlugin` value the merge fn would normally receive
    /// from the discover walker.
    struct PluginFixture {
        root: TempDir,
    }

    impl PluginFixture {
        fn new(plugin_id: &str, allow_override: bool) -> Self {
            let root = tempfile::tempdir().unwrap();
            let manifest_str = format!(
                "[plugin]\n\
                 id = \"{plugin_id}\"\n\
                 version = \"0.1.0\"\n\
                 name = \"{plugin_id}\"\n\
                 description = \"fixture\"\n\
                 min_nexo_version = \">=0.0.1\"\n\
                 [plugin.agents]\n\
                 contributes_dir = \"agents\"\n\
                 allow_override = {allow_override}\n",
            );
            fs::write(root.path().join("nexo-plugin.toml"), manifest_str).unwrap();
            fs::create_dir_all(root.path().join("agents")).unwrap();
            Self { root }
        }

        fn add_agent(&self, agent_id: &str, system_prompt: &str) -> &Self {
            let yaml = format!(
                "agents:\n  - id: {agent_id}\n    model:\n      provider: minimax\n      model: minimax-m2.5\n    system_prompt: \"{system_prompt}\"\n"
            );
            fs::write(
                self.root
                    .path()
                    .join("agents")
                    .join(format!("{agent_id}.yaml")),
                yaml,
            )
            .unwrap();
            self
        }

        fn add_malformed_agent(&self, file_name: &str) -> &Self {
            fs::write(
                self.root
                    .path()
                    .join("agents")
                    .join(format!("{file_name}.yaml")),
                "this is :: not valid [yaml ===\n",
            )
            .unwrap();
            self
        }

        fn discovered(&self) -> DiscoveredPlugin {
            let raw = fs::read_to_string(self.root.path().join("nexo-plugin.toml"))
                .unwrap();
            let manifest: PluginManifest = toml::from_str(&raw).unwrap();
            DiscoveredPlugin {
                manifest,
                root_dir: self.root.path().to_path_buf(),
                manifest_path: self.root.path().join("nexo-plugin.toml"),
            }
        }
    }

    fn snapshot_with(plugins: Vec<DiscoveredPlugin>) -> NexoPluginRegistrySnapshot {
        NexoPluginRegistrySnapshot {
            plugins,
            last_report: PluginDiscoveryReport::default(),
            skill_roots: std::collections::BTreeMap::new(),
        }
    }

    fn empty_base() -> AgentsConfig {
        AgentsConfig { agents: Vec::new() }
    }

    fn base_with_operator_agent(agent_id: &str) -> AgentsConfig {
        let yaml = format!(
            "agents:\n  - id: {agent_id}\n    model:\n      provider: minimax\n      model: minimax-m2.5\n    system_prompt: \"operator\"\n"
        );
        serde_yaml::from_str(&yaml).unwrap()
    }

    #[test]
    fn plugin_contributed_agent_loads() {
        let p = PluginFixture::new("alpha", false);
        p.add_agent("triager", "you triage");
        let snap = snapshot_with(vec![p.discovered()]);
        let mut base = empty_base();
        let report = merge_plugin_contributed_agents(&snap, &mut base);
        assert_eq!(base.agents.len(), 1);
        assert_eq!(base.agents[0].id, "triager");
        assert_eq!(report.attribution.get("triager"), Some(&"alpha".to_string()));
        assert_eq!(report.conflicts.len(), 0);
    }

    #[test]
    fn operator_drop_in_wins_over_plugin_default() {
        let p = PluginFixture::new("alpha", false);
        p.add_agent("shared", "plugin-supplied");
        let snap = snapshot_with(vec![p.discovered()]);
        let mut base = base_with_operator_agent("shared");
        let report = merge_plugin_contributed_agents(&snap, &mut base);
        assert_eq!(base.agents.len(), 1);
        assert_eq!(base.agents[0].system_prompt, "operator");
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].resolution, MergeResolution::OperatorWins);
        assert!(!report.attribution.contains_key("shared"));
    }

    #[test]
    fn plugin_with_allow_override_wins_over_operator() {
        let p = PluginFixture::new("alpha", true);
        p.add_agent("shared", "plugin-supplied");
        let snap = snapshot_with(vec![p.discovered()]);
        let mut base = base_with_operator_agent("shared");
        let report = merge_plugin_contributed_agents(&snap, &mut base);
        assert_eq!(base.agents.len(), 1);
        assert_eq!(base.agents[0].system_prompt, "plugin-supplied");
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(
            report.conflicts[0].resolution,
            MergeResolution::PluginOverrideAccepted
        );
        assert_eq!(report.attribution.get("shared"), Some(&"alpha".to_string()));
    }

    #[test]
    fn two_plugins_same_agent_id_emits_conflict() {
        let p1 = PluginFixture::new("alpha", false);
        p1.add_agent("helper", "alpha");
        let p2 = PluginFixture::new("beta", false);
        p2.add_agent("helper", "beta");
        let snap = snapshot_with(vec![p1.discovered(), p2.discovered()]);
        let mut base = empty_base();
        let report = merge_plugin_contributed_agents(&snap, &mut base);
        assert_eq!(base.agents.len(), 1);
        assert_eq!(base.agents[0].system_prompt, "beta");
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(
            report.conflicts[0].resolution,
            MergeResolution::LastPluginWins
        );
        assert_eq!(
            report.conflicts[0].plugin_ids,
            vec!["alpha".to_string(), "beta".to_string()]
        );
        assert_eq!(report.attribution.get("helper"), Some(&"beta".to_string()));
    }

    #[test]
    fn malformed_plugin_agent_yaml_logs_diagnostic_skips_agent() {
        let p = PluginFixture::new("alpha", false);
        p.add_agent("good", "ok").add_malformed_agent("broken");
        let snap = snapshot_with(vec![p.discovered()]);
        let mut base = empty_base();
        let report = merge_plugin_contributed_agents(&snap, &mut base);
        assert_eq!(base.agents.len(), 1);
        assert_eq!(base.agents[0].id, "good");
        assert!(
            report.diagnostics.iter().any(|d| matches!(
                &d.kind,
                DiscoveryDiagnosticKind::ManifestParseError { .. }
            ))
        );
    }

    #[test]
    fn attribution_contains_every_contributed_agent() {
        let p = PluginFixture::new("alpha", false);
        p.add_agent("a1", "x")
            .add_agent("a2", "y")
            .add_agent("a3", "z");
        let snap = snapshot_with(vec![p.discovered()]);
        let mut base = empty_base();
        let report = merge_plugin_contributed_agents(&snap, &mut base);
        assert_eq!(base.agents.len(), 3);
        assert_eq!(report.attribution.len(), 3);
        for id in ["a1", "a2", "a3"] {
            assert_eq!(report.attribution.get(id), Some(&"alpha".to_string()));
        }
        assert_eq!(
            report.contributed_agents_per_plugin.get("alpha").map(|v| v.len()),
            Some(3)
        );
    }
}
