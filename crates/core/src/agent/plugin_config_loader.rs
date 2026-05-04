//! Phase 81.4 — Plugin-scoped config dir loader.
//!
//! Reads each plugin's `<config_dir>/plugins/<plugin_id>/*.yaml`
//! files, deep-merges them with `${ENV_VAR}` resolution, validates
//! the merged tree against `manifest.config.schema_path`'s
//! JSONSchema (when present), and surfaces the result as
//! [`PluginConfig`] for the init-loop to thread into
//! `PluginInitContext`.
//!
//! Multi-file sharding lets operators split sensitive settings
//! (credentials) from declarative ones (channel allowlists).
//! Files merge alphabetically:
//!
//! ```text
//! <config_dir>/plugins/slack/
//!   01-credentials.yaml
//!   02-channels.yaml
//!   03-allowlist.yaml
//! ```
//!
//! Empty / missing dir is OK — returns an empty mapping. Plugins
//! whose schema declares all fields optional load with no
//! operator action.
//!
//! Schema validation reuses the lightweight subset validator
//! shipped in `nexo-plugin-manifest::config_schema` (covers
//! `type`, `required`, `properties`, `additionalProperties`,
//! `enum`). Plugins needing `oneOf` / `$ref` / `pattern` get
//! richer validation in 81.4.c.
//!
//! Hot-reload of plugin config files is OUT OF SCOPE — operators
//! restart the daemon today. 81.4.b wires the post-hook.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nexo_plugin_manifest::{validate_config, ConfigSchemaError, PluginManifest};
use serde_yaml::Value;

/// Pre-loaded + pre-validated plugin config. Empty mapping when
/// the operator has not placed any yaml files at the per-plugin
/// dir — the plugin still boots; its `init` decides whether the
/// missing config is acceptable.
#[derive(Debug, Clone)]
pub struct PluginConfig {
    /// Deep-merged YAML value across every `*.yaml` / `*.yml`
    /// file in the plugin's config dir. Always at least
    /// `Value::Mapping(Mapping::new())` (never `Null`).
    pub merged: Value,
    /// `true` when `manifest.config.schema_path` was set AND the
    /// merged value validated cleanly against it.
    pub schema_validated: bool,
    /// Absolute paths of every yaml file consumed, in merge
    /// order. Empty when the dir was missing or contained no
    /// yaml files.
    pub source_files: Vec<PathBuf>,
}

impl PluginConfig {
    fn empty() -> Self {
        Self {
            merged: Value::Mapping(serde_yaml::Mapping::new()),
            schema_validated: false,
            source_files: Vec::new(),
        }
    }
}

/// Errors `load_plugin_config` can return. Boundary type — every
/// failure mode the init-loop needs to discriminate is its own
/// variant.
#[derive(Debug, thiserror::Error)]
pub enum PluginConfigError {
    #[error("read dir {path}: {error}", path = .path.display())]
    DirRead { path: PathBuf, error: String },
    #[error("read file {path}: {error}", path = .path.display())]
    FileRead { path: PathBuf, error: String },
    #[error("env-var resolve in {path}: {error}", path = .path.display())]
    EnvResolve { path: PathBuf, error: String },
    #[error("yaml parse {path}: {error}", path = .path.display())]
    YamlParse { path: PathBuf, error: String },
    #[error("read schema {path}: {error}", path = .path.display())]
    SchemaRead { path: PathBuf, error: String },
    #[error("parse schema {path}: {error}", path = .path.display())]
    SchemaParse { path: PathBuf, error: String },
    #[error(
        "schema validation failed against {path} ({} error(s))", errors.len(),
        path = .schema_path.display()
    )]
    SchemaValidation {
        schema_path: PathBuf,
        errors: Vec<ConfigSchemaError>,
    },
    #[error("symlink at {path} escapes the plugin config dir", path = .path.display())]
    SymlinkEscape { path: PathBuf },
}

/// Stable string discriminator for the JSON shape used by the
/// `plugin.lifecycle.<id>.config_load_failed` broker event +
/// future doctor surface.
pub fn config_error_kind(e: &PluginConfigError) -> &'static str {
    match e {
        PluginConfigError::DirRead { .. } => "DirRead",
        PluginConfigError::FileRead { .. } => "FileRead",
        PluginConfigError::EnvResolve { .. } => "EnvResolve",
        PluginConfigError::YamlParse { .. } => "YamlParse",
        PluginConfigError::SchemaRead { .. } => "SchemaRead",
        PluginConfigError::SchemaParse { .. } => "SchemaParse",
        PluginConfigError::SchemaValidation { .. } => "SchemaValidation",
        PluginConfigError::SymlinkEscape { .. } => "SymlinkEscape",
    }
}

/// `<config_dir>/plugins/<plugin_id>/`. Convenience for callers
/// that need the path before invoking the loader.
pub fn plugin_config_dir_for(config_dir: &Path, plugin_id: &str) -> PathBuf {
    config_dir.join("plugins").join(plugin_id)
}

/// Load + validate a single plugin's config. Reads
/// `<config_dir>/plugins/<plugin_id>/*.yaml` (alphabetical),
/// resolves `${ENV_VAR}` placeholders, deep-merges, and
/// validates against `manifest.config.schema_path` (resolved
/// relative to `plugin_root`) when set.
pub fn load_plugin_config(
    plugin_root: &Path,
    config_dir: &Path,
    manifest: &PluginManifest,
) -> Result<PluginConfig, PluginConfigError> {
    let plugin_id = &manifest.plugin.id;
    let dir = plugin_config_dir_for(config_dir, plugin_id);

    if !dir.exists() {
        return Ok(PluginConfig::empty());
    }

    let canonical_dir = std::fs::canonicalize(&dir).map_err(|e| PluginConfigError::DirRead {
        path: dir.clone(),
        error: e.to_string(),
    })?;

    let entries = std::fs::read_dir(&canonical_dir).map_err(|e| PluginConfigError::DirRead {
        path: canonical_dir.clone(),
        error: e.to_string(),
    })?;

    // Filter to files with .yaml/.yml extensions, then sort
    // alphabetically by file name for deterministic merge order.
    let mut yaml_files: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !ft.is_file() && !ft.is_symlink() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("yaml") | Some("yml")) {
            continue;
        }
        yaml_files.insert(path);
    }

    let mut merged = Value::Mapping(serde_yaml::Mapping::new());
    let mut source_files = Vec::with_capacity(yaml_files.len());

    for path in &yaml_files {
        // Path-traversal guard — canonicalize + starts_with the
        // canonicalized dir. Symlinks pointing INTO the dir are
        // fine; escapes rejected.
        let canonical = std::fs::canonicalize(path).map_err(|e| PluginConfigError::FileRead {
            path: path.clone(),
            error: e.to_string(),
        })?;
        if !canonical.starts_with(&canonical_dir) {
            return Err(PluginConfigError::SymlinkEscape {
                path: path.clone(),
            });
        }

        let raw = std::fs::read_to_string(&canonical).map_err(|e| PluginConfigError::FileRead {
            path: canonical.clone(),
            error: e.to_string(),
        })?;

        let resolved = nexo_config::env::resolve_placeholders(&raw, &canonical.display().to_string())
            .map_err(|e| PluginConfigError::EnvResolve {
                path: canonical.clone(),
                error: e.to_string(),
            })?;

        let value: Value =
            serde_yaml::from_str(&resolved).map_err(|e| PluginConfigError::YamlParse {
                path: canonical.clone(),
                error: e.to_string(),
            })?;

        // `serde_yaml::from_str("")` returns `Value::Null`; treat
        // empty file as empty mapping for merge purposes.
        let value = if matches!(value, Value::Null) {
            Value::Mapping(serde_yaml::Mapping::new())
        } else {
            value
        };

        merge_yaml(&mut merged, value);
        source_files.push(canonical);
    }

    let schema_validated = match manifest.plugin.config.schema_path.as_deref() {
        None => false,
        Some(rel) => {
            let schema_path = plugin_root.join(rel);
            let schema_bytes = std::fs::read(&schema_path).map_err(|e| {
                PluginConfigError::SchemaRead {
                    path: schema_path.clone(),
                    error: e.to_string(),
                }
            })?;
            let schema_json: serde_json::Value = serde_json::from_slice(&schema_bytes)
                .map_err(|e| PluginConfigError::SchemaParse {
                    path: schema_path.clone(),
                    error: e.to_string(),
                })?;
            // Round-trip via JSON string to convert
            // `serde_yaml::Value` into `serde_json::Value`. Direct
            // `serde_json::to_value(yaml_val)` works for the
            // subset we need (mappings + scalars + sequences).
            let merged_json: serde_json::Value =
                yaml_to_json(&merged).map_err(|e| PluginConfigError::YamlParse {
                    path: schema_path.clone(),
                    error: format!("merged yaml -> json: {e}"),
                })?;
            let errors = validate_config(&merged_json, &schema_json);
            if !errors.is_empty() {
                return Err(PluginConfigError::SchemaValidation {
                    schema_path,
                    errors,
                });
            }
            true
        }
    };

    Ok(PluginConfig {
        merged,
        schema_validated,
        source_files,
    })
}

/// Deep-merge `from` into `into`. Mappings merge key-by-key
/// recursively; arrays full-replace (NOT concat); scalars / null
/// from `from` overwrite `into`.
fn merge_yaml(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Mapping(into_map), Value::Mapping(from_map)) => {
            for (k, v) in from_map {
                if let Some(existing) = into_map.get_mut(&k) {
                    merge_yaml(existing, v);
                } else {
                    into_map.insert(k, v);
                }
            }
        }
        (slot, from) => {
            *slot = from;
        }
    }
}

/// Convert `serde_yaml::Value` to `serde_json::Value` via
/// JSON-string round-trip. Lossy on YAML-specific features
/// (tags, aliases) but fine for the subset our schema validator
/// reads.
fn yaml_to_json(value: &Value) -> Result<serde_json::Value, String> {
    let s = serde_yaml::to_string(value).map_err(|e| e.to_string())?;
    let json: serde_json::Value =
        serde_yaml::from_str(&s).map_err(|e| format!("yaml->json reparse: {e}"))?;
    Ok(json)
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_plugin_manifest::PluginManifest;
    use std::fs;
    use tempfile::tempdir;

    fn manifest(plugin_id: &str, schema_path: Option<&str>) -> PluginManifest {
        let mut raw = format!(
            "[plugin]\n\
             id = \"{plugin_id}\"\n\
             version = \"0.1.0\"\n\
             name = \"{plugin_id}\"\n\
             description = \"fixture\"\n\
             min_nexo_version = \">=0.0.1\"\n",
        );
        if let Some(p) = schema_path {
            raw.push_str(&format!("\n[plugin.config]\nschema_path = \"{p}\"\n"));
        }
        toml::from_str(&raw).unwrap()
    }

    fn write_file(dir: &Path, name: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn load_returns_empty_when_dir_missing() {
        let cfg_dir = tempdir().unwrap();
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        assert!(matches!(pc.merged, Value::Mapping(ref m) if m.is_empty()));
        assert!(!pc.schema_validated);
        assert!(pc.source_files.is_empty());
    }

    #[test]
    fn load_returns_empty_when_dir_empty() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        fs::create_dir_all(&plugin_dir).unwrap();
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        assert!(matches!(pc.merged, Value::Mapping(ref m) if m.is_empty()));
        assert!(pc.source_files.is_empty());
    }

    #[test]
    fn load_skips_non_yaml_files() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        write_file(&plugin_dir, "notes.txt", "ignore me");
        write_file(&plugin_dir, "01-cred.yaml", "api_token: abc\n");
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        assert_eq!(pc.source_files.len(), 1);
        assert!(pc.source_files[0].ends_with("01-cred.yaml"));
        assert_eq!(
            pc.merged.get("api_token").and_then(Value::as_str),
            Some("abc")
        );
    }

    #[test]
    fn load_merges_files_alphabetically_with_deep_merge() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        write_file(&plugin_dir, "01-cred.yaml", "auth:\n  token: t1\n");
        write_file(&plugin_dir, "02-extra.yaml", "auth:\n  workspace: w1\n");
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        assert_eq!(pc.source_files.len(), 2);
        let auth = pc.merged.get("auth").unwrap();
        assert_eq!(auth.get("token").and_then(Value::as_str), Some("t1"));
        assert_eq!(auth.get("workspace").and_then(Value::as_str), Some("w1"));
    }

    #[test]
    fn load_later_files_overwrite_scalar_keys() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        write_file(&plugin_dir, "01-base.yaml", "endpoint: prod\n");
        write_file(&plugin_dir, "02-override.yaml", "endpoint: staging\n");
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        assert_eq!(
            pc.merged.get("endpoint").and_then(Value::as_str),
            Some("staging")
        );
    }

    #[test]
    fn load_arrays_are_full_replace_not_concat() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        write_file(&plugin_dir, "01-base.yaml", "channels:\n  - a\n  - b\n");
        write_file(&plugin_dir, "02-override.yaml", "channels:\n  - x\n");
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        let channels = pc.merged.get("channels").and_then(Value::as_sequence).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].as_str(), Some("x"));
    }

    #[test]
    fn load_resolves_env_var_substitutions() {
        // Use a unique env var to avoid pollution.
        std::env::set_var("NEXO_TEST_PCL_TOKEN_X", "secret-value");
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        write_file(
            &plugin_dir,
            "01-cred.yaml",
            "api_token: \"${NEXO_TEST_PCL_TOKEN_X}\"\n",
        );
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        assert_eq!(
            pc.merged.get("api_token").and_then(Value::as_str),
            Some("secret-value")
        );
        std::env::remove_var("NEXO_TEST_PCL_TOKEN_X");
    }

    #[test]
    fn load_returns_yaml_parse_error_on_malformed_file() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        write_file(&plugin_dir, "01-bad.yaml", "key: [unclosed\n");
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let err = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap_err();
        assert!(matches!(err, PluginConfigError::YamlParse { .. }));
    }

    #[test]
    fn load_validates_against_schema_when_present() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        write_file(
            &plugin_dir,
            "01-cred.yaml",
            "api_token: abc\nworkspace: ws\n",
        );
        let plugin_root = tempdir().unwrap();
        fs::write(
            plugin_root.path().join("config.schema.json"),
            r#"{
                "type": "object",
                "required": ["api_token"],
                "properties": {
                    "api_token": { "type": "string" },
                    "workspace": { "type": "string" }
                }
            }"#,
        )
        .unwrap();
        let m = manifest("slack", Some("config.schema.json"));
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        assert!(pc.schema_validated);
    }

    #[test]
    fn load_returns_schema_validation_errors_with_pointers() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        // Missing required `api_token`.
        write_file(&plugin_dir, "01-cred.yaml", "workspace: ws\n");
        let plugin_root = tempdir().unwrap();
        fs::write(
            plugin_root.path().join("config.schema.json"),
            r#"{
                "type": "object",
                "required": ["api_token"],
                "properties": {
                    "api_token": { "type": "string" }
                }
            }"#,
        )
        .unwrap();
        let m = manifest("slack", Some("config.schema.json"));
        let err = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap_err();
        match err {
            PluginConfigError::SchemaValidation { errors, .. } => {
                assert!(!errors.is_empty());
                // Subset validator surfaces required-field failures
                // with a JSON pointer.
                assert!(errors
                    .iter()
                    .any(|e| e.message.contains("required") || e.message.contains("api_token")));
            }
            other => panic!("expected SchemaValidation, got {other:?}"),
        }
    }

    #[test]
    fn load_skips_validation_when_schema_path_unset() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        write_file(&plugin_dir, "01-cred.yaml", "anything_goes: 42\n");
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let pc = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap();
        assert!(!pc.schema_validated);
    }

    #[test]
    #[cfg(unix)]
    fn load_rejects_symlink_escape() {
        let cfg_dir = tempdir().unwrap();
        let plugin_dir = cfg_dir.path().join("plugins").join("slack");
        fs::create_dir_all(&plugin_dir).unwrap();
        // Create an outside-target file the symlink will point at.
        let outside = cfg_dir.path().join("outside.yaml");
        fs::write(&outside, "evil: true\n").unwrap();
        std::os::unix::fs::symlink(&outside, plugin_dir.join("01-evil.yaml")).unwrap();
        let plugin_root = tempdir().unwrap();
        let m = manifest("slack", None);
        let err = load_plugin_config(plugin_root.path(), cfg_dir.path(), &m).unwrap_err();
        assert!(matches!(err, PluginConfigError::SymlinkEscape { .. }));
    }

    #[test]
    fn config_error_kind_maps_all_variants() {
        let path = PathBuf::from("/x");
        let cases: Vec<(PluginConfigError, &'static str)> = vec![
            (
                PluginConfigError::DirRead {
                    path: path.clone(),
                    error: "x".into(),
                },
                "DirRead",
            ),
            (
                PluginConfigError::FileRead {
                    path: path.clone(),
                    error: "x".into(),
                },
                "FileRead",
            ),
            (
                PluginConfigError::EnvResolve {
                    path: path.clone(),
                    error: "x".into(),
                },
                "EnvResolve",
            ),
            (
                PluginConfigError::YamlParse {
                    path: path.clone(),
                    error: "x".into(),
                },
                "YamlParse",
            ),
            (
                PluginConfigError::SchemaRead {
                    path: path.clone(),
                    error: "x".into(),
                },
                "SchemaRead",
            ),
            (
                PluginConfigError::SchemaParse {
                    path: path.clone(),
                    error: "x".into(),
                },
                "SchemaParse",
            ),
            (
                PluginConfigError::SchemaValidation {
                    schema_path: path.clone(),
                    errors: vec![],
                },
                "SchemaValidation",
            ),
            (
                PluginConfigError::SymlinkEscape { path: path.clone() },
                "SymlinkEscape",
            ),
        ];
        for (e, want) in cases {
            assert_eq!(config_error_kind(&e), want);
        }
    }
}
