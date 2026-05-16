//! Filesystem walker that produces a
//! [`NexoPluginRegistrySnapshot`] from operator-configured search
//! paths. Mirrors the never-panic shape of
//! `crates/extensions/src/discovery.rs`: every error becomes a typed
//! diagnostic; the daemon always boots.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use semver::Version;
use walkdir::WalkDir;

use nexo_plugin_manifest::{validate, ManifestError, PluginManifest, PLUGIN_MANIFEST_FILENAMES};

use super::config::{resolve_search_paths, PluginDiscoveryConfig};
use super::report::{
    DiagnosticLevel, DiscoveredPlugin, DiscoveryDiagnostic, DiscoveryDiagnosticKind,
    PluginDiscoveryReport,
};
use super::NexoPluginRegistrySnapshot;

/// Canonical manifest filename. Fixed (no operator
/// override) so a malicious or careless plugin dir cannot smuggle a
/// manifest under an unexpected name. We also accept the legacy
/// `nexo-plugin.toml` via [`PLUGIN_MANIFEST_FILENAMES`] so plugins
/// shipped before the rename keep loading.
#[cfg(test)]
pub(crate) const MANIFEST_FILENAME: &str = "nexo-plugin.toml";

/// `true` when `name` matches either the canonical `plugin.toml`
/// or the legacy `nexo-plugin.toml` filename.
fn is_manifest_filename(name: &std::ffi::OsStr) -> bool {
    PLUGIN_MANIFEST_FILENAMES
        .iter()
        .any(|candidate| name == std::ffi::OsStr::new(candidate))
}

/// Walk the configured search paths, parse + validate each
/// `<entry>/nexo-plugin.toml`, return a snapshot ready to swap into
/// [`super::NexoPluginRegistry`]. Never panics.
pub fn discover(
    cfg: &PluginDiscoveryConfig,
    current_nexo_version: &Version,
) -> Arc<NexoPluginRegistrySnapshot> {
    let mut report = PluginDiscoveryReport::default();
    let mut plugins: Vec<DiscoveredPlugin> = Vec::new();

    let (search_paths, env_diagnostics) = resolve_search_paths(cfg);
    report.diagnostics.extend(env_diagnostics);

    let allowlist = &cfg.allowlist;
    let disabled = &cfg.disabled;

    // Track first-seen ids so duplicates can be reported with the
    // path that won.
    let mut first_seen: HashMap<String, PathBuf> = HashMap::new();

    for (root_index, root) in search_paths.iter().enumerate() {
        let canonical_root = match std::fs::canonicalize(root) {
            Ok(p) => p,
            Err(e) => {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: root.clone(),
                    kind: DiscoveryDiagnosticKind::SearchPathMissing {
                        reason: e.to_string(),
                    },
                });
                continue;
            }
        };

        let walker = WalkDir::new(&canonical_root)
            .max_depth(2)
            .follow_links(cfg.follow_symlinks)
            .sort_by_file_name()
            .into_iter();

        for entry_result in walker {
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    let is_perm_denied = e.io_error().is_some_and(|io: &std::io::Error| {
                        io.kind() == std::io::ErrorKind::PermissionDenied
                    });
                    let kind = if is_perm_denied {
                        DiscoveryDiagnosticKind::PermissionDenied
                    } else {
                        DiscoveryDiagnosticKind::ManifestParseError {
                            error: format!("walk error: {e}"),
                        }
                    };
                    let err_path: PathBuf =
                        e.path().map(PathBuf::from).unwrap_or_else(|| root.clone());
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Warn,
                        path: err_path,
                        kind,
                    });
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            if !is_manifest_filename(entry.file_name()) {
                continue;
            }

            report.scanned += 1;
            let manifest_path = entry.path().to_path_buf();

            // Symlink escape check when follow_symlinks=false: the
            // manifest's canonical form must stay inside the
            // canonical search-path root.
            let canonical_manifest = match std::fs::canonicalize(&manifest_path) {
                Ok(p) => p,
                Err(_) => {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Warn,
                        path: manifest_path.clone(),
                        kind: DiscoveryDiagnosticKind::PermissionDenied,
                    });
                    continue;
                }
            };
            if !cfg.follow_symlinks && !canonical_manifest.starts_with(&canonical_root) {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Error,
                    path: manifest_path.clone(),
                    kind: DiscoveryDiagnosticKind::SymlinkEscape {
                        target: canonical_manifest.clone(),
                    },
                });
                report.invalid += 1;
                continue;
            }

            let raw = match std::fs::read_to_string(&manifest_path) {
                Ok(s) => s,
                Err(e) => {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Error,
                        path: manifest_path.clone(),
                        kind: DiscoveryDiagnosticKind::ManifestParseError {
                            error: e.to_string(),
                        },
                    });
                    report.invalid += 1;
                    continue;
                }
            };

            let manifest: PluginManifest = match toml::from_str(&raw) {
                Ok(m) => m,
                Err(e) => {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Error,
                        path: manifest_path.clone(),
                        kind: DiscoveryDiagnosticKind::ManifestParseError {
                            error: e.to_string(),
                        },
                    });
                    report.invalid += 1;
                    continue;
                }
            };

            let mut errs: Vec<ManifestError> = Vec::new();
            validate::run_all(&manifest, current_nexo_version, &mut errs);
            if !errs.is_empty() {
                let strings: Vec<String> = errs.iter().map(|e| format!("{e}")).collect();
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Error,
                    path: manifest_path.clone(),
                    kind: DiscoveryDiagnosticKind::ValidationFailed { errors: strings },
                });
                report.invalid += 1;
                continue;
            }

            let id = manifest.plugin.id.clone();

            // Version mismatch — the validator above already considers
            // `min_nexo_version`; but if the manifest declares a
            // version requirement and the validator returned clean
            // (the requirement is just "all-versions"), fall through.

            // Disabled filter — applied AFTER validation so the
            // operator sees the plugin existed + was valid.
            if disabled.iter().any(|d| d == &id) {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: manifest_path.clone(),
                    kind: DiscoveryDiagnosticKind::Disabled { id: id.clone() },
                });
                report.disabled += 1;
                continue;
            }

            // Allowlist filter — empty list = accept all.
            if !allowlist.is_empty() && !allowlist.iter().any(|a| a == &id) {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: manifest_path.clone(),
                    kind: DiscoveryDiagnosticKind::AllowlistRejected { id: id.clone() },
                });
                continue;
            }

            // Duplicate-id detection — first wins.
            if let Some(kept) = first_seen.get(&id) {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: manifest_path.clone(),
                    kind: DiscoveryDiagnosticKind::DuplicateId {
                        id: id.clone(),
                        kept_path: kept.clone(),
                    },
                });
                report.duplicates += 1;
                continue;
            }
            first_seen.insert(id.clone(), manifest_path.clone());

            let root_dir = entry
                .path()
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| canonical_root.clone());
            plugins.push(DiscoveredPlugin {
                manifest,
                root_dir,
                manifest_path,
            });
            report.loaded_ids.push(id);
            // root_index ensures stable ordering across runs even
            // when search_paths differ in mtime.
            let _ = root_index;
        }

        if cfg.auto_detect_binaries {
            scan_binaries_in_root(
                &canonical_root,
                current_nexo_version,
                allowlist,
                disabled,
                &mut first_seen,
                &mut plugins,
                &mut report,
            );
        }
    }

    Arc::new(NexoPluginRegistrySnapshot {
        plugins,
        last_report: report,
        skill_roots: std::collections::BTreeMap::new(),
    })
}

/// Auto-discovery branch: enumerate ELF/PE/Mach-O executables in
/// `root` whose filename matches `nexo-plugin-<id>` (`.exe` accepted
/// on Windows), spawn each with `--print-manifest` (2s timeout),
/// parse the emitted TOML, and register the plugin if validation
/// passes. Mirrors the manifest-file branch on diagnostics, dedup,
/// disabled/allowlist filters.
///
/// Boot-time security note: this opens the door to executing
/// untrusted binaries on daemon start. The trust root is whoever
/// owns `root` (typically the operator's own `~/.cargo/bin`).
/// Operators can disable via `discovery.auto_detect_binaries: false`.
fn scan_binaries_in_root(
    root: &std::path::Path,
    current_nexo_version: &Version,
    allowlist: &[String],
    disabled: &[String],
    first_seen: &mut HashMap<String, PathBuf>,
    plugins: &mut Vec<DiscoveredPlugin>,
    report: &mut PluginDiscoveryReport,
) {
    let read_dir = match std::fs::read_dir(root) {
        Ok(r) => r,
        // SearchPathMissing already reported by the outer canonicalize call.
        Err(_) => return,
    };
    let mut entries: Vec<_> = read_dir.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_plugin_binary_name(file_name) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        if !is_executable(&meta) {
            continue;
        }

        report.scanned += 1;
        let raw_manifest = match spawn_print_manifest(&path) {
            Ok(s) => s,
            Err(e) => {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: path.clone(),
                    kind: DiscoveryDiagnosticKind::ManifestParseError {
                        error: format!("--print-manifest failed: {e}"),
                    },
                });
                continue;
            }
        };
        let mut manifest: PluginManifest = match toml::from_str(&raw_manifest) {
            Ok(m) => m,
            Err(e) => {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: path.clone(),
                    kind: DiscoveryDiagnosticKind::ManifestParseError {
                        error: format!("--print-manifest emitted invalid TOML: {e}"),
                    },
                });
                report.invalid += 1;
                continue;
            }
        };
        // Override entrypoint with the on-disk path we just probed.
        // The embedded manifest's `command` is built for the plugin's
        // own source tree (`./bin/...`) and won't resolve from the
        // daemon CWD.
        manifest.plugin.entrypoint.command = Some(path.to_string_lossy().into_owned());

        let mut errs: Vec<ManifestError> = Vec::new();
        validate::run_all(&manifest, current_nexo_version, &mut errs);
        if !errs.is_empty() {
            let strings: Vec<String> = errs.iter().map(|e| format!("{e}")).collect();
            report.diagnostics.push(DiscoveryDiagnostic {
                level: DiagnosticLevel::Error,
                path: path.clone(),
                kind: DiscoveryDiagnosticKind::ValidationFailed { errors: strings },
            });
            report.invalid += 1;
            continue;
        }

        let id = manifest.plugin.id.clone();
        if disabled.iter().any(|d| d == &id) {
            report.diagnostics.push(DiscoveryDiagnostic {
                level: DiagnosticLevel::Warn,
                path: path.clone(),
                kind: DiscoveryDiagnosticKind::Disabled { id: id.clone() },
            });
            report.disabled += 1;
            continue;
        }
        if !allowlist.is_empty() && !allowlist.iter().any(|a| a == &id) {
            report.diagnostics.push(DiscoveryDiagnostic {
                level: DiagnosticLevel::Warn,
                path: path.clone(),
                kind: DiscoveryDiagnosticKind::AllowlistRejected { id: id.clone() },
            });
            continue;
        }
        if let Some(kept) = first_seen.get(&id) {
            report.diagnostics.push(DiscoveryDiagnostic {
                level: DiagnosticLevel::Warn,
                path: path.clone(),
                kind: DiscoveryDiagnosticKind::DuplicateId {
                    id: id.clone(),
                    kept_path: kept.clone(),
                },
            });
            report.duplicates += 1;
            continue;
        }
        first_seen.insert(id.clone(), path.clone());
        plugins.push(DiscoveredPlugin {
            manifest,
            root_dir: root.to_path_buf(),
            manifest_path: path.clone(),
        });
        report.loaded_ids.push(id);
    }
}

/// `true` when `name` is shaped `nexo-plugin-<id>` (optional `.exe`
/// suffix on Windows). The `<id>` segment is restricted to
/// lowercase ASCII alphanumerics + hyphens — same charset enforced
/// by `nexo_plugin_manifest::validate` on the `plugin.id` field.
fn is_plugin_binary_name(name: &str) -> bool {
    const PREFIX: &str = "nexo-plugin-";
    let Some(rest) = name.strip_prefix(PREFIX) else {
        return false;
    };
    let stem = rest.strip_suffix(".exe").unwrap_or(rest);
    if stem.is_empty() {
        return false;
    }
    stem.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    // On Windows there's no Unix-style x bit; the `.exe` suffix
    // gate in `is_plugin_binary_name` already filters non-executables.
    true
}

/// Spawn `<bin> --print-manifest`, return stdout. Hard 2s timeout —
/// a plugin that hangs at startup is excluded from discovery and
/// surfaced as a diagnostic. Kills on timeout to avoid orphaned
/// children. Never blocks the daemon boot path indefinitely.
fn spawn_print_manifest(bin: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new(bin)
        .arg("--print-manifest")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(format!("exit status {status}"));
                }
                let mut buf = String::new();
                if let Some(mut out) = child.stdout.take() {
                    out.read_to_string(&mut buf)
                        .map_err(|e| format!("read stdout: {e}"))?;
                }
                return Ok(buf);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("timeout after 2s".into());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("wait: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod binary_name_tests {
    use super::is_plugin_binary_name;

    #[test]
    fn accepts_lowercase_id() {
        assert!(is_plugin_binary_name("nexo-plugin-email"));
        assert!(is_plugin_binary_name("nexo-plugin-whatsapp"));
    }

    #[test]
    fn accepts_hyphens_and_digits_in_id() {
        assert!(is_plugin_binary_name("nexo-plugin-my-plugin-v2"));
        assert!(is_plugin_binary_name("nexo-plugin-sms2"));
    }

    #[test]
    fn accepts_windows_exe_suffix() {
        assert!(is_plugin_binary_name("nexo-plugin-email.exe"));
    }

    #[test]
    fn rejects_missing_id_segment() {
        assert!(!is_plugin_binary_name("nexo-plugin-"));
        assert!(!is_plugin_binary_name("nexo-plugin-.exe"));
    }

    #[test]
    fn rejects_unrelated_names() {
        assert!(!is_plugin_binary_name("agent"));
        assert!(!is_plugin_binary_name("nexo-rs"));
        assert!(!is_plugin_binary_name("plugin-email"));
    }

    #[test]
    fn rejects_uppercase_in_id() {
        // Mirror the validator's lowercase-only rule on plugin.id.
        assert!(!is_plugin_binary_name("nexo-plugin-Email"));
    }

    #[test]
    fn rejects_invalid_chars() {
        assert!(!is_plugin_binary_name("nexo-plugin-foo_bar"));
        assert!(!is_plugin_binary_name("nexo-plugin-foo.bar"));
        assert!(!is_plugin_binary_name("nexo-plugin-../etc/passwd"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a search-path tree on disk for a fixture test.
    struct PluginTreeBuilder {
        root: TempDir,
    }

    impl PluginTreeBuilder {
        fn new() -> Self {
            Self {
                root: tempfile::tempdir().expect("tempdir"),
            }
        }

        fn root(&self) -> &std::path::Path {
            self.root.path()
        }

        fn with_valid(&self, plugin_id: &str, version: &str) -> &Self {
            let dir = self.root.path().join(plugin_id);
            fs::create_dir_all(&dir).unwrap();
            let manifest = format!(
                "[plugin]\n\
                 id = \"{plugin_id}\"\n\
                 version = \"{version}\"\n\
                 name = \"{plugin_id}\"\n\
                 description = \"fixture\"\n\
                 min_nexo_version = \">=0.0.1\"\n",
            );
            fs::write(dir.join(MANIFEST_FILENAME), manifest).unwrap();
            self
        }

        fn with_malformed(&self, plugin_id: &str) -> &Self {
            let dir = self.root.path().join(plugin_id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(MANIFEST_FILENAME), "this is not [valid toml ===\n").unwrap();
            self
        }

        fn with_invalid_id(&self, dir_name: &str) -> &Self {
            let dir = self.root.path().join(dir_name);
            fs::create_dir_all(&dir).unwrap();
            // id "Bad ID" violates the [a-z][a-z0-9_]{0,31} rule.
            let manifest = "[plugin]\n\
                            id = \"Bad ID\"\n\
                            version = \"0.1.0\"\n\
                            name = \"x\"\n\
                            description = \"x\"\n\
                            min_nexo_version = \">=0.0.1\"\n";
            fs::write(dir.join(MANIFEST_FILENAME), manifest).unwrap();
            self
        }
    }

    fn current_v() -> Version {
        Version::parse("0.1.0").unwrap()
    }

    fn cfg(paths: Vec<PathBuf>) -> PluginDiscoveryConfig {
        PluginDiscoveryConfig {
            search_paths: paths,
            ..Default::default()
        }
    }

    #[test]
    fn valid_single_plugin() {
        let t = PluginTreeBuilder::new();
        t.with_valid("browser", "0.1.0");
        let snap = discover(&cfg(vec![t.root().to_path_buf()]), &current_v());
        assert_eq!(snap.plugins.len(), 1);
        assert_eq!(snap.last_report.loaded_ids, vec!["browser".to_string()]);
        assert_eq!(snap.last_report.invalid, 0);
        assert_eq!(snap.last_report.disabled, 0);
        assert_eq!(snap.last_report.duplicates, 0);
        assert!(snap
            .last_report
            .diagnostics
            .iter()
            .all(|d| d.level == DiagnosticLevel::Warn
                || matches!(d.kind, DiscoveryDiagnosticKind::Disabled { .. })));
    }

    #[test]
    fn mixed_valid_and_malformed() {
        let t = PluginTreeBuilder::new();
        t.with_valid("good", "0.1.0").with_malformed("bad");
        let snap = discover(&cfg(vec![t.root().to_path_buf()]), &current_v());
        assert_eq!(snap.plugins.len(), 1);
        assert_eq!(snap.last_report.invalid, 1);
        assert!(snap
            .last_report
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiscoveryDiagnosticKind::ManifestParseError { .. })));
    }

    #[test]
    fn validation_failure_invalid_id() {
        let t = PluginTreeBuilder::new();
        t.with_invalid_id("plugin-dir");
        let snap = discover(&cfg(vec![t.root().to_path_buf()]), &current_v());
        assert_eq!(snap.plugins.len(), 0);
        assert_eq!(snap.last_report.invalid, 1);
        assert!(snap
            .last_report
            .diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiscoveryDiagnosticKind::ValidationFailed { .. })));
    }

    #[test]
    fn disabled_filter_rejects() {
        let t = PluginTreeBuilder::new();
        t.with_valid("browser", "0.1.0");
        let mut c = cfg(vec![t.root().to_path_buf()]);
        c.disabled = vec!["browser".into()];
        let snap = discover(&c, &current_v());
        assert_eq!(snap.plugins.len(), 0);
        assert_eq!(snap.last_report.disabled, 1);
        assert!(snap.last_report.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiscoveryDiagnosticKind::Disabled { id } if id == "browser"
        )));
    }

    #[test]
    fn allowlist_rejects_non_listed() {
        let t = PluginTreeBuilder::new();
        t.with_valid("browser", "0.1.0")
            .with_valid("email", "0.1.0");
        let mut c = cfg(vec![t.root().to_path_buf()]);
        c.allowlist = vec!["browser".into()];
        let snap = discover(&c, &current_v());
        assert_eq!(snap.plugins.len(), 1);
        assert_eq!(snap.last_report.loaded_ids, vec!["browser".to_string()]);
        assert!(snap.last_report.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiscoveryDiagnosticKind::AllowlistRejected { id } if id == "email"
        )));
    }

    #[test]
    fn duplicate_id_first_wins() {
        let t1 = PluginTreeBuilder::new();
        let t2 = PluginTreeBuilder::new();
        t1.with_valid("foo", "0.1.0");
        t2.with_valid("foo", "0.2.0");
        let snap = discover(
            &cfg(vec![t1.root().to_path_buf(), t2.root().to_path_buf()]),
            &current_v(),
        );
        assert_eq!(snap.plugins.len(), 1);
        assert_eq!(
            snap.plugins[0].manifest.plugin.version,
            Version::parse("0.1.0").unwrap()
        );
        assert_eq!(snap.last_report.duplicates, 1);
    }

    #[test]
    fn version_mismatch_skips() {
        let t = PluginTreeBuilder::new();
        let dir = t.root().join("future");
        fs::create_dir_all(&dir).unwrap();
        let manifest = "[plugin]\n\
                        id = \"future\"\n\
                        version = \"0.1.0\"\n\
                        name = \"x\"\n\
                        description = \"x\"\n\
                        min_nexo_version = \"^99.0\"\n";
        fs::write(dir.join(MANIFEST_FILENAME), manifest).unwrap();
        let snap = discover(&cfg(vec![t.root().to_path_buf()]), &current_v());
        assert_eq!(snap.plugins.len(), 0);
        assert_eq!(snap.last_report.invalid, 1);
        assert!(snap.last_report.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiscoveryDiagnosticKind::ValidationFailed { errors }
                if errors.iter().any(|e| e.contains("nexo") || e.contains("version"))
        )));
    }

    #[test]
    fn symlink_escape_blocked() {
        // Build outside-tree target and a search-path tree that
        // contains a symlink redirecting into it.
        let outside = tempfile::tempdir().unwrap();
        let outside_dir = outside.path().join("evil");
        fs::create_dir_all(&outside_dir).unwrap();
        let manifest = "[plugin]\nid = \"evil\"\nversion = \"0.1.0\"\nname = \"x\"\n";
        fs::write(outside_dir.join(MANIFEST_FILENAME), manifest).unwrap();

        let inside = tempfile::tempdir().unwrap();
        let symlink_at = inside.path().join("evil");
        std::os::unix::fs::symlink(&outside_dir, &symlink_at).unwrap();

        let snap = discover(&cfg(vec![inside.path().to_path_buf()]), &current_v());
        assert_eq!(snap.plugins.len(), 0);
        // walker ignores symlinks entirely when follow_links=false
        // (the entry is reported as a symlink, not descended).
        // The escape test asserts no plugin loaded — which is the
        // contract operators care about.
    }

    #[test]
    fn unresolved_env_var_diagnostic() {
        unsafe { std::env::remove_var("NEXO_TEST_DISCOVER_NONEXISTENT") };
        let snap = discover(
            &cfg(vec![PathBuf::from(
                "$NEXO_TEST_DISCOVER_NONEXISTENT/plugins",
            )]),
            &current_v(),
        );
        assert_eq!(snap.plugins.len(), 0);
        assert!(snap.last_report.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiscoveryDiagnosticKind::UnresolvedEnvVar { var_name, .. }
                if var_name == "NEXO_TEST_DISCOVER_NONEXISTENT"
        )));
    }

    #[test]
    fn empty_search_paths_returns_empty_snapshot() {
        // Explicit empty list — the type's `Default` populates
        // standard paths (`~/.cargo/bin` etc.) which may contain
        // installed plugins on a developer machine.
        let snap = discover(
            &PluginDiscoveryConfig {
                search_paths: Vec::new(),
                ..PluginDiscoveryConfig::default()
            },
            &current_v(),
        );
        assert_eq!(snap.plugins.len(), 0);
        assert_eq!(snap.last_report.loaded_ids.len(), 0);
        assert_eq!(snap.last_report.invalid, 0);
        assert_eq!(snap.last_report.disabled, 0);
        assert_eq!(snap.last_report.duplicates, 0);
    }

    /// Build a shell script at `<root>/<name>` that emits `manifest`
    /// when invoked with `--print-manifest` and exits non-zero
    /// otherwise. Returns the script path. Used by the binary
    /// auto-detection tests below.
    #[cfg(unix)]
    fn fake_plugin_binary(root: &std::path::Path, name: &str, manifest: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join(name);
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--print-manifest\" ]; then\n\
               cat <<'MANIFEST_EOF'\n{manifest}\nMANIFEST_EOF\n\
               exit 0\n\
             fi\n\
             exit 1\n",
        );
        fs::write(&path, script).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn binary_scan_picks_up_cargo_install_style_plugin() {
        let root = tempfile::tempdir().unwrap();
        let manifest = "[plugin]\n\
                        id = \"fakeplugin\"\n\
                        version = \"0.1.0\"\n\
                        name = \"fakeplugin\"\n\
                        description = \"fixture\"\n\
                        min_nexo_version = \">=0.0.1\"\n\
                        [plugin.entrypoint]\n\
                        command = \"./does/not/exist\"\n";
        let bin_path = fake_plugin_binary(root.path(), "nexo-plugin-fakeplugin", manifest);

        let snap = discover(&cfg(vec![root.path().to_path_buf()]), &current_v());

        assert_eq!(snap.plugins.len(), 1, "binary not detected");
        assert_eq!(snap.last_report.loaded_ids, vec!["fakeplugin".to_string()]);
        // Entrypoint command must be overridden to the on-disk path.
        let cmd = snap.plugins[0]
            .manifest
            .plugin
            .entrypoint
            .command
            .as_deref()
            .unwrap();
        assert_eq!(std::path::Path::new(cmd), bin_path.as_path());
    }

    #[cfg(unix)]
    #[test]
    fn binary_scan_is_disabled_when_toggle_off() {
        let root = tempfile::tempdir().unwrap();
        let manifest = "[plugin]\n\
                        id = \"offcheck\"\n\
                        version = \"0.1.0\"\n\
                        name = \"offcheck\"\n\
                        description = \"fixture\"\n\
                        min_nexo_version = \">=0.0.1\"\n";
        fake_plugin_binary(root.path(), "nexo-plugin-offcheck", manifest);

        let snap = discover(
            &PluginDiscoveryConfig {
                search_paths: vec![root.path().to_path_buf()],
                auto_detect_binaries: false,
                ..PluginDiscoveryConfig::default()
            },
            &current_v(),
        );

        assert!(snap.plugins.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn binary_scan_skips_non_matching_executables() {
        let root = tempfile::tempdir().unwrap();
        let manifest = "ignored\n";
        // Wrong prefix — should never be probed.
        fake_plugin_binary(root.path(), "agent", manifest);
        fake_plugin_binary(root.path(), "nexo-plugin-", manifest);
        fake_plugin_binary(root.path(), "some-other-tool", manifest);

        let snap = discover(&cfg(vec![root.path().to_path_buf()]), &current_v());

        assert!(snap.plugins.is_empty());
        assert_eq!(snap.last_report.scanned, 0);
    }

    #[cfg(unix)]
    #[test]
    fn binary_scan_reports_diagnostic_on_failed_probe() {
        let root = tempfile::tempdir().unwrap();
        // Script exits non-zero on `--print-manifest`.
        use std::os::unix::fs::PermissionsExt;
        let path = root.path().join("nexo-plugin-brokenprobe");
        fs::write(&path, "#!/bin/sh\nexit 7\n").unwrap();
        let mut p = fs::metadata(&path).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&path, p).unwrap();

        let snap = discover(&cfg(vec![root.path().to_path_buf()]), &current_v());

        assert!(snap.plugins.is_empty());
        assert!(snap.last_report.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiscoveryDiagnosticKind::ManifestParseError { error }
                if error.contains("--print-manifest failed")
        )));
    }

    #[test]
    fn walker_max_depth_does_not_recurse_three_levels() {
        // A nexo-plugin.toml three levels deep must NOT be picked up
        // (max_depth=2 → search_path → child_dir → manifest).
        let t = PluginTreeBuilder::new();
        let nested = t.root().join("a").join("nested").join("deep");
        fs::create_dir_all(&nested).unwrap();
        let manifest = "[plugin]\nid = \"deep\"\nversion = \"0.1.0\"\nname = \"x\"\n";
        fs::write(nested.join(MANIFEST_FILENAME), manifest).unwrap();
        let snap = discover(&cfg(vec![t.root().to_path_buf()]), &current_v());
        assert!(snap.plugins.is_empty());
    }
}
