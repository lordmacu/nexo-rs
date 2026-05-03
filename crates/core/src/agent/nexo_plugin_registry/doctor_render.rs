//! Phase 81.9 — render the `NexoPluginRegistrySnapshot` +
//! `ChannelAdapterRegistry` as the human-readable + JSON outputs
//! consumed by `nexo agent doctor plugins`.
//!
//! Stable wire shape — admin-ui (Phase 81.11) reads the same
//! `DoctorPluginsJsonReport` over wire when its `--json` mode
//! ships.

use std::fmt::Write as _;
use std::path::Path;

use semver::Version;
use serde::Serialize;

use super::contributes::MergeResolution;
use super::init_loop::InitOutcome;
use super::report::{DiagnosticLevel, DiscoveryDiagnosticKind, PluginDiscoveryReport};
use super::NexoPluginRegistrySnapshot;
use crate::agent::channel_adapter::ChannelAdapterRegistry;

/// Wire format consumed by `--json` mode of
/// `nexo agent doctor plugins`. Stable serde shape so admin-ui
/// (Phase 81.11) decodes the same struct.
#[derive(Debug, Serialize)]
pub struct DoctorPluginsJsonReport<'a> {
    pub config_path: &'a Path,
    pub daemon_version: String,
    pub snapshot_report: &'a PluginDiscoveryReport,
    pub channel_kinds: Vec<String>,
}

/// CLI exit code per spec rules:
/// `0` if no Error-level diagnostics, no `LastPluginWins`
///     conflicts, no `Failed` init outcomes;
/// `1` otherwise.
pub fn determine_exit_code(snap: &NexoPluginRegistrySnapshot) -> i32 {
    let report = &snap.last_report;
    let has_error_diag = report
        .diagnostics
        .iter()
        .any(|d| matches!(d.level, DiagnosticLevel::Error));
    let has_last_plugin_wins = report
        .agent_merge_conflicts
        .iter()
        .any(|c| matches!(c.resolution, MergeResolution::LastPluginWins));
    let has_failed_init = report
        .init_outcomes
        .values()
        .any(|o| matches!(o, InitOutcome::Failed { .. }));
    if has_error_diag || has_last_plugin_wins || has_failed_init {
        1
    } else {
        0
    }
}

/// Render the snapshot + channel registry as a multi-section text
/// report for the doctor CLI. Output ends without a trailing
/// newline (caller decides whether to add one).
pub fn render_text(
    snap: &NexoPluginRegistrySnapshot,
    channels: &ChannelAdapterRegistry,
    config_path: &Path,
    daemon_version: &Version,
) -> String {
    let report = &snap.last_report;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "PLUGIN DISCOVERY REPORT  (config={}, daemon={daemon_version})",
        config_path.display()
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "SCANNED {}  LOADED {}  INVALID {}  DISABLED {}  DUPLICATES {}",
        report.scanned,
        report.loaded_ids.len(),
        report.invalid,
        report.disabled,
        report.duplicates,
    );
    let _ = writeln!(out);

    // LOADED PLUGINS
    let _ = writeln!(out, "LOADED PLUGINS");
    if snap.plugins.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for plugin in &snap.plugins {
            let display = plugin
                .root_dir
                .strip_prefix(config_path)
                .unwrap_or(&plugin.root_dir)
                .display();
            let _ = writeln!(
                out,
                "  ✅ {}  v{}  {}",
                plugin.manifest.plugin.id, plugin.manifest.plugin.version, display
            );
        }
    }
    let _ = writeln!(out);

    // DIAGNOSTICS
    let _ = writeln!(out, "DIAGNOSTICS");
    if report.diagnostics.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for d in &report.diagnostics {
            let emoji = match d.level {
                DiagnosticLevel::Warn => "⚠️",
                DiagnosticLevel::Error => "❌",
            };
            let summary = diagnostic_summary(&d.kind);
            let kind_tag = diagnostic_kind_tag(&d.kind);
            let _ = writeln!(out, "  {emoji} {kind_tag}  {summary}");
        }
    }
    let _ = writeln!(out);

    // PLUGIN AGENTS CONTRIBUTED
    let _ = writeln!(out, "PLUGIN AGENTS CONTRIBUTED");
    if report.contributed_agents_per_plugin.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for (plugin_id, agents) in &report.contributed_agents_per_plugin {
            let _ = writeln!(
                out,
                "  {plugin_id}: {} agents ({})",
                agents.len(),
                agents.join(", ")
            );
        }
    }
    let _ = writeln!(out);

    // AGENT MERGE CONFLICTS
    let _ = writeln!(out, "AGENT MERGE CONFLICTS");
    if report.agent_merge_conflicts.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for c in &report.agent_merge_conflicts {
            let emoji = match c.resolution {
                MergeResolution::OperatorWins => "⚠️",
                MergeResolution::PluginOverrideAccepted => "⚠️",
                MergeResolution::LastPluginWins => "❌",
            };
            let resolution = match c.resolution {
                MergeResolution::OperatorWins => "operator_wins",
                MergeResolution::PluginOverrideAccepted => "plugin_override_accepted",
                MergeResolution::LastPluginWins => "last_plugin_wins",
            };
            let _ = writeln!(
                out,
                "  {emoji} {}  plugins=[{}]  resolution={resolution}",
                c.agent_id,
                c.plugin_ids.join(", ")
            );
        }
    }
    let _ = writeln!(out);

    // PLUGIN INIT OUTCOMES
    let _ = writeln!(out, "PLUGIN INIT OUTCOMES");
    if report.init_outcomes.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for (plugin_id, outcome) in &report.init_outcomes {
            match outcome {
                InitOutcome::Ok { duration_ms } => {
                    let _ = writeln!(
                        out,
                        "  ✅ {plugin_id}  ok  {duration_ms}ms"
                    );
                }
                InitOutcome::Failed { error } => {
                    let _ = writeln!(out, "  ❌ {plugin_id}  failed  {error}");
                }
                InitOutcome::NoHandle => {
                    let _ = writeln!(
                        out,
                        "  ⏸  {plugin_id}  no_handle  (manifest-driven instantiation lands in 81.12)"
                    );
                }
            }
        }
    }
    let _ = writeln!(out);

    // PLUGIN SKILLS CONTRIBUTED
    let _ = writeln!(out, "PLUGIN SKILLS CONTRIBUTED");
    if report.contributed_skills_per_plugin.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for (plugin_id, skills) in &report.contributed_skills_per_plugin {
            let _ = writeln!(
                out,
                "  {plugin_id}: {} skills ({})",
                skills.len(),
                skills.join(", ")
            );
        }
    }
    let _ = writeln!(out);

    // SKILL CONFLICTS
    let _ = writeln!(out, "SKILL CONFLICTS");
    if report.skill_conflicts.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for c in &report.skill_conflicts {
            let resolved = c.plugin_ids.first().cloned().unwrap_or_default();
            let _ = writeln!(
                out,
                "  ⚠️  {}  contributed by [{}]  resolved={resolved}",
                c.skill_name,
                c.plugin_ids.join(", ")
            );
        }
    }
    let _ = writeln!(out);

    // CHANNEL ADAPTERS
    let _ = writeln!(out, "CHANNEL ADAPTERS");
    let kinds = channels.kinds();
    if kinds.is_empty() {
        let _ = writeln!(
            out,
            "  (none registered — manifest-driven plugin instantiation lands in 81.12)"
        );
    } else {
        for kind in kinds {
            let _ = writeln!(out, "  ✅ {kind}");
        }
    }
    let _ = writeln!(out);

    // EXIT
    let _ = writeln!(out, "EXIT {}", determine_exit_code(snap));
    out
}

/// JSON-mode output (single line, valid JSON).
pub fn render_json(
    snap: &NexoPluginRegistrySnapshot,
    channels: &ChannelAdapterRegistry,
    config_path: &Path,
    daemon_version: &Version,
) -> String {
    let report = DoctorPluginsJsonReport {
        config_path,
        daemon_version: daemon_version.to_string(),
        snapshot_report: &snap.last_report,
        channel_kinds: channels.kinds(),
    };
    serde_json::to_string(&report).unwrap_or_else(|e| {
        format!("{{\"error\":\"render_json failed: {e}\"}}")
    })
}

fn diagnostic_kind_tag(kind: &DiscoveryDiagnosticKind) -> &'static str {
    match kind {
        DiscoveryDiagnosticKind::SearchPathMissing { .. } => "search_path_missing",
        DiscoveryDiagnosticKind::ManifestParseError { .. } => "manifest_parse_error",
        DiscoveryDiagnosticKind::ValidationFailed { .. } => "validation_failed",
        DiscoveryDiagnosticKind::SymlinkEscape { .. } => "symlink_escape",
        DiscoveryDiagnosticKind::PermissionDenied => "permission_denied",
        DiscoveryDiagnosticKind::DuplicateId { .. } => "duplicate_id",
        DiscoveryDiagnosticKind::VersionMismatch { .. } => "version_mismatch",
        DiscoveryDiagnosticKind::Disabled { .. } => "disabled",
        DiscoveryDiagnosticKind::AllowlistRejected { .. } => "allowlist_rejected",
        DiscoveryDiagnosticKind::UnresolvedEnvVar { .. } => "unresolved_env_var",
        DiscoveryDiagnosticKind::ChannelKindAlreadyRegistered { .. } => {
            "channel_kind_already_registered"
        }
    }
}

fn diagnostic_summary(kind: &DiscoveryDiagnosticKind) -> String {
    match kind {
        DiscoveryDiagnosticKind::SearchPathMissing { reason } => reason.clone(),
        DiscoveryDiagnosticKind::ManifestParseError { error } => error.clone(),
        DiscoveryDiagnosticKind::ValidationFailed { errors } => errors.join("; "),
        DiscoveryDiagnosticKind::SymlinkEscape { target } => {
            format!("target={}", target.display())
        }
        DiscoveryDiagnosticKind::PermissionDenied => "permission denied".to_string(),
        DiscoveryDiagnosticKind::DuplicateId { id, kept_path } => {
            format!("id={id}  kept_path={}", kept_path.display())
        }
        DiscoveryDiagnosticKind::VersionMismatch {
            id,
            required,
            current,
        } => format!("{id} requires {required} (current {current})"),
        DiscoveryDiagnosticKind::Disabled { id } => id.clone(),
        DiscoveryDiagnosticKind::AllowlistRejected { id } => id.clone(),
        DiscoveryDiagnosticKind::UnresolvedEnvVar { var_name, in_path } => {
            format!("var={var_name} in={}", in_path.display())
        }
        DiscoveryDiagnosticKind::ChannelKindAlreadyRegistered {
            channel_kind,
            prior_registered_by,
            attempted_by,
        } => format!(
            "kind={channel_kind} prior={prior_registered_by} attempted={attempted_by}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::super::contributes::{AgentMergeConflict, MergeResolution};
    use super::super::report::{
        DiagnosticLevel, DiscoveryDiagnostic, DiscoveryDiagnosticKind,
        PluginDiscoveryReport,
    };
    use super::super::DiscoveredPlugin;
    use nexo_plugin_manifest::PluginManifest;

    fn empty_snapshot() -> NexoPluginRegistrySnapshot {
        NexoPluginRegistrySnapshot {
            plugins: Vec::new(),
            last_report: PluginDiscoveryReport::default(),
            skill_roots: BTreeMap::new(),
        }
    }

    fn version() -> Version {
        Version::parse("0.1.0").unwrap()
    }

    fn discovered(plugin_id: &str) -> DiscoveredPlugin {
        let raw = format!(
            "[plugin]\nid = \"{plugin_id}\"\nversion = \"0.1.0\"\nname = \"{plugin_id}\"\ndescription = \"x\"\nmin_nexo_version = \">=0.0.1\"\n"
        );
        let manifest: PluginManifest = toml::from_str(&raw).unwrap();
        DiscoveredPlugin {
            manifest,
            root_dir: PathBuf::from("/tmp/p"),
            manifest_path: PathBuf::from("/tmp/p/nexo-plugin.toml"),
        }
    }

    #[test]
    fn empty_snapshot_renders_all_sections_with_none_placeholder() {
        let snap = empty_snapshot();
        let channels = ChannelAdapterRegistry::new();
        let out = render_text(&snap, &channels, Path::new("/cfg"), &version());
        for header in [
            "PLUGIN DISCOVERY REPORT",
            "LOADED PLUGINS",
            "DIAGNOSTICS",
            "PLUGIN AGENTS CONTRIBUTED",
            "AGENT MERGE CONFLICTS",
            "PLUGIN INIT OUTCOMES",
            "PLUGIN SKILLS CONTRIBUTED",
            "SKILL CONFLICTS",
            "CHANNEL ADAPTERS",
        ] {
            assert!(out.contains(header), "missing header: {header}\n---\n{out}");
        }
        assert!(out.contains("(none)"));
        assert!(out.contains(
            "(none registered — manifest-driven plugin instantiation lands in 81.12)"
        ));
        assert!(out.contains("EXIT 0"));
    }

    #[test]
    fn loaded_section_lists_each_plugin_with_path() {
        let mut snap = empty_snapshot();
        snap.plugins = vec![discovered("alpha"), discovered("beta")];
        snap.last_report.loaded_ids = vec!["alpha".into(), "beta".into()];
        let channels = ChannelAdapterRegistry::new();
        let out = render_text(&snap, &channels, Path::new("/cfg"), &version());
        assert!(out.contains("✅ alpha  v0.1.0"));
        assert!(out.contains("✅ beta  v0.1.0"));
    }

    #[test]
    fn diagnostics_section_groups_by_level_and_renders_emoji() {
        let mut snap = empty_snapshot();
        snap.last_report.diagnostics = vec![
            DiscoveryDiagnostic {
                level: DiagnosticLevel::Warn,
                path: PathBuf::from("/x"),
                kind: DiscoveryDiagnosticKind::Disabled { id: "alpha".into() },
            },
            DiscoveryDiagnostic {
                level: DiagnosticLevel::Error,
                path: PathBuf::from("/y"),
                kind: DiscoveryDiagnosticKind::ManifestParseError {
                    error: "bad toml".into(),
                },
            },
        ];
        let channels = ChannelAdapterRegistry::new();
        let out = render_text(&snap, &channels, Path::new("/cfg"), &version());
        assert!(out.contains("⚠️ disabled"));
        assert!(out.contains("❌ manifest_parse_error"));
        assert!(out.contains("bad toml"));
    }

    #[test]
    fn agent_conflicts_section_uses_resolution_emoji() {
        let mut snap = empty_snapshot();
        snap.last_report.agent_merge_conflicts = vec![
            AgentMergeConflict {
                agent_id: "a1".into(),
                plugin_ids: vec!["p1".into()],
                resolution: MergeResolution::OperatorWins,
            },
            AgentMergeConflict {
                agent_id: "a2".into(),
                plugin_ids: vec!["p2".into()],
                resolution: MergeResolution::PluginOverrideAccepted,
            },
            AgentMergeConflict {
                agent_id: "a3".into(),
                plugin_ids: vec!["p3".into(), "p4".into()],
                resolution: MergeResolution::LastPluginWins,
            },
        ];
        let channels = ChannelAdapterRegistry::new();
        let out = render_text(&snap, &channels, Path::new("/cfg"), &version());
        assert!(out.contains("a1") && out.contains("operator_wins"));
        assert!(out.contains("a2") && out.contains("plugin_override_accepted"));
        assert!(out.contains("a3") && out.contains("last_plugin_wins"));
        // last_plugin_wins gets ❌
        assert!(out.contains("❌ a3"));
    }

    #[test]
    fn init_outcomes_section_shows_duration_for_ok_and_error_for_failed() {
        let mut snap = empty_snapshot();
        let mut outcomes: BTreeMap<String, InitOutcome> = BTreeMap::new();
        outcomes.insert("p1".into(), InitOutcome::Ok { duration_ms: 12 });
        outcomes.insert(
            "p2".into(),
            InitOutcome::Failed {
                error: "boom".into(),
            },
        );
        outcomes.insert("p3".into(), InitOutcome::NoHandle);
        snap.last_report.init_outcomes = outcomes;
        let channels = ChannelAdapterRegistry::new();
        let out = render_text(&snap, &channels, Path::new("/cfg"), &version());
        assert!(out.contains("✅ p1  ok  12ms"));
        assert!(out.contains("❌ p2  failed  boom"));
        assert!(out.contains("⏸  p3  no_handle"));
    }

    #[test]
    fn exit_code_one_when_any_error_or_lastpluginwins_or_failed() {
        let clean = empty_snapshot();
        assert_eq!(determine_exit_code(&clean), 0);

        let mut error_diag = empty_snapshot();
        error_diag.last_report.diagnostics.push(DiscoveryDiagnostic {
            level: DiagnosticLevel::Error,
            path: PathBuf::from("/x"),
            kind: DiscoveryDiagnosticKind::PermissionDenied,
        });
        assert_eq!(determine_exit_code(&error_diag), 1);

        let mut last_wins = empty_snapshot();
        last_wins
            .last_report
            .agent_merge_conflicts
            .push(AgentMergeConflict {
                agent_id: "a".into(),
                plugin_ids: vec!["p1".into(), "p2".into()],
                resolution: MergeResolution::LastPluginWins,
            });
        assert_eq!(determine_exit_code(&last_wins), 1);

        let mut failed = empty_snapshot();
        failed.last_report.init_outcomes.insert(
            "p1".into(),
            InitOutcome::Failed {
                error: "x".into(),
            },
        );
        assert_eq!(determine_exit_code(&failed), 1);

        let mut warn_only = empty_snapshot();
        warn_only.last_report.diagnostics.push(DiscoveryDiagnostic {
            level: DiagnosticLevel::Warn,
            path: PathBuf::from("/x"),
            kind: DiscoveryDiagnosticKind::Disabled { id: "x".into() },
        });
        assert_eq!(determine_exit_code(&warn_only), 0);
    }

    #[test]
    fn render_json_contains_loaded_ids_and_channel_kinds() {
        let mut snap = empty_snapshot();
        snap.last_report.loaded_ids = vec!["alpha".into()];
        let channels = ChannelAdapterRegistry::new();
        let out = render_json(&snap, &channels, Path::new("/cfg"), &version());
        assert!(out.contains("\"loaded_ids\""));
        assert!(out.contains("\"alpha\""));
        assert!(out.contains("\"channel_kinds\""));
    }

    #[test]
    fn render_text_ends_with_exit_line() {
        let snap = empty_snapshot();
        let channels = ChannelAdapterRegistry::new();
        let out = render_text(&snap, &channels, Path::new("/cfg"), &version());
        assert!(
            out.contains("\nEXIT 0\n") || out.ends_with("EXIT 0\n"),
            "output must include EXIT line: {out}"
        );
    }
}
