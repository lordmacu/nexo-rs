//! Phase 82.10.h.b.b — surface `[capabilities.admin]` and
//! `[capabilities.http_server]` from per-plugin
//! `nexo-plugin.toml` files into the maps `AdminRpcBootstrap`
//! consumes.
//!
//! Boot today reads `plugin.toml` (`nexo-extensions` schema —
//! tools / hooks / channels / providers / pollers) once during
//! discovery. The admin RPC layer needs a SEPARATE manifest
//! (`nexo-plugin.toml` — `nexo-plugin-manifest` schema) that
//! declares which `nexo/admin/*` capabilities the microapp
//! requires + optional, plus the HTTP server bind policy.
//!
//! Plugin authors maintain both files by design — they cover
//! different concerns (runtime contributions vs daemon-RPC
//! orchestration). This module bridges them at boot so main.rs
//! can wire `AdminRpcBootstrap` from a single source of truth
//! without re-parsing inside `run_extension_discovery`.
//!
//! Plugins that ship only `plugin.toml` (legacy / pre-82.10
//! extensions without admin RPC needs) silently skip — the
//! returned map omits their id and `AdminRpcBootstrap::build`
//! treats omission as "no admin RPC declared".

use std::collections::BTreeMap;
use std::path::Path;

use nexo_plugin_manifest::manifest::{
    AdminCapabilities, HttpServerCapability,
};
use nexo_plugin_manifest::{PluginManifest, PLUGIN_MANIFEST_FILENAME};

/// Walk every plugin root, parse `<root>/nexo-plugin.toml` if
/// present, and return a map keyed by `manifest.id()` with the
/// declared admin capabilities. Roots without the file (or with
/// a parse error) are logged + skipped — never fatal — so a
/// single misconfigured plugin can't block the daemon from
/// booting.
pub fn collect_admin_capabilities(
    plugin_roots: impl IntoIterator<Item = impl AsRef<Path>>,
) -> BTreeMap<String, AdminCapabilities> {
    let mut out = BTreeMap::new();
    for root in plugin_roots {
        let root = root.as_ref();
        let path = root.join(PLUGIN_MANIFEST_FILENAME);
        if !path.exists() {
            continue;
        }
        match PluginManifest::from_path(&path) {
            Ok(m) => {
                let id = m.id().to_string();
                if !m.plugin.capabilities.admin.declared().is_empty() {
                    out.insert(id, m.plugin.capabilities.admin);
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "manifest_collect: skipping malformed nexo-plugin.toml",
                );
            }
        }
    }
    out
}

/// Same shape as [`collect_admin_capabilities`] but returns the
/// HTTP server capability (Phase 82.12) for plugins that declare
/// one. Plugins without the section omit; boot's bind-policy
/// validator skips checks for omitted plugins.
pub fn collect_http_server_capabilities(
    plugin_roots: impl IntoIterator<Item = impl AsRef<Path>>,
) -> BTreeMap<String, HttpServerCapability> {
    let mut out = BTreeMap::new();
    for root in plugin_roots {
        let root = root.as_ref();
        let path = root.join(PLUGIN_MANIFEST_FILENAME);
        if !path.exists() {
            continue;
        }
        match PluginManifest::from_path(&path) {
            Ok(m) => {
                let id = m.id().to_string();
                if let Some(http) = m.plugin.capabilities.http_server {
                    out.insert(id, http);
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "manifest_collect: skipping malformed nexo-plugin.toml",
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(root: &Path, body: &str) {
        std::fs::write(root.join(PLUGIN_MANIFEST_FILENAME), body).unwrap();
    }

    fn manifest_with_admin(id: &str, required: &[&str], optional: &[&str]) -> String {
        let req = required
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let opt = optional
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"
[plugin]
id = "{id}"
version = "0.1.0"
name = "{id}"
description = "test"
min_nexo_version = ">=0.0.0"

[plugin.capabilities.admin]
required = [{req}]
optional = [{opt}]
"#
        )
    }

    fn manifest_with_http(id: &str, bind: &str) -> String {
        format!(
            r#"
[plugin]
id = "{id}"
version = "0.1.0"
name = "{id}"
description = "test"
min_nexo_version = ">=0.0.0"

[plugin.capabilities.http_server]
bind = "{bind}"
port = 8080
health_path = "/healthz"
token_env = "TOKEN"
"#
        )
    }

    #[test]
    fn collect_admin_picks_up_declared_capabilities_per_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = dir.path().join("alpha");
        let beta = dir.path().join("beta");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&beta).unwrap();
        write_manifest(&alpha, &manifest_with_admin("alpha", &["agents_crud"], &[]));
        write_manifest(
            &beta,
            &manifest_with_admin("beta", &["agents_crud"], &["llm_keys_crud"]),
        );

        let map = collect_admin_capabilities([&alpha, &beta]);
        assert_eq!(map.len(), 2);
        assert_eq!(map["alpha"].required, vec!["agents_crud"]);
        assert!(map["alpha"].optional.is_empty());
        assert_eq!(map["beta"].required, vec!["agents_crud"]);
        assert_eq!(map["beta"].optional, vec!["llm_keys_crud"]);
    }

    #[test]
    fn collect_admin_skips_plugins_without_declared_capabilities() {
        // A manifest present but with empty admin caps gets
        // skipped — same effective behavior as a plugin without
        // the section at all (omit from the map).
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        write_manifest(&bare, &manifest_with_admin("bare", &[], &[]));

        let map = collect_admin_capabilities([&bare]);
        assert!(map.is_empty(), "no caps declared → omit from map");
    }

    #[test]
    fn collect_admin_silently_skips_roots_without_manifest_file() {
        let dir = tempfile::tempdir().unwrap();
        let nada = dir.path().join("nada");
        std::fs::create_dir_all(&nada).unwrap();
        // No nexo-plugin.toml written — legacy plugin.

        let map = collect_admin_capabilities([&nada]);
        assert!(map.is_empty());
    }

    #[test]
    fn collect_admin_logs_and_skips_malformed_manifests_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad");
        let good = dir.path().join("good");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::create_dir_all(&good).unwrap();
        // Garbage TOML in `bad` — must not panic + must not stop
        // discovery of `good`.
        write_manifest(&bad, "not = a [valid \"manifest");
        write_manifest(&good, &manifest_with_admin("good", &["agents_crud"], &[]));

        let map = collect_admin_capabilities([&bad, &good]);
        assert_eq!(map.len(), 1, "only `good` survives");
        assert!(map.contains_key("good"));
    }

    #[test]
    fn collect_http_picks_up_declared_capability() {
        let dir = tempfile::tempdir().unwrap();
        let ui = dir.path().join("ui");
        std::fs::create_dir_all(&ui).unwrap();
        write_manifest(&ui, &manifest_with_http("ui", "127.0.0.1"));

        let map = collect_http_server_capabilities([&ui]);
        assert_eq!(map.len(), 1);
        assert_eq!(map["ui"].bind, "127.0.0.1");
        assert_eq!(map["ui"].port, 8080);
    }

    #[test]
    fn collect_http_omits_plugins_without_http_section() {
        let dir = tempfile::tempdir().unwrap();
        let stdio = dir.path().join("stdio-only");
        std::fs::create_dir_all(&stdio).unwrap();
        write_manifest(
            &stdio,
            &manifest_with_admin("stdio-only", &["agents_crud"], &[]),
        );

        let map = collect_http_server_capabilities([&stdio]);
        assert!(map.is_empty(), "no http_server section → omit");
    }

    #[test]
    fn collect_helpers_share_walk_so_one_loop_covers_both_axes() {
        // Plugin author can declare both blocks in the same file;
        // both helpers see the same plugin id keyed in their
        // respective maps. Boot calls them sequentially over the
        // same plugin_roots list.
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("full");
        std::fs::create_dir_all(&full).unwrap();
        let body = format!(
            r#"
[plugin]
id = "full"
version = "0.1.0"
name = "full"
description = "test"
min_nexo_version = ">=0.0.0"

[plugin.capabilities.admin]
required = ["agents_crud"]

[plugin.capabilities.http_server]
bind = "127.0.0.1"
port = 9090
health_path = "/healthz"
token_env = "TOKEN"
"#
        );
        write_manifest(&full, &body);

        let admins = collect_admin_capabilities([&full]);
        let https = collect_http_server_capabilities([&full]);
        assert_eq!(admins.len(), 1);
        assert_eq!(https.len(), 1);
        assert!(admins.contains_key("full"));
        assert!(https.contains_key("full"));
    }
}
