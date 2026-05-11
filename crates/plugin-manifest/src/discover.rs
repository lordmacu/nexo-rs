//! Plugin manifest discovery in a plugin root.
//!
//! The framework used to look for two different files in
//! disjoint code paths: `nexo-extensions::ExtensionDiscovery`
//! walked `plugin.toml`, while
//! `crates/setup/src/admin_capability_collect.rs` walked
//! `nexo-plugin.toml`. Both paths now share this single
//! resolver, which prefers `plugin.toml` (the canonical
//! filename) and falls back to `nexo-plugin.toml` for plugins
//! that haven't migrated yet.

use std::path::{Path, PathBuf};

/// File names this resolver looks at, in priority order.
///
/// `plugin.toml` is the canonical filename the framework
/// standardised on; `nexo-plugin.toml` is the legacy modern-
/// schema filename kept around for one deprecation cycle so
/// existing plugins (8 in-tree, plus out-of-tree microapps)
/// keep parsing without an immediate rename.
pub const PLUGIN_MANIFEST_FILENAMES: &[&str] = &["plugin.toml", "nexo-plugin.toml"];

/// Walk `plugin_root` and return the first manifest file found
/// per [`PLUGIN_MANIFEST_FILENAMES`] order. `None` when neither
/// file exists.
///
/// Side-effects:
/// - Both files present → `tracing::warn!` + return `plugin.toml`.
/// - Only `nexo-plugin.toml` present → `tracing::warn!` recommending
///   the rename + return it.
/// - Only `plugin.toml` present → no warn, return it.
/// - Neither present → return `None`.
pub fn discover_in_root(plugin_root: &Path) -> Option<PathBuf> {
    let canonical = plugin_root.join(PLUGIN_MANIFEST_FILENAMES[0]); // plugin.toml
    let legacy = plugin_root.join(PLUGIN_MANIFEST_FILENAMES[1]); // nexo-plugin.toml

    let canonical_present = canonical.exists();
    let legacy_present = legacy.exists();

    match (canonical_present, legacy_present) {
        (true, true) => {
            tracing::warn!(
                plugin_root = %plugin_root.display(),
                "both plugin.toml and nexo-plugin.toml present; using plugin.toml — please remove nexo-plugin.toml (Phase 81.13)",
            );
            Some(canonical)
        }
        (true, false) => Some(canonical),
        (false, true) => {
            tracing::warn!(
                plugin_root = %plugin_root.display(),
                "found legacy nexo-plugin.toml; please rename to plugin.toml (Phase 81.13 unifies on the canonical filename)",
            );
            Some(legacy)
        }
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn discover_returns_none_when_neither_present() {
        let dir = TempDir::new().unwrap();
        assert!(discover_in_root(dir.path()).is_none());
    }

    #[test]
    fn discover_returns_plugin_toml_when_only_present() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("plugin.toml");
        std::fs::write(&p, "manifest_version = 2\n").unwrap();
        assert_eq!(discover_in_root(dir.path()), Some(p));
    }

    #[test]
    fn discover_returns_nexo_plugin_toml_when_only_present() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("nexo-plugin.toml");
        std::fs::write(&p, "manifest_version = 2\n").unwrap();
        assert_eq!(discover_in_root(dir.path()), Some(p));
    }

    #[test]
    fn discover_prefers_plugin_toml_when_both_present() {
        let dir = TempDir::new().unwrap();
        let canonical = dir.path().join("plugin.toml");
        let legacy = dir.path().join("nexo-plugin.toml");
        std::fs::write(&canonical, "manifest_version = 2\n").unwrap();
        std::fs::write(&legacy, "manifest_version = 2\n").unwrap();
        assert_eq!(discover_in_root(dir.path()), Some(canonical));
    }

    #[test]
    fn filenames_const_lists_canonical_first() {
        // Locking the order — flipping it would silently swap
        // the priority + alter operator-visible behaviour.
        assert_eq!(
            PLUGIN_MANIFEST_FILENAMES,
            &["plugin.toml", "nexo-plugin.toml"]
        );
    }
}
