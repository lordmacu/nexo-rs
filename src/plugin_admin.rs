//! Phase 31.8 — `nexo plugin {list,upgrade,remove}` operator UI.
//!
//! Shipping CRUD for installed plugins on top of the Phase 31.1.c
//! install pipeline:
//!
//! - `list` walks `cfg.plugins.discovery.search_paths`, decodes
//!   each plugin's `nexo-plugin.toml` + `.nexo-install.json`, and
//!   prints a tabular (or JSON) inventory.
//! - `upgrade <id>` looks up install metadata, re-resolves the
//!   GitHub Release for `<owner>/<repo>@<tag>` (tag defaults to
//!   `latest`), checks for no-op / downgrade, then delegates to
//!   the install pipeline so the new tarball is downloaded,
//!   verified, extracted, and metadata is rewritten atomically.
//! - `remove <id>` atomically renames the plugin directory aside
//!   then deletes it; with `--purge-cache` it also walks the
//!   per-plugin state directories under `nexo_state_dir()` and
//!   deletes them.
//!
//! Metadata schema in `<plugin_dir>/.nexo-install.json` (v1.0)
//! is written by the install pipeline (Phase 31.8 also patched
//! `plugin_install.rs` to emit it). Plugins missing the file are
//! "orphans" — `list` hides them by default and `upgrade` refuses
//! to operate on them (operator must reinstall via `plugin
//! install` to seed metadata).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use nexo_ext_installer::{
    resolve_release, InstallError, PluginCoords, DEFAULT_GITHUB_API_BASE,
};
use nexo_plugin_manifest::PluginManifest;
use serde::{Deserialize, Serialize};

const BROKER_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const METADATA_FILENAME: &str = ".nexo-install.json";
const METADATA_SCHEMA_VERSION: &str = "1.0";

// ── Metadata schema ────────────────────────────────────────────────

/// Per-plugin install metadata persisted at
/// `<plugin_dir>/.nexo-install.json`. Written by `plugin install`
/// (and `plugin upgrade`) so that subsequent `plugin upgrade`
/// invocations know how to re-resolve the release. Operators can
/// hand-edit the file (e.g. to pin to a tag) but the schema is
/// stable across daemon versions thanks to `schema_version`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallMetadata {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub owner: String,
    pub repo: String,
    /// Tag the operator originally requested. `"latest"` is
    /// preserved literally so `plugin upgrade` keeps tracking the
    /// floating tag; explicit tags (`"v0.2.0"`) pin the upgrade
    /// resolver to that specific release.
    pub tag: String,
    pub target: String,
    pub sha256: String,
    /// RFC3339 UTC timestamp.
    pub installed_at: String,
    /// Source identifier (`"github-releases"` for v1).
    pub source: String,
}

impl InstallMetadata {
    /// Build a fresh metadata record stamped with `now()`.
    pub fn new_now(
        id: String,
        version: String,
        owner: String,
        repo: String,
        tag: String,
        target: String,
        sha256: String,
        source: String,
    ) -> Self {
        Self {
            schema_version: METADATA_SCHEMA_VERSION.to_string(),
            id,
            version,
            owner,
            repo,
            tag,
            target,
            sha256,
            installed_at: chrono::Utc::now().to_rfc3339(),
            source,
        }
    }
}

// ── Errors ─────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("plugin `{id}` is not installed under any configured search path")]
    PluginNotFound { id: String },
    #[error(
        "plugin `{id}` has no .nexo-install.json metadata; reinstall with `plugin install` to enable upgrades"
    )]
    NoInstallMetadata { id: String },
    #[error("metadata at {path} is malformed: {error}")]
    MetadataInvalid { path: PathBuf, error: String },
    #[error("manifest at {path} is malformed: {error}")]
    ManifestRead { path: PathBuf, error: String },
    #[error("failed to load config from {path}: {error}")]
    ConfigLoad { path: PathBuf, error: String },
    #[error("io error: {0}")]
    Io(String),
    #[error(
        "refusing to downgrade plugin `{id}` from {from} to {to}; pass --tag <higher> or remove + reinstall"
    )]
    DowngradeRefused {
        id: String,
        from: String,
        to: String,
    },
    #[error(
        "removing plugin `{id}` requires confirmation; pass --yes (interactive prompt deferred to 31.8.b)"
    )]
    NeedsYesConfirm { id: String },
    #[error("resolve failed: {0}")]
    Resolve(#[from] InstallError),
}

pub fn admin_error_kind(e: &AdminError) -> &'static str {
    match e {
        AdminError::PluginNotFound { .. } => "PluginNotFound",
        AdminError::NoInstallMetadata { .. } => "NoInstallMetadata",
        AdminError::MetadataInvalid { .. } => "MetadataInvalid",
        AdminError::ManifestRead { .. } => "ManifestRead",
        AdminError::ConfigLoad { .. } => "ConfigLoad",
        AdminError::Io(_) => "Io",
        AdminError::DowngradeRefused { .. } => "DowngradeRefused",
        AdminError::NeedsYesConfirm { .. } => "NeedsYesConfirm",
        AdminError::Resolve(_) => "Resolve",
    }
}

// ── Metadata IO ────────────────────────────────────────────────────

pub fn metadata_path(plugin_dir: &Path) -> PathBuf {
    plugin_dir.join(METADATA_FILENAME)
}

/// Read `<plugin_dir>/.nexo-install.json`. `Ok(None)` when the
/// file is absent (orphan plugin); `Err` when present-but-malformed.
pub fn read_install_metadata(plugin_dir: &Path) -> Result<Option<InstallMetadata>, AdminError> {
    let path = metadata_path(plugin_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AdminError::Io(format!("read {}: {e}", path.display()))),
    };
    match serde_json::from_slice::<InstallMetadata>(&bytes) {
        Ok(m) => Ok(Some(m)),
        Err(e) => Err(AdminError::MetadataInvalid {
            path,
            error: e.to_string(),
        }),
    }
}

/// Atomic write — serializes metadata to `<plugin_dir>/.nexo-install.json.tmp`
/// then renames over the target. Caller guarantees `plugin_dir` exists.
pub fn write_install_metadata(plugin_dir: &Path, meta: &InstallMetadata) -> Result<(), AdminError> {
    let dest = metadata_path(plugin_dir);
    let tmp = plugin_dir.join(format!("{}.tmp", METADATA_FILENAME));
    let body = serde_json::to_string_pretty(meta)
        .map_err(|e| AdminError::Io(format!("serialize metadata: {e}")))?;
    std::fs::write(&tmp, body.as_bytes())
        .map_err(|e| AdminError::Io(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &dest)
        .map_err(|e| AdminError::Io(format!("rename {} -> {}: {e}", tmp.display(), dest.display())))?;
    Ok(())
}

// ── Plugin discovery ──────────────────────────────────────────────

/// One installed plugin discovered on disk.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub plugin_dir: PathBuf,
    pub manifest: PluginManifest,
    pub metadata: Option<InstallMetadata>,
}

fn read_local_manifest(plugin_dir: &Path) -> Result<PluginManifest, AdminError> {
    let path = plugin_dir.join("nexo-plugin.toml");
    let body = std::fs::read_to_string(&path)
        .map_err(|e| AdminError::Io(format!("read {}: {e}", path.display())))?;
    PluginManifest::from_str(&body).map_err(|e| AdminError::ManifestRead {
        path,
        error: e.to_string(),
    })
}

/// Walk every `search_path` and collect immediate child dirs that
/// contain a parseable `nexo-plugin.toml`. Skips dirs without a
/// manifest (rejected silently — they're not plugins). Skips dirs
/// whose manifest fails to parse (rejected silently — operator can
/// debug with `plugin run <dir>`). Each `DiscoveredPlugin` carries
/// its `metadata` if `.nexo-install.json` is present.
pub fn discover_installed_plugins(search_paths: &[PathBuf]) -> Vec<DiscoveredPlugin> {
    let mut out = Vec::new();
    for root in search_paths {
        let entries = match std::fs::read_dir(root) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let plugin_dir = entry.path();
            if !plugin_dir.is_dir() {
                continue;
            }
            let manifest = match read_local_manifest(&plugin_dir) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let metadata = read_install_metadata(&plugin_dir).ok().flatten();
            out.push(DiscoveredPlugin {
                plugin_dir,
                manifest,
                metadata,
            });
        }
    }
    out.sort_by(|a, b| a.manifest.plugin.id.cmp(&b.manifest.plugin.id));
    out
}

// ── `plugin list` ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PluginListEntry {
    pub id: String,
    pub version: String,
    pub plugin_dir: PathBuf,
    pub is_orphan: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginListReport {
    pub ok: bool,
    pub plugins: Vec<PluginListEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginAdminErrorReport {
    pub ok: bool,
    pub kind: &'static str,
    pub error: String,
}

fn discovered_to_entry(d: DiscoveredPlugin) -> PluginListEntry {
    let id = d.manifest.plugin.id.clone();
    let version = d.manifest.plugin.version.to_string();
    match d.metadata {
        Some(m) => PluginListEntry {
            id,
            version,
            plugin_dir: d.plugin_dir,
            is_orphan: false,
            target: Some(m.target),
            tag: Some(m.tag),
            owner: Some(m.owner),
            repo: Some(m.repo),
            sha256: Some(m.sha256),
            installed_at: Some(m.installed_at),
            source: Some(m.source),
        },
        None => PluginListEntry {
            id,
            version,
            plugin_dir: d.plugin_dir,
            is_orphan: true,
            target: None,
            tag: None,
            owner: None,
            repo: None,
            sha256: None,
            installed_at: None,
            source: None,
        },
    }
}

pub async fn run_plugin_list(
    config_dir: &Path,
    include_orphan: bool,
    json: bool,
) -> Result<i32> {
    let cfg = match nexo_config::AppConfig::load(config_dir) {
        Ok(c) => c,
        Err(e) => {
            return Ok(emit_admin_error(
                &AdminError::ConfigLoad {
                    path: config_dir.to_path_buf(),
                    error: e.to_string(),
                },
                json,
            ))
        }
    };

    let discovered = discover_installed_plugins(&cfg.plugins.discovery.search_paths);
    let entries: Vec<PluginListEntry> = discovered
        .into_iter()
        .map(discovered_to_entry)
        .filter(|e| include_orphan || !e.is_orphan)
        .collect();

    if json {
        let report = PluginListReport {
            ok: true,
            plugins: entries,
        };
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else if entries.is_empty() {
        eprintln!(
            "(no plugins installed under {} configured search path(s))",
            cfg.plugins.discovery.search_paths.len()
        );
    } else {
        print_list_table(&entries);
    }
    Ok(0)
}

fn print_list_table(entries: &[PluginListEntry]) {
    let id_w = entries
        .iter()
        .map(|e| e.id.len())
        .max()
        .unwrap_or(2)
        .max("ID".len());
    let version_w = entries
        .iter()
        .map(|e| e.version.len())
        .max()
        .unwrap_or(7)
        .max("VERSION".len());
    let target_w = entries
        .iter()
        .map(|e| e.target.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(6)
        .max("TARGET".len());
    let tag_w = entries
        .iter()
        .map(|e| e.tag.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(3)
        .max("TAG".len());

    println!(
        "{:<id_w$}  {:<version_w$}  {:<target_w$}  {:<tag_w$}  {}",
        "ID",
        "VERSION",
        "TARGET",
        "TAG",
        "INSTALLED_AT",
        id_w = id_w,
        version_w = version_w,
        target_w = target_w,
        tag_w = tag_w,
    );
    for e in entries {
        let target = e.target.as_deref().unwrap_or("-");
        let tag = e.tag.as_deref().unwrap_or("-");
        let installed_at = e.installed_at.as_deref().unwrap_or("-");
        let suffix = if e.is_orphan { "  (orphan)" } else { "" };
        println!(
            "{:<id_w$}  {:<version_w$}  {:<target_w$}  {:<tag_w$}  {}{}",
            e.id,
            e.version,
            target,
            tag,
            installed_at,
            suffix,
            id_w = id_w,
            version_w = version_w,
            target_w = target_w,
            tag_w = tag_w,
        );
    }
}

// ── `plugin upgrade` ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PluginUpgradeReport {
    pub ok: bool,
    pub id: String,
    pub from_version: String,
    pub to_version: String,
    pub plugin_dir: PathBuf,
    pub was_no_op: bool,
}

pub async fn run_plugin_upgrade(
    config_dir: &Path,
    id: String,
    target_override: Option<String>,
    require_signature: bool,
    skip_signature_verify: bool,
    json: bool,
) -> Result<i32> {
    let cfg = match nexo_config::AppConfig::load(config_dir) {
        Ok(c) => c,
        Err(e) => {
            return Ok(emit_admin_error(
                &AdminError::ConfigLoad {
                    path: config_dir.to_path_buf(),
                    error: e.to_string(),
                },
                json,
            ))
        }
    };

    let discovered = discover_installed_plugins(&cfg.plugins.discovery.search_paths);
    let installed = match discovered
        .into_iter()
        .find(|d| d.manifest.plugin.id == id)
    {
        Some(d) => d,
        None => return Ok(emit_admin_error(&AdminError::PluginNotFound { id }, json)),
    };
    let metadata = match installed.metadata.clone() {
        Some(m) => m,
        None => {
            return Ok(emit_admin_error(
                &AdminError::NoInstallMetadata { id: id.clone() },
                json,
            ))
        }
    };

    let target = target_override.unwrap_or_else(|| metadata.target.clone());
    let coords = PluginCoords {
        owner: metadata.owner.clone(),
        repo: metadata.repo.clone(),
        tag: metadata.tag.clone(),
    };

    if !json {
        eprintln!(
            "→ Resolving {}/{}@{} (current: {}, target: {})",
            coords.owner, coords.repo, coords.tag, metadata.version, target
        );
    }

    let api_base = std::env::var("NEXO_GITHUB_API_BASE")
        .unwrap_or_else(|_| DEFAULT_GITHUB_API_BASE.to_string());
    let client = build_reqwest_client();
    let resolved = match resolve_release(&client, &coords, &target, &api_base).await {
        Ok(r) => r,
        Err(e) => return Ok(emit_admin_error(&AdminError::Resolve(e), json)),
    };

    let new_version = resolved.entry.version.to_string();
    let current = match semver::Version::parse(&metadata.version) {
        Ok(v) => v,
        Err(e) => {
            return Ok(emit_admin_error(
                &AdminError::MetadataInvalid {
                    path: metadata_path(&installed.plugin_dir),
                    error: format!("version not semver: {e}"),
                },
                json,
            ))
        }
    };

    if resolved.entry.version == current {
        let report = PluginUpgradeReport {
            ok: true,
            id: id.clone(),
            from_version: metadata.version.clone(),
            to_version: new_version.clone(),
            plugin_dir: installed.plugin_dir.clone(),
            was_no_op: true,
        };
        if json {
            println!("{}", serde_json::to_string(&report).unwrap_or_default());
        } else {
            eprintln!("✓ Already on {} {} — nothing to do", id, new_version);
        }
        return Ok(0);
    }

    if resolved.entry.version < current {
        return Ok(emit_admin_error(
            &AdminError::DowngradeRefused {
                id: id.clone(),
                from: metadata.version.clone(),
                to: new_version.clone(),
            },
            json,
        ));
    }

    if !json {
        eprintln!(
            "→ Upgrading {} from {} to {}",
            id, metadata.version, new_version
        );
    }

    // Delegate to the install pipeline so cosign verification +
    // metadata write reuse one code path. The install report is
    // the upgrade's JSON line in this delegated path; operators
    // detect upgrade vs no-op vs error via top-level `ok` /
    // `was_already_present`.
    let coords_str = format!("{}/{}@{}", coords.owner, coords.repo, coords.tag);
    crate::plugin_install::run_plugin_install(
        config_dir,
        coords_str,
        None,
        Some(target),
        json,
        require_signature,
        skip_signature_verify,
    )
    .await
}

// ── `plugin remove` ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PluginRemoveReport {
    pub ok: bool,
    pub id: String,
    pub version: String,
    pub plugin_dir: PathBuf,
    pub removed_paths: Vec<PathBuf>,
    pub cache_purged: bool,
    pub lifecycle_event_emitted: bool,
}

pub async fn run_plugin_remove(
    config_dir: &Path,
    id: String,
    purge_cache: bool,
    yes: bool,
    json: bool,
) -> Result<i32> {
    let cfg = match nexo_config::AppConfig::load(config_dir) {
        Ok(c) => c,
        Err(e) => {
            return Ok(emit_admin_error(
                &AdminError::ConfigLoad {
                    path: config_dir.to_path_buf(),
                    error: e.to_string(),
                },
                json,
            ))
        }
    };

    let discovered = discover_installed_plugins(&cfg.plugins.discovery.search_paths);
    let installed = match discovered
        .into_iter()
        .find(|d| d.manifest.plugin.id == id)
    {
        Some(d) => d,
        None => return Ok(emit_admin_error(&AdminError::PluginNotFound { id }, json)),
    };

    if !yes {
        return Ok(emit_admin_error(
            &AdminError::NeedsYesConfirm { id: id.clone() },
            json,
        ));
    }

    let plugin_dir = installed.plugin_dir.clone();
    let version = installed.manifest.plugin.version.to_string();

    let mut removed_paths = Vec::new();

    // Atomic remove: rename aside, then rm.
    let aside = aside_path(&plugin_dir);
    if let Err(e) = std::fs::rename(&plugin_dir, &aside) {
        return Ok(emit_admin_error(
            &AdminError::Io(format!(
                "rename {} -> {}: {e}",
                plugin_dir.display(),
                aside.display()
            )),
            json,
        ));
    }
    if let Err(e) = std::fs::remove_dir_all(&aside) {
        // Plugin dir is gone from operator's POV but the aside
        // pile remains. Not fatal; surface a warning.
        eprintln!(
            "! Failed to remove staged dir {}: {e} (delete manually)",
            aside.display()
        );
    } else {
        removed_paths.push(plugin_dir.clone());
    }

    let mut cache_purged = false;
    if purge_cache {
        let state_root = nexo_project_tracker::state::nexo_state_dir();
        for sub in ["plugins", "plugins/cache"] {
            let cache_dir = state_root.join(sub).join(&id);
            if cache_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&cache_dir) {
                    eprintln!(
                        "! Failed to purge cache {}: {e} (skip)",
                        cache_dir.display()
                    );
                } else {
                    removed_paths.push(cache_dir);
                    cache_purged = true;
                }
            }
        }
    }

    let lifecycle_emitted =
        emit_lifecycle_removed_event(&cfg, &id, &version, &plugin_dir).await;

    let report = PluginRemoveReport {
        ok: true,
        id: id.clone(),
        version,
        plugin_dir,
        removed_paths,
        cache_purged,
        lifecycle_event_emitted: lifecycle_emitted,
    };

    if json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        eprintln!("✓ Plugin `{}` removed from {}", id, report.plugin_dir.display());
        if cache_purged {
            eprintln!("✓ Cache purged");
        }
        if lifecycle_emitted {
            eprintln!("✓ Lifecycle event emitted (broker)");
        }
    }
    Ok(0)
}

fn aside_path(plugin_dir: &Path) -> PathBuf {
    let suffix: u32 = rand::random();
    let mut name = plugin_dir
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".removing-{:08x}", suffix));
    plugin_dir.with_file_name(name)
}

fn build_reqwest_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static("nexo-plugin-admin"),
    );
    if let Ok(token) = std::env::var("NEXO_GITHUB_TOKEN") {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
            headers.insert(reqwest::header::AUTHORIZATION, v);
        }
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

async fn emit_lifecycle_removed_event(
    cfg: &nexo_config::AppConfig,
    id: &str,
    version: &str,
    plugin_dir: &Path,
) -> bool {
    let broker = &cfg.broker.broker;
    if broker.kind != nexo_config::types::broker::BrokerKind::Nats {
        return false;
    }
    let url = broker.url.clone();
    let payload = serde_json::json!({
        "plugin_id": id,
        "version": version,
        "plugin_dir": plugin_dir.display().to_string(),
        "source": "plugin.cli",
    });
    let bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let topic = format!("plugin.lifecycle.{id}.removed");
    let connect = async move { async_nats::connect(&url).await };
    let client = match tokio::time::timeout(BROKER_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(c)) => c,
        _ => return false,
    };
    if client.publish(topic, bytes.into()).await.is_err() {
        return false;
    }
    let _ = client.flush().await;
    true
}

// ── Error emission helper ─────────────────────────────────────────

fn emit_admin_error(err: &AdminError, json: bool) -> i32 {
    let kind = admin_error_kind(err);
    if json {
        let report = PluginAdminErrorReport {
            ok: false,
            kind,
            error: err.to_string(),
        };
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        eprintln!("✗ {}", err);
    }
    1
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;
    use tempfile::tempdir;

    fn write_manifest(dir: &Path, id: &str, version: &str) {
        let body = format!(
            "[plugin]\nid = \"{id}\"\nversion = \"{version}\"\nname = \"X\"\ndescription = \"d\"\nmin_nexo_version = \">=0.0.0\"\n\n[plugin.entrypoint]\ncommand = \"./bin/{id}\"\n"
        );
        std::fs::write(dir.join("nexo-plugin.toml"), body).unwrap();
    }

    fn sample_metadata(id: &str, version: &str) -> InstallMetadata {
        InstallMetadata::new_now(
            id.to_string(),
            version.to_string(),
            "alice".to_string(),
            format!("nexo-plugin-{id}"),
            "latest".to_string(),
            "x86_64-unknown-linux-gnu".to_string(),
            "abc123".to_string(),
            "github-releases".to_string(),
        )
    }

    #[test]
    fn install_metadata_round_trips() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("foo-0.1.0");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let m = sample_metadata("foo", "0.1.0");
        write_install_metadata(&plugin_dir, &m).unwrap();
        let read = read_install_metadata(&plugin_dir).unwrap().unwrap();
        assert_eq!(read, m);
        assert_eq!(read.schema_version, METADATA_SCHEMA_VERSION);
    }

    #[test]
    fn read_metadata_returns_none_when_absent() {
        let dir = tempdir().unwrap();
        assert!(read_install_metadata(dir.path()).unwrap().is_none());
    }

    #[test]
    fn read_metadata_errors_on_invalid_json() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(METADATA_FILENAME), b"not-json").unwrap();
        let err = read_install_metadata(dir.path()).unwrap_err();
        assert!(matches!(err, AdminError::MetadataInvalid { .. }));
    }

    #[test]
    fn discover_skips_dirs_without_manifest() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("not-a-plugin")).unwrap();
        std::fs::create_dir_all(dir.path().join("real-plugin")).unwrap();
        write_manifest(&dir.path().join("real-plugin"), "real_plugin", "0.1.0");
        let found = discover_installed_plugins(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.plugin.id, "real_plugin");
        assert_eq!(found[0].manifest.plugin.version, Version::new(0, 1, 0));
    }

    #[test]
    fn discover_collects_metadata_when_present() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("foo");
        std::fs::create_dir_all(&p).unwrap();
        write_manifest(&p, "foo", "0.1.0");
        write_install_metadata(&p, &sample_metadata("foo", "0.1.0")).unwrap();
        let found = discover_installed_plugins(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert!(found[0].metadata.is_some());
    }

    #[test]
    fn list_filters_orphans_by_default() {
        let dir = tempdir().unwrap();
        let orphan = dir.path().join("orphan");
        std::fs::create_dir_all(&orphan).unwrap();
        write_manifest(&orphan, "orphan_plugin", "0.1.0");
        let with_meta = dir.path().join("metaplug");
        std::fs::create_dir_all(&with_meta).unwrap();
        write_manifest(&with_meta, "metaplug", "0.2.0");
        write_install_metadata(&with_meta, &sample_metadata("metaplug", "0.2.0")).unwrap();
        let all = discover_installed_plugins(&[dir.path().to_path_buf()]);
        assert_eq!(all.len(), 2);
        let entries: Vec<_> = all
            .into_iter()
            .map(discovered_to_entry)
            .filter(|e| !e.is_orphan)
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "metaplug");
    }

    #[test]
    fn list_includes_orphans_with_flag() {
        let dir = tempdir().unwrap();
        let orphan = dir.path().join("orphan");
        std::fs::create_dir_all(&orphan).unwrap();
        write_manifest(&orphan, "orphan_plugin", "0.1.0");
        let entries: Vec<_> = discover_installed_plugins(&[dir.path().to_path_buf()])
            .into_iter()
            .map(discovered_to_entry)
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_orphan);
        assert!(entries[0].target.is_none());
    }

    #[test]
    fn list_empty_when_search_paths_missing() {
        let entries = discover_installed_plugins(&[PathBuf::from("/no/such/dir")]);
        assert!(entries.is_empty());
    }

    #[test]
    fn admin_error_kind_maps_all_variants() {
        let cases: Vec<(AdminError, &'static str)> = vec![
            (AdminError::PluginNotFound { id: "x".into() }, "PluginNotFound"),
            (
                AdminError::NoInstallMetadata { id: "x".into() },
                "NoInstallMetadata",
            ),
            (
                AdminError::MetadataInvalid {
                    path: PathBuf::from("/x"),
                    error: "y".into(),
                },
                "MetadataInvalid",
            ),
            (
                AdminError::ManifestRead {
                    path: PathBuf::from("/x"),
                    error: "y".into(),
                },
                "ManifestRead",
            ),
            (
                AdminError::ConfigLoad {
                    path: PathBuf::from("/x"),
                    error: "y".into(),
                },
                "ConfigLoad",
            ),
            (AdminError::Io("x".into()), "Io"),
            (
                AdminError::DowngradeRefused {
                    id: "x".into(),
                    from: "1".into(),
                    to: "0".into(),
                },
                "DowngradeRefused",
            ),
            (AdminError::NeedsYesConfirm { id: "x".into() }, "NeedsYesConfirm"),
            (
                AdminError::Resolve(InstallError::Http("x".into())),
                "Resolve",
            ),
        ];
        for (e, want) in cases {
            assert_eq!(admin_error_kind(&e), want);
        }
    }

    #[test]
    fn aside_path_differs_from_origin() {
        let p = PathBuf::from("/plugins/foo-0.1.0");
        let a = aside_path(&p);
        assert_ne!(a, p);
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("foo-0.1.0.removing-")
        );
    }

    #[test]
    fn list_entry_includes_metadata_fields_when_present() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("foo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        write_manifest(&plugin_dir, "foo", "0.1.0");
        write_install_metadata(&plugin_dir, &sample_metadata("foo", "0.1.0")).unwrap();
        let entry = discover_installed_plugins(&[dir.path().to_path_buf()])
            .into_iter()
            .map(discovered_to_entry)
            .next()
            .unwrap();
        assert!(!entry.is_orphan);
        assert_eq!(entry.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(entry.tag.as_deref(), Some("latest"));
        assert_eq!(entry.owner.as_deref(), Some("alice"));
        assert_eq!(entry.repo.as_deref(), Some("nexo-plugin-foo"));
        assert_eq!(entry.sha256.as_deref(), Some("abc123"));
        assert_eq!(entry.source.as_deref(), Some("github-releases"));
    }
}
