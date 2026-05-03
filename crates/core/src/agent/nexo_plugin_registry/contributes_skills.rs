//! Phase 81.7 — catalog plugin-contributed skills. Walks each
//! loaded plugin's `skills.contributes_dir`, indexes any subdir
//! containing a `SKILL.md` file, and records the per-plugin root
//! plus first-plugin-wins attribution. Actual skill content is NOT
//! parsed here — `SkillLoader` does that at request time using the
//! roots returned in [`SkillsMergeReport::skill_roots`].
//!
//! Operator-priority is structural: this fn does not see the
//! operator's `skills_dir`. The runtime resolution order lives
//! inside `SkillLoader::candidate_paths`, which searches the
//! operator's tenant/global/legacy chain BEFORE any plugin root.
//! That keeps a malicious / careless plugin from silently
//! replacing an operator-installed skill (skills exec subprocesses;
//! shadowing them would be a security escalation).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use super::report::{
    DiagnosticLevel, DiscoveryDiagnostic, DiscoveryDiagnosticKind,
};
use super::NexoPluginRegistrySnapshot;

#[derive(Clone, Debug, Default, Serialize)]
pub struct SkillsMergeReport {
    /// `plugin_id -> absolute skill root`. The path is the fully-
    /// resolved `<plugin.root_dir>/<skills.contributes_dir>` —
    /// passed verbatim to `SkillLoader::with_plugin_roots`.
    pub skill_roots: BTreeMap<String, PathBuf>,
    /// `skill_name -> plugin_id`. First-plugin-wins on collision —
    /// the FIRST plugin (in snapshot iteration order) whose
    /// contributes_dir contains `<name>/SKILL.md` is the attributed
    /// source. Subsequent plugins still appear in `skill_roots` but
    /// their skill_name does not overwrite the attribution.
    pub attribution: BTreeMap<String, String>,
    /// `plugin_id -> [skill_name, ...]` — every skill this plugin
    /// successfully exposes via its contributes_dir.
    pub contributed_per_plugin: BTreeMap<String, Vec<String>>,
    pub conflicts: Vec<SkillConflict>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillConflict {
    pub skill_name: String,
    /// Plugin ids in iteration order. First entry = attribution
    /// winner (matches the search-order semantic in
    /// `SkillLoader::candidate_paths`).
    pub plugin_ids: Vec<String>,
}

/// Walk every loaded plugin's `skills.contributes_dir`, collect the
/// roots + skill names + conflicts. Pure observation — no skill
/// content parsing happens here; that's `SkillLoader`'s job.
pub fn merge_plugin_contributed_skills(
    snapshot: &NexoPluginRegistrySnapshot,
) -> SkillsMergeReport {
    let mut report = SkillsMergeReport::default();
    // Track first-seen skill names so collisions can be flagged
    // without losing the original attribution.
    let mut conflict_index: BTreeMap<String, usize> = BTreeMap::new();

    for plugin in &snapshot.plugins {
        let plugin_id = plugin.manifest.plugin.id.clone();
        let contributes_dir =
            match &plugin.manifest.plugin.skills.contributes_dir {
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
                        "plugin '{plugin_id}' declared skills.contributes_dir but path does not exist"
                    ),
                },
            });
            continue;
        }

        let entries = match std::fs::read_dir(&abs_dir) {
            Ok(rd) => rd.filter_map(|r| r.ok()).collect::<Vec<_>>(),
            Err(_) => {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: abs_dir.clone(),
                    kind: DiscoveryDiagnosticKind::PermissionDenied,
                });
                continue;
            }
        };

        // Sort by file_name so test fixtures + production runs see
        // deterministic skill order per plugin.
        let mut entries: Vec<_> = entries
            .into_iter()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();
        entries.sort_by_key(|e| e.file_name());

        report.skill_roots.insert(plugin_id.clone(), abs_dir.clone());

        for entry in entries {
            let skill_name = match entry.file_name().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() {
                // Subdirs without SKILL.md are intentionally NOT
                // diagnostics — they could be docs / fixtures /
                // anything else. The plugin author opts into the
                // skill contract by placing a SKILL.md.
                continue;
            }

            report
                .contributed_per_plugin
                .entry(plugin_id.clone())
                .or_default()
                .push(skill_name.clone());

            if let Some(conflict_idx) = conflict_index.get(&skill_name).copied()
            {
                // Append this plugin id to the existing conflict.
                report.conflicts[conflict_idx].plugin_ids.push(plugin_id.clone());
            } else if let Some(prior_plugin) = report.attribution.get(&skill_name)
            {
                let prior = prior_plugin.clone();
                let conflict = SkillConflict {
                    skill_name: skill_name.clone(),
                    plugin_ids: vec![prior, plugin_id.clone()],
                };
                conflict_index.insert(skill_name.clone(), report.conflicts.len());
                report.conflicts.push(conflict);
                // Attribution stays with the FIRST plugin (no
                // overwrite) — matches the search-order semantic.
            } else {
                report
                    .attribution
                    .insert(skill_name.clone(), plugin_id.clone());
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    use nexo_plugin_manifest::PluginManifest;

    use super::super::report::PluginDiscoveryReport;
    use super::super::DiscoveredPlugin;

    /// Fixture plugin tree builder for skills tests.
    struct PluginSkillFixture {
        root: TempDir,
    }

    impl PluginSkillFixture {
        fn new(plugin_id: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let manifest = format!(
                "[plugin]\n\
                 id = \"{plugin_id}\"\n\
                 version = \"0.1.0\"\n\
                 name = \"{plugin_id}\"\n\
                 description = \"fixture\"\n\
                 min_nexo_version = \">=0.0.1\"\n\
                 [plugin.skills]\n\
                 contributes_dir = \"skills\"\n",
            );
            fs::write(root.path().join("nexo-plugin.toml"), manifest).unwrap();
            fs::create_dir_all(root.path().join("skills")).unwrap();
            Self { root }
        }

        fn new_no_skills(plugin_id: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let manifest = format!(
                "[plugin]\n\
                 id = \"{plugin_id}\"\n\
                 version = \"0.1.0\"\n\
                 name = \"{plugin_id}\"\n\
                 description = \"fixture\"\n\
                 min_nexo_version = \">=0.0.1\"\n",
            );
            fs::write(root.path().join("nexo-plugin.toml"), manifest).unwrap();
            Self { root }
        }

        fn new_dangling_dir(plugin_id: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let manifest = format!(
                "[plugin]\n\
                 id = \"{plugin_id}\"\n\
                 version = \"0.1.0\"\n\
                 name = \"{plugin_id}\"\n\
                 description = \"fixture\"\n\
                 min_nexo_version = \">=0.0.1\"\n\
                 [plugin.skills]\n\
                 contributes_dir = \"skills\"\n",
            );
            fs::write(root.path().join("nexo-plugin.toml"), manifest).unwrap();
            // Intentionally NOT creating the skills/ subdir.
            Self { root }
        }

        fn add_skill(&self, name: &str, body: &str) -> &Self {
            let dir = self.root.path().join("skills").join(name);
            fs::create_dir_all(&dir).unwrap();
            let content = format!("---\ndescription: {name}\n---\n\n{body}\n");
            fs::write(dir.join("SKILL.md"), content).unwrap();
            self
        }

        fn add_subdir_without_skill_md(&self, name: &str) -> &Self {
            let dir = self.root.path().join("skills").join(name);
            fs::create_dir_all(&dir).unwrap();
            // Note: NO SKILL.md — should be silently skipped.
            fs::write(dir.join("README.md"), "docs only").unwrap();
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
            skill_roots: BTreeMap::new(),
        }
    }

    #[test]
    fn plugin_skill_loaded_when_agent_requests_it() {
        let p = PluginSkillFixture::new("alpha");
        p.add_skill("triager", "you triage");
        let snap = snapshot_with(vec![p.discovered()]);
        let report = merge_plugin_contributed_skills(&snap);
        assert_eq!(report.skill_roots.len(), 1);
        assert!(report.skill_roots.contains_key("alpha"));
        assert_eq!(
            report.attribution.get("triager"),
            Some(&"alpha".to_string())
        );
        assert_eq!(
            report
                .contributed_per_plugin
                .get("alpha")
                .map(|v| v.as_slice()),
            Some(&["triager".to_string()][..])
        );
        assert!(report.conflicts.is_empty());
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn two_plugins_same_skill_name_emits_conflict_first_wins() {
        let p1 = PluginSkillFixture::new("alpha");
        p1.add_skill("helper", "alpha-content");
        let p2 = PluginSkillFixture::new("beta");
        p2.add_skill("helper", "beta-content");
        let snap = snapshot_with(vec![p1.discovered(), p2.discovered()]);
        let report = merge_plugin_contributed_skills(&snap);
        // Both plugins indexed.
        assert_eq!(report.skill_roots.len(), 2);
        // First plugin wins attribution.
        assert_eq!(
            report.attribution.get("helper"),
            Some(&"alpha".to_string())
        );
        // One conflict recorded.
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].skill_name, "helper");
        assert_eq!(
            report.conflicts[0].plugin_ids,
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn three_plugins_same_skill_name_appends_to_conflict() {
        let p1 = PluginSkillFixture::new("alpha");
        p1.add_skill("dup", "a");
        let p2 = PluginSkillFixture::new("beta");
        p2.add_skill("dup", "b");
        let p3 = PluginSkillFixture::new("gamma");
        p3.add_skill("dup", "c");
        let snap = snapshot_with(vec![
            p1.discovered(),
            p2.discovered(),
            p3.discovered(),
        ]);
        let report = merge_plugin_contributed_skills(&snap);
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(
            report.conflicts[0].plugin_ids,
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
        assert_eq!(report.attribution.get("dup"), Some(&"alpha".to_string()));
    }

    #[test]
    fn plugin_with_no_contributes_dir_no_op() {
        let p = PluginSkillFixture::new_no_skills("quiet");
        let snap = snapshot_with(vec![p.discovered()]);
        let report = merge_plugin_contributed_skills(&snap);
        assert!(report.skill_roots.is_empty());
        assert!(report.attribution.is_empty());
        assert!(report.contributed_per_plugin.is_empty());
        assert!(report.conflicts.is_empty());
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn missing_contributes_dir_emits_search_path_missing() {
        let p = PluginSkillFixture::new_dangling_dir("ghost");
        let snap = snapshot_with(vec![p.discovered()]);
        let report = merge_plugin_contributed_skills(&snap);
        assert!(report.skill_roots.is_empty());
        assert!(
            report.diagnostics.iter().any(|d| matches!(
                &d.kind,
                DiscoveryDiagnosticKind::SearchPathMissing { reason }
                    if reason.contains("ghost")
            ))
        );
    }

    #[test]
    fn subdir_without_skill_md_silently_skipped() {
        let p = PluginSkillFixture::new("alpha");
        p.add_skill("real_skill", "ok")
            .add_subdir_without_skill_md("just_docs");
        let snap = snapshot_with(vec![p.discovered()]);
        let report = merge_plugin_contributed_skills(&snap);
        assert_eq!(
            report.attribution.get("real_skill"),
            Some(&"alpha".to_string())
        );
        // The non-skill subdir does NOT appear anywhere.
        assert!(!report.attribution.contains_key("just_docs"));
        let alpha_skills = report.contributed_per_plugin.get("alpha").unwrap();
        assert!(!alpha_skills.iter().any(|s| s == "just_docs"));
        // Silent — no diagnostic.
        assert!(report.diagnostics.is_empty());
    }
}
