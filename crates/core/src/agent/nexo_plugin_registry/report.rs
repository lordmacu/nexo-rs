//! Phase 81.5 — typed report shape produced by [`super::discover`].
//!
//! Every diagnostic kind is its own enum variant so admin-ui + the
//! `nexo agent doctor plugins` CLI can render localized messages
//! without parsing free-form strings.

use std::path::PathBuf;

use semver::Version;
use serde::Serialize;

use nexo_plugin_manifest::PluginManifest;

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
        };
        let s = serde_json::to_string(&report).expect("serialize");
        assert!(s.contains("\"loaded_ids\""));
        assert!(s.contains("\"browser\""));
        assert!(s.contains("\"kind\":\"disabled\""));
        assert!(s.contains("\"level\":\"warn\""));
    }
}
