//! Phase 98 follow-up — toggle a plugin's enabled state by editing
//! `<config>/plugins/discovery.yaml`'s `discovery.disabled[]` list.
//!
//! The daemon's boot discovery walker already honours
//! `plugins.discovery.disabled` (a `Vec<String>` of plugin ids it
//! skips). Disabling at runtime therefore = append the id here +
//! hot-remove the live handle; enabling = remove the id + hot-spawn.
//! Because the change lands in the persisted yaml it survives a
//! daemon restart without further action.
//!
//! The patcher loads the file as a generic `serde_yaml::Value`,
//! mutates only `discovery.disabled`, and writes it back so existing
//! `search_paths` / `allowlist` / `auto_detect_binaries` knobs are
//! preserved verbatim.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_yaml::{Mapping, Value};

/// Path to `plugins/discovery.yaml` under the config dir.
fn discovery_yaml_path(config_dir: &Path) -> PathBuf {
    config_dir.join("plugins").join("discovery.yaml")
}

/// Add or remove `plugin_id` from `discovery.disabled[]`.
///
/// - `disabled = true`  → ensure the id IS in the list (disable).
/// - `disabled = false` → ensure the id is NOT in the list (enable).
///
/// Returns `true` when the file actually changed. Idempotent:
/// disabling an already-disabled plugin (or enabling an
/// already-enabled one) is a no-op returning `false`.
pub fn set_plugin_disabled(
    config_dir: &Path,
    plugin_id: &str,
    disabled: bool,
) -> Result<bool> {
    let path = discovery_yaml_path(config_dir);

    // Load existing file or start from an empty mapping. A malformed
    // file is a hard error — we don't want to clobber an operator's
    // hand-edited discovery.yaml by silently overwriting it.
    let mut root: Value = if path.exists() {
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        if body.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&body)
                .with_context(|| format!("parse {}", path.display()))?
        }
    } else {
        Value::Mapping(Mapping::new())
    };

    let root_map = root
        .as_mapping_mut()
        .context("discovery.yaml root is not a mapping")?;

    let discovery = root_map
        .entry(Value::String("discovery".into()))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let discovery_map = discovery
        .as_mapping_mut()
        .context("discovery.yaml `discovery` is not a mapping")?;

    let disabled_val = discovery_map
        .entry(Value::String("disabled".into()))
        .or_insert_with(|| Value::Sequence(Vec::new()));
    let seq = disabled_val
        .as_sequence_mut()
        .context("discovery.yaml `discovery.disabled` is not a sequence")?;

    let present = seq.iter().any(|v| v.as_str() == Some(plugin_id));

    let changed = if disabled {
        if present {
            false
        } else {
            seq.push(Value::String(plugin_id.to_string()));
            true
        }
    } else {
        let before = seq.len();
        seq.retain(|v| v.as_str() != Some(plugin_id));
        seq.len() != before
    };

    if changed {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let body = serde_yaml::to_string(&root).context("serialize discovery.yaml")?;
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
    }

    Ok(changed)
}

/// Read the current `discovery.disabled[]` list. Returns an empty
/// Vec when the file is absent. Used by the adapter to compute a
/// fresh discovery cfg for the enable hot-spawn path.
pub fn read_disabled(config_dir: &Path) -> Result<Vec<String>> {
    let path = discovery_yaml_path(config_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: Value =
        serde_yaml::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    let list = root
        .get("discovery")
        .and_then(|d| d.get("disabled"))
        .and_then(|d| d.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn read_yaml(config_dir: &Path) -> String {
        std::fs::read_to_string(discovery_yaml_path(config_dir)).unwrap()
    }

    #[test]
    fn disable_creates_file_when_absent() {
        let tmp = TempDir::new().unwrap();
        let changed = set_plugin_disabled(tmp.path(), "telegram", true).unwrap();
        assert!(changed);
        assert_eq!(read_disabled(tmp.path()).unwrap(), vec!["telegram"]);
    }

    #[test]
    fn disable_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        assert!(set_plugin_disabled(tmp.path(), "telegram", true).unwrap());
        // Second disable → no change.
        assert!(!set_plugin_disabled(tmp.path(), "telegram", true).unwrap());
        assert_eq!(read_disabled(tmp.path()).unwrap(), vec!["telegram"]);
    }

    #[test]
    fn enable_removes_id() {
        let tmp = TempDir::new().unwrap();
        set_plugin_disabled(tmp.path(), "telegram", true).unwrap();
        set_plugin_disabled(tmp.path(), "whatsapp", true).unwrap();
        let changed = set_plugin_disabled(tmp.path(), "telegram", false).unwrap();
        assert!(changed);
        assert_eq!(read_disabled(tmp.path()).unwrap(), vec!["whatsapp"]);
    }

    #[test]
    fn enable_already_enabled_is_noop() {
        let tmp = TempDir::new().unwrap();
        // Enable a plugin that was never disabled → no change.
        assert!(!set_plugin_disabled(tmp.path(), "telegram", false).unwrap());
    }

    #[test]
    fn preserves_existing_discovery_fields() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        // Operator hand-edited discovery.yaml with search_paths +
        // auto_detect_binaries. Disabling a plugin must NOT drop them.
        std::fs::write(
            plugins_dir.join("discovery.yaml"),
            "discovery:\n  search_paths:\n    - /opt/plugins\n  auto_detect_binaries: false\n",
        )
        .unwrap();
        set_plugin_disabled(tmp.path(), "telegram", true).unwrap();
        let body = read_yaml(tmp.path());
        assert!(body.contains("/opt/plugins"), "search_paths lost: {body}");
        assert!(
            body.contains("auto_detect_binaries"),
            "auto_detect_binaries lost: {body}"
        );
        assert!(body.contains("telegram"), "disabled id missing: {body}");
    }

    #[test]
    fn read_disabled_empty_when_absent() {
        let tmp = TempDir::new().unwrap();
        assert!(read_disabled(tmp.path()).unwrap().is_empty());
    }
}
