//! Phase 31.1.c — `nexo plugin install <coords>` CLI integration.
//!
//! Wraps the `nexo-ext-installer` resolve / download / verify /
//! extract pipeline into a single operator-facing command.
//! Resolves the install dest from
//! `cfg.plugins.discovery.search_paths[0]` (with a state-dir
//! fallback), resolves the target triple from `--target` / env /
//! autodetect, downloads + verifies + extracts, then best-effort
//! emits a `plugin.lifecycle.<id>.installed` event on the broker
//! so any subsystem that cares (future hot-reload) can react.
//!
//! The crate intentionally exposes JSON output (`--json`) for
//! scripted operator workflows (CI, ansible playbooks).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use nexo_ext_installer::{
    download_and_verify, extract_verified_tarball, resolve_release, ExtractError, ExtractInput,
    ExtractLimits, InstallError, PluginCoords, ResolvedInstall, DEFAULT_GITHUB_API_BASE,
};
use serde::Serialize;

const BROKER_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Public report types ────────────────────────────────────────────

/// Successful install report. Serialized to JSON when `--json`
/// is passed; rendered to multiple human lines otherwise.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInstallReport {
    pub ok: bool,
    pub id: String,
    pub version: String,
    pub target: String,
    pub plugin_dir: PathBuf,
    pub binary_path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    pub was_already_present: bool,
    pub lifecycle_event_emitted: bool,
}

/// Error report shape. Serialized to JSON when `--json` is passed.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInstallErrorReport {
    pub ok: bool,
    pub kind: &'static str,
    pub error: String,
    /// Populated for `TargetNotFound` to surface available targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<Vec<String>>,
}

// ── Error kind mapping ─────────────────────────────────────────────

pub fn install_error_kind(e: &InstallError) -> &'static str {
    match e {
        InstallError::CoordsInvalid { .. } => "CoordsInvalid",
        InstallError::Http(_) => "Http",
        InstallError::Io(_) => "Io",
        InstallError::ReleaseShape { .. } => "ReleaseShape",
        InstallError::TargetNotFound { .. } => "TargetNotFound",
        InstallError::Sha256Invalid { .. } => "Sha256Invalid",
        InstallError::Sha256Mismatch { .. } => "Sha256Mismatch",
    }
}

pub fn extract_error_kind(e: &ExtractError) -> &'static str {
    match e {
        ExtractError::Io(_) => "ExtractIo",
        ExtractError::TarballTooLarge { .. } => "TarballTooLarge",
        ExtractError::TooManyEntries { .. } => "TooManyEntries",
        ExtractError::EntryTooLarge { .. } => "EntryTooLarge",
        ExtractError::ExtractedTooLarge { .. } => "ExtractedTooLarge",
        ExtractError::UnsafePath { .. } => "UnsafePath",
        ExtractError::DisallowedEntryType { .. } => "DisallowedEntryType",
        ExtractError::ManifestMismatch { .. } => "ManifestMismatch",
        ExtractError::ManifestInvalid { .. } => "ManifestInvalid",
        ExtractError::BinaryMissing { .. } => "BinaryMissing",
        ExtractError::JoinError(_) => "JoinError",
    }
}

fn install_error_available(e: &InstallError) -> Option<Vec<String>> {
    match e {
        InstallError::TargetNotFound { available, .. } => Some(available.clone()),
        _ => None,
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn resolve_target(cli_override: Option<String>) -> String {
    if let Some(t) = cli_override {
        return t;
    }
    if let Ok(t) = std::env::var("NEXO_INSTALL_TARGET") {
        if !t.is_empty() {
            return t;
        }
    }
    nexo_ext_installer::current_target_triple()
}

fn resolve_dest_root(
    cfg: &nexo_config::AppConfig,
    cli_override: Option<PathBuf>,
    json: bool,
) -> Result<PathBuf> {
    if let Some(dest) = cli_override {
        std::fs::create_dir_all(&dest)
            .with_context(|| format!("create --dest {}", dest.display()))?;
        return Ok(dest);
    }
    if let Some(first) = cfg.plugins.discovery.search_paths.first() {
        std::fs::create_dir_all(first).with_context(|| {
            format!("create plugins.discovery.search_paths[0] {}", first.display())
        })?;
        return Ok(first.clone());
    }
    let fallback = nexo_project_tracker::state::nexo_state_dir().join("plugins");
    std::fs::create_dir_all(&fallback)
        .with_context(|| format!("create fallback plugin root {}", fallback.display()))?;
    if !json {
        eprintln!(
            "! plugins.discovery.search_paths empty in config; defaulting to {}",
            fallback.display()
        );
    }
    Ok(fallback)
}

fn build_reqwest_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
    );
    let ua = format!("nexo-rs-plugin-install/{}", CRATE_VERSION);
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&ua) {
        headers.insert(reqwest::header::USER_AGENT, v);
    }
    if let Ok(token) = std::env::var("NEXO_GITHUB_TOKEN") {
        if !token.is_empty() {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)) {
                headers.insert(reqwest::header::AUTHORIZATION, v);
            }
        }
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn cache_tarball_path(state_dir: &Path, id: &str, version: &str, target: &str) -> PathBuf {
    state_dir
        .join("plugin-install-cache")
        .join(format!("{}-{}-{}.tar.gz", id, version, target))
}

fn truncate_sha(sha: &str) -> String {
    if sha.len() <= 12 {
        sha.to_string()
    } else {
        format!("{}…", &sha[..12])
    }
}

fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

// ── Broker lifecycle event (best-effort) ───────────────────────────

async fn emit_lifecycle_installed_event(
    cfg: &nexo_config::AppConfig,
    report_id: &str,
    version: &str,
    plugin_dir: &Path,
    was_already_present: bool,
) -> bool {
    let broker = &cfg.broker.broker;
    if broker.kind != nexo_config::types::broker::BrokerKind::Nats {
        return false;
    }
    let url = broker.url.clone();
    let payload = serde_json::json!({
        "plugin_id": report_id,
        "version": version,
        "plugin_dir": plugin_dir.display().to_string(),
        "source": "plugin.cli",
        "was_already_present": was_already_present,
    });
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "lifecycle event payload serialize failed");
            return false;
        }
    };
    let topic = format!("plugin.lifecycle.{}.installed", report_id);

    let connect = async move { async_nats::connect(&url).await };
    let client = match tokio::time::timeout(BROKER_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "lifecycle event broker connect failed");
            return false;
        }
        Err(_) => {
            tracing::warn!("lifecycle event broker connect timed out");
            return false;
        }
    };
    if let Err(e) = client.publish(topic, payload_bytes.into()).await {
        tracing::warn!(error = %e, "lifecycle event publish failed");
        return false;
    }
    let _ = client.flush().await;
    true
}

// ── Public orchestration ───────────────────────────────────────────

pub async fn run_plugin_install(
    config_dir: &Path,
    coords_str: String,
    dest_override: Option<PathBuf>,
    target_override: Option<String>,
    json: bool,
) -> Result<i32> {
    let cfg = nexo_config::AppConfig::load(config_dir)
        .with_context(|| format!("failed to load config from {}", config_dir.display()))?;

    let target = resolve_target(target_override);
    let dest_root = resolve_dest_root(&cfg, dest_override, json)?;
    let state_dir = nexo_project_tracker::state::nexo_state_dir();

    // Coords parse.
    let coords = match PluginCoords::parse(&coords_str) {
        Ok(c) => c,
        Err(e) => return Ok(emit_install_error_and_exit(&e, json)),
    };

    if !json {
        eprintln!(
            "→ Resolving {}/{}@{} (target: {})",
            coords.owner, coords.repo, coords.tag, target
        );
    }
    let client = build_reqwest_client();
    let resolved: ResolvedInstall =
        match resolve_release(&client, &coords, &target, DEFAULT_GITHUB_API_BASE).await {
            Ok(r) => r,
            Err(e) => return Ok(emit_install_error_and_exit(&e, json)),
        };

    let entry = &resolved.entry;
    let download = &entry.downloads[resolved.download_index];
    let id = entry.id.clone();
    let version = entry.version.clone();

    if !json {
        eprintln!(
            "✓ Found release v{} ({}, {}, sha256 {})",
            version,
            target,
            human_bytes(download.size_bytes),
            truncate_sha(&download.sha256)
        );
        eprintln!("→ Downloading");
    }

    let cache_path = cache_tarball_path(&state_dir, &id, &version.to_string(), &target);
    let installed = match download_and_verify(&client, &resolved, &cache_path).await {
        Ok(t) => t,
        Err(e) => return Ok(emit_install_error_and_exit(&e, json)),
    };

    if !json {
        eprintln!("✓ sha256 verified");
        eprintln!("→ Extracting to {}", dest_root.display());
    }

    let extracted = match extract_verified_tarball(ExtractInput {
        tarball_path: &installed.tarball_path,
        dest_root: &dest_root,
        expected: &installed.entry,
        limits: ExtractLimits::default(),
    })
    .await
    {
        Ok(p) => p,
        Err(e) => return Ok(emit_extract_error_and_exit(&e, json)),
    };

    // Cleanup cached tarball on success — extracted tree is the source of truth.
    let _ = std::fs::remove_file(&installed.tarball_path);

    let lifecycle_emitted = emit_lifecycle_installed_event(
        &cfg,
        &id,
        &version.to_string(),
        &extracted.plugin_dir,
        extracted.was_already_present,
    )
    .await;

    let report = PluginInstallReport {
        ok: true,
        id: id.clone(),
        version: version.to_string(),
        target,
        plugin_dir: extracted.plugin_dir.clone(),
        binary_path: extracted.binary_path.clone(),
        sha256: download.sha256.clone(),
        size_bytes: download.size_bytes,
        was_already_present: extracted.was_already_present,
        lifecycle_event_emitted: lifecycle_emitted,
    };

    if json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else if extracted.was_already_present {
        eprintln!(
            "✓ {}@{} already installed at {} — nothing to do",
            id,
            version,
            extracted.plugin_dir.display()
        );
    } else {
        eprintln!(
            "✓ Plugin installed at {}",
            extracted.plugin_dir.display()
        );
        if lifecycle_emitted {
            eprintln!("✓ Lifecycle event emitted (broker)");
        } else {
            eprintln!("! Lifecycle event not emitted (broker offline or non-NATS)");
        }
    }

    Ok(0)
}

fn emit_install_error_and_exit(err: &InstallError, json: bool) -> i32 {
    let kind = install_error_kind(err);
    let available = install_error_available(err);
    if json {
        let report = PluginInstallErrorReport {
            ok: false,
            kind,
            error: err.to_string(),
            available,
        };
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        eprintln!("✗ Install failed: {}", err);
        match err {
            InstallError::TargetNotFound { available, .. } => {
                eprintln!("  Available targets: {}", available.join(", "));
                eprintln!(
                    "  Hint: pass --target=<other-triple> or set NEXO_INSTALL_TARGET, or"
                );
                eprintln!("  ask the plugin author to publish a build for your target.");
            }
            InstallError::Http(msg) if msg.contains("rate limit") => {
                eprintln!(
                    "  Hint: set NEXO_GITHUB_TOKEN env to bypass anonymous rate limit."
                );
            }
            InstallError::Http(_) => {
                eprintln!("  Hint: verify <owner>/<repo>@<tag> exists on GitHub.");
            }
            _ => {}
        }
    }
    1
}

fn emit_extract_error_and_exit(err: &ExtractError, json: bool) -> i32 {
    let kind = extract_error_kind(err);
    if json {
        let report = PluginInstallErrorReport {
            ok: false,
            kind,
            error: err.to_string(),
            available: None,
        };
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        eprintln!("✗ Extract failed: {}", err);
    }
    1
}

// Used by main.rs to satisfy the Arc-once import; suppresses a
// dead-code warning when the broker emit path is not exercised.
#[allow(dead_code)]
const _ARC_KEEPALIVE: Option<Arc<()>> = None;

// ── Static help text ───────────────────────────────────────────────

pub fn print_plugin_help() {
    println!(
        "nexo plugin <subcommand>\n\
         \n\
         Subcommands:\n\
         \n\
         install <owner>/<repo>[@<tag>] [--dest <path>] [--target <triple>] [--json]\n\
             Install a community plugin from GitHub Releases. The CLI resolves the\n\
             release JSON via the GitHub API, downloads the matching\n\
             <id>-<version>-<target>.tar.gz asset, verifies its sha256 against the\n\
             companion .sha256 asset, and extracts under\n\
             plugins.discovery.search_paths[0] (or --dest, or the state-dir\n\
             fallback when both are unset). Tag defaults to `latest`.\n\
         \n\
         help\n\
             Show this help.\n\
         \n\
         Environment:\n\
           NEXO_GITHUB_TOKEN    Bearer token for GitHub API (private repos /\n\
                                higher rate limits).\n\
           NEXO_INSTALL_TARGET  Override the target-triple autodetect.\n"
    );
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_ext_installer::ExtractError;

    fn dummy_install_errors() -> Vec<(InstallError, &'static str)> {
        vec![
            (
                InstallError::CoordsInvalid {
                    got: "x".into(),
                    reason: "bad",
                },
                "CoordsInvalid",
            ),
            (InstallError::Http("net".into()), "Http"),
            (InstallError::Io("disk".into()), "Io"),
            (
                InstallError::ReleaseShape {
                    owner: "o".into(),
                    repo: "r".into(),
                    reason: "x".into(),
                },
                "ReleaseShape",
            ),
            (
                InstallError::TargetNotFound {
                    id: "slack".into(),
                    version: semver::Version::parse("0.1.0").unwrap(),
                    target: "aarch64".into(),
                    available: vec!["x86_64".into()],
                },
                "TargetNotFound",
            ),
            (
                InstallError::Sha256Invalid {
                    id: "slack".into(),
                    got: "x".into(),
                },
                "Sha256Invalid",
            ),
            (
                InstallError::Sha256Mismatch {
                    id: "slack".into(),
                    expected: "a".into(),
                    got: "b".into(),
                },
                "Sha256Mismatch",
            ),
        ]
    }

    fn dummy_extract_errors() -> Vec<(ExtractError, &'static str)> {
        use std::path::PathBuf;
        vec![
            (ExtractError::Io("x".into()), "ExtractIo"),
            (
                ExtractError::TarballTooLarge {
                    path: PathBuf::from("/x"),
                    actual: 1,
                    limit: 0,
                },
                "TarballTooLarge",
            ),
            (
                ExtractError::TooManyEntries { limit: 0 },
                "TooManyEntries",
            ),
            (
                ExtractError::EntryTooLarge {
                    path: "x".into(),
                    actual: 1,
                    limit: 0,
                },
                "EntryTooLarge",
            ),
            (
                ExtractError::ExtractedTooLarge { limit: 0 },
                "ExtractedTooLarge",
            ),
            (
                ExtractError::UnsafePath {
                    path: "x".into(),
                    reason: "y",
                },
                "UnsafePath",
            ),
            (
                ExtractError::DisallowedEntryType {
                    path: "x".into(),
                    kind: "symlink",
                },
                "DisallowedEntryType",
            ),
            (
                ExtractError::ManifestMismatch {
                    expected_id: "a".into(),
                    expected_version: semver::Version::parse("0.1.0").unwrap(),
                    got_id: "b".into(),
                    got_version: semver::Version::parse("0.1.0").unwrap(),
                },
                "ManifestMismatch",
            ),
            (
                ExtractError::ManifestInvalid {
                    path: PathBuf::from("/x"),
                    reason: "y".into(),
                },
                "ManifestInvalid",
            ),
            (
                ExtractError::BinaryMissing {
                    path: PathBuf::from("/x"),
                },
                "BinaryMissing",
            ),
            (ExtractError::JoinError("panic".into()), "JoinError"),
        ]
    }

    #[test]
    fn install_error_kind_maps_all_variants() {
        for (err, expected) in dummy_install_errors() {
            assert_eq!(install_error_kind(&err), expected);
        }
    }

    #[test]
    fn extract_error_kind_maps_all_variants() {
        for (err, expected) in dummy_extract_errors() {
            assert_eq!(extract_error_kind(&err), expected);
        }
    }

    #[test]
    fn install_error_available_only_set_for_target_not_found() {
        for (err, kind) in dummy_install_errors() {
            let av = install_error_available(&err);
            if kind == "TargetNotFound" {
                assert_eq!(av, Some(vec!["x86_64".to_string()]));
            } else {
                assert!(av.is_none(), "{kind} should not surface available");
            }
        }
    }

    #[test]
    fn report_serializes_to_expected_json_shape() {
        let r = PluginInstallReport {
            ok: true,
            id: "slack".into(),
            version: "0.2.0".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            plugin_dir: PathBuf::from("/p/slack-0.2.0"),
            binary_path: PathBuf::from("/p/slack-0.2.0/bin/slack"),
            sha256: "a".repeat(64),
            size_bytes: 1234,
            was_already_present: false,
            lifecycle_event_emitted: true,
        };
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["id"], "slack");
        assert_eq!(v["version"], "0.2.0");
        assert_eq!(v["target"], "x86_64-unknown-linux-gnu");
        assert!(v["plugin_dir"].as_str().unwrap().ends_with("slack-0.2.0"));
        assert!(v["binary_path"].as_str().unwrap().ends_with("bin/slack"));
        assert_eq!(v["size_bytes"], 1234);
        assert_eq!(v["was_already_present"], false);
        assert_eq!(v["lifecycle_event_emitted"], true);
    }

    #[test]
    fn error_report_serializes_with_kind_and_available() {
        let r = PluginInstallErrorReport {
            ok: false,
            kind: "TargetNotFound",
            error: "no tarball".into(),
            available: Some(vec!["x86_64".into()]),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["kind"], "TargetNotFound");
        assert_eq!(v["error"], "no tarball");
        assert_eq!(v["available"][0], "x86_64");
    }

    #[test]
    fn error_report_skips_available_when_none() {
        let r = PluginInstallErrorReport {
            ok: false,
            kind: "Http",
            error: "404".into(),
            available: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("available").is_none(), "available must be omitted");
    }

    #[test]
    fn human_bytes_formats_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn truncate_sha_keeps_short_strings() {
        assert_eq!(truncate_sha("abc"), "abc");
        let truncated = truncate_sha(&"a".repeat(64));
        assert_eq!(truncated.chars().count(), 13); // 12 hex chars + ellipsis
        assert!(truncated.ends_with('…'));
    }
}
