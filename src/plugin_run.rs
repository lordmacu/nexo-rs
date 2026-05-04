//! Phase 31.7 — `nexo plugin run <path>` local dev loop.
//!
//! Boots the daemon with a local plugin directory injected as
//! `cfg.plugins.discovery.search_paths[0]`, bypassing the
//! install + verify pipeline. Used during inner-loop dev to
//! skip publish + cosign + sha256 round-trips.
//!
//! Implementation: validates the path + manifest pre-boot, sets
//! a side-channel override on `CliArgs`, falls through to the
//! existing `Mode::Run` boot path. The daemon's existing
//! subprocess auto-fallback (Phase 81.17.b) handles the spawn
//! identically to a normally-installed plugin; hot-reload
//! (Phase 81.10) re-walks `search_paths` on every tick so
//! `cargo build --release` triggers a respawn.

use std::path::{Path, PathBuf};

use nexo_config::AppConfig;
use nexo_plugin_manifest::PluginManifest;
use serde::Serialize;

const MANIFEST_FILE: &str = "nexo-plugin.toml";

/// Pre-boot resolution result. The dispatch handler stamps this
/// onto `CliArgs`; the daemon boot path applies it to the loaded
/// `AppConfig` via [`apply_override`].
#[derive(Debug, Clone)]
pub struct PluginRunOverride {
    pub plugin_root: PathBuf,
    pub manifest_path: PathBuf,
    pub plugin_id: String,
    pub plugin_version: String,
    pub no_daemon_config: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginRunError {
    #[error("path `{}` does not exist", path.display())]
    PathNotFound { path: PathBuf },

    #[error(
        "path `{}` is neither a directory containing nexo-plugin.toml nor a manifest file",
        path.display()
    )]
    NotAPluginPath { path: PathBuf },

    #[error("manifest at `{}` failed to parse: {reason}", path.display())]
    ManifestInvalid { path: PathBuf, reason: String },

    #[error(
        "manifest at `{}` has no [plugin.entrypoint] command — cannot spawn",
        path.display()
    )]
    MissingEntrypoint { path: PathBuf },

    #[error("--watch deferred to Phase 31.7.b — re-run without --watch")]
    WatchDeferred,

    #[error("io error: {0}")]
    Io(String),
}

pub fn plugin_run_error_kind(e: &PluginRunError) -> &'static str {
    match e {
        PluginRunError::PathNotFound { .. } => "PathNotFound",
        PluginRunError::NotAPluginPath { .. } => "NotAPluginPath",
        PluginRunError::ManifestInvalid { .. } => "ManifestInvalid",
        PluginRunError::MissingEntrypoint { .. } => "MissingEntrypoint",
        PluginRunError::WatchDeferred => "WatchDeferred",
        PluginRunError::Io(_) => "Io",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginRunReport {
    pub ok: bool,
    pub id: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub plugin_root: PathBuf,
    pub no_daemon_config: bool,
    pub next: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginRunErrorReport {
    pub ok: bool,
    pub kind: &'static str,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// Resolve `<path>` to a manifest + plugin root; validate.
/// No filesystem mutation. The dispatch handler applies the
/// returned override to the loaded `AppConfig` before daemon
/// boot.
pub fn resolve_local_plugin(
    raw_path: &Path,
    no_daemon_config: bool,
) -> Result<PluginRunOverride, PluginRunError> {
    if !raw_path.exists() {
        return Err(PluginRunError::PathNotFound {
            path: raw_path.to_path_buf(),
        });
    }
    let abs = std::fs::canonicalize(raw_path)
        .map_err(|e| PluginRunError::Io(format!("canonicalize {}: {e}", raw_path.display())))?;

    let (plugin_root, manifest_path) = if abs.is_file() {
        if abs.file_name().and_then(|n| n.to_str()) != Some(MANIFEST_FILE) {
            return Err(PluginRunError::NotAPluginPath { path: abs });
        }
        let parent = abs
            .parent()
            .ok_or_else(|| PluginRunError::NotAPluginPath { path: abs.clone() })?
            .to_path_buf();
        (parent, abs)
    } else if abs.is_dir() {
        let candidate = abs.join(MANIFEST_FILE);
        if !candidate.is_file() {
            return Err(PluginRunError::NotAPluginPath { path: abs });
        }
        (abs, candidate)
    } else {
        return Err(PluginRunError::NotAPluginPath { path: abs });
    };

    let body = std::fs::read_to_string(&manifest_path).map_err(|e| {
        PluginRunError::ManifestInvalid {
            path: manifest_path.clone(),
            reason: format!("read failed: {e}"),
        }
    })?;
    let manifest = PluginManifest::from_str(&body).map_err(|e| {
        PluginRunError::ManifestInvalid {
            path: manifest_path.clone(),
            reason: e.to_string(),
        }
    })?;

    let entrypoint_command = manifest
        .plugin
        .entrypoint
        .command
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    if entrypoint_command.is_empty() {
        return Err(PluginRunError::MissingEntrypoint {
            path: manifest_path,
        });
    }

    Ok(PluginRunOverride {
        plugin_root,
        manifest_path,
        plugin_id: manifest.plugin.id.clone(),
        plugin_version: manifest.plugin.version.to_string(),
        no_daemon_config,
    })
}

/// Apply the override to a loaded `AppConfig`. Mutates:
/// - prepends `plugin_root` to `cfg.plugins.discovery.search_paths`
/// - if `no_daemon_config`, clears `cfg.agents.agents`
pub fn apply_override(cfg: &mut AppConfig, override_: &PluginRunOverride) {
    apply_search_path_prepend(
        &mut cfg.plugins.discovery.search_paths,
        &override_.plugin_root,
    );
    if override_.no_daemon_config {
        cfg.agents.agents.clear();
    }
}

/// Idempotent prepend — exposed for unit tests so the override
/// logic can be exercised without constructing a full `AppConfig`
/// (which has no `Default` impl).
fn apply_search_path_prepend(search_paths: &mut Vec<PathBuf>, plugin_root: &Path) {
    let already_at_head = search_paths
        .first()
        .map(|p| p == plugin_root)
        .unwrap_or(false);
    if !already_at_head {
        search_paths.insert(0, plugin_root.to_path_buf());
    }
}

/// Print a human-mode pre-boot banner to stderr, or a single
/// JSON report to stdout. Called once before daemon boot starts
/// emitting log lines.
pub fn print_pre_boot_banner(override_: &PluginRunOverride, json: bool) {
    if json {
        let report = PluginRunReport {
            ok: true,
            id: override_.plugin_id.clone(),
            version: override_.plugin_version.clone(),
            manifest_path: override_.manifest_path.clone(),
            plugin_root: override_.plugin_root.clone(),
            no_daemon_config: override_.no_daemon_config,
            next: "daemon-boot",
        };
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
        return;
    }
    eprintln!(
        "→ Resolving local plugin at {}",
        override_.plugin_root.display()
    );
    eprintln!(
        "✓ Manifest valid: {}@{}",
        override_.plugin_id, override_.plugin_version
    );
    eprintln!(
        "✓ Booting daemon with {} in search_paths[0]",
        override_.plugin_id
    );
    if override_.no_daemon_config {
        eprintln!(
            "→ Standalone mode: cfg.agents cleared; plugin runs for inspection"
        );
    }
}

/// Render an error in human or JSON mode. Returns the exit code
/// the caller should propagate.
pub fn emit_error(err: &PluginRunError, json: bool, path: Option<PathBuf>) -> i32 {
    let kind = plugin_run_error_kind(err);
    if json {
        let report = PluginRunErrorReport {
            ok: false,
            kind,
            error: err.to_string(),
            path,
        };
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        eprintln!("✗ Plugin run failed: {}", err);
        match err {
            PluginRunError::PathNotFound { .. } => {
                eprintln!("  Hint: pass a path to a plugin directory or its nexo-plugin.toml file.");
            }
            PluginRunError::NotAPluginPath { .. } => {
                eprintln!("  Hint: the directory must contain a nexo-plugin.toml at its root.");
            }
            PluginRunError::MissingEntrypoint { .. } => {
                eprintln!(
                    "  Hint: declare `[plugin.entrypoint] command = \"./bin/<id>\"` in the manifest."
                );
            }
            PluginRunError::WatchDeferred => {
                eprintln!(
                    "  Hint: re-run without --watch. File-watching ships in Phase 31.7.b."
                );
            }
            _ => {}
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_minimal_manifest(path: &Path, id: &str) {
        fs::write(
            path,
            format!(
                r#"
[plugin]
id = "{id}"
version = "0.1.0"
name = "Local Test Plugin"
description = "A local test plugin"
min_nexo_version = ">=0.1.0"

[plugin.entrypoint]
command = "./bin/{id}"
args = []
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn path_to_manifest_resolves_dir_with_manifest() {
        let tmp = TempDir::new().unwrap();
        let plugin_dir = tmp.path().join("local_x");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_minimal_manifest(&plugin_dir.join("nexo-plugin.toml"), "local_x");

        let override_ = resolve_local_plugin(&plugin_dir, false).expect("resolve ok");
        assert_eq!(override_.plugin_id, "local_x");
        assert_eq!(override_.plugin_version, "0.1.0");
        assert_eq!(override_.plugin_root, plugin_dir.canonicalize().unwrap());
        assert_eq!(
            override_.manifest_path,
            plugin_dir.canonicalize().unwrap().join("nexo-plugin.toml")
        );
    }

    #[test]
    fn path_to_manifest_resolves_manifest_directly() {
        let tmp = TempDir::new().unwrap();
        let plugin_dir = tmp.path().join("local_y");
        fs::create_dir_all(&plugin_dir).unwrap();
        let manifest = plugin_dir.join("nexo-plugin.toml");
        write_minimal_manifest(&manifest, "local_y");

        let override_ = resolve_local_plugin(&manifest, false).expect("resolve ok");
        assert_eq!(override_.plugin_root, plugin_dir.canonicalize().unwrap());
        assert_eq!(override_.manifest_path, manifest.canonicalize().unwrap());
    }

    #[test]
    fn path_to_manifest_rejects_non_existent() {
        let tmp = TempDir::new().unwrap();
        let bogus = tmp.path().join("does_not_exist");
        let err = resolve_local_plugin(&bogus, false).unwrap_err();
        assert!(matches!(err, PluginRunError::PathNotFound { .. }));
    }

    #[test]
    fn path_to_manifest_rejects_dir_without_manifest() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_local_plugin(tmp.path(), false).unwrap_err();
        assert!(matches!(err, PluginRunError::NotAPluginPath { .. }));
    }

    #[test]
    fn path_to_manifest_rejects_invalid_toml() {
        let tmp = TempDir::new().unwrap();
        let plugin_dir = tmp.path().join("bad_plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("nexo-plugin.toml"), "[[[unterminated").unwrap();
        let err = resolve_local_plugin(&plugin_dir, false).unwrap_err();
        assert!(matches!(err, PluginRunError::ManifestInvalid { .. }));
    }

    #[test]
    fn path_to_manifest_rejects_missing_entrypoint() {
        let tmp = TempDir::new().unwrap();
        let plugin_dir = tmp.path().join("entrypointless");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("nexo-plugin.toml"),
            r#"
[plugin]
id = "entrypointless"
version = "0.1.0"
name = "Entrypointless"
description = "no entrypoint command"
min_nexo_version = ">=0.1.0"

[plugin.entrypoint]
command = ""
args = []
"#,
        )
        .unwrap();
        let err = resolve_local_plugin(&plugin_dir, false).unwrap_err();
        assert!(matches!(err, PluginRunError::MissingEntrypoint { .. }));
    }

    #[test]
    fn apply_search_path_prepend_inserts_at_head() {
        let mut search_paths =
            vec![PathBuf::from("/etc/nexo/plugins"), PathBuf::from("/usr/local")];
        let plugin_root = PathBuf::from("/tmp/local");
        apply_search_path_prepend(&mut search_paths, &plugin_root);
        assert_eq!(search_paths.len(), 3);
        assert_eq!(search_paths[0], PathBuf::from("/tmp/local"));
        assert_eq!(search_paths[1], PathBuf::from("/etc/nexo/plugins"));
    }

    #[test]
    fn apply_search_path_prepend_is_idempotent_for_head_match() {
        let mut search_paths =
            vec![PathBuf::from("/tmp/local"), PathBuf::from("/etc/nexo/plugins")];
        apply_search_path_prepend(&mut search_paths, &PathBuf::from("/tmp/local"));
        assert_eq!(
            search_paths.len(),
            2,
            "head already matched → no duplicate insert"
        );
    }
}
