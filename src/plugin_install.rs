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
    discover_cosign_binary, download_and_verify, extract_verified_tarball, resolve_release,
    verify_plugin_signature, AuthorPolicy, ExtractError, ExtractInput, ExtractLimits, InstallError,
    PluginCoords, ResolvedInstall, TrustMode, TrustedKeysConfig, VerifyError, VerifyInput,
    DEFAULT_GITHUB_API_BASE,
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
    /// `true` when cosign verification ran and succeeded.
    pub signature_verified: bool,
    /// SAN extracted from cosign's stderr (e.g. workflow URL).
    /// Omitted when verification was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_identity: Option<String>,
    /// OIDC issuer the cert was minted by. Omitted when
    /// verification was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_issuer: Option<String>,
    /// Effective trust mode used for this install
    /// (`"ignore" | "warn" | "require"`).
    pub trust_mode: &'static str,
    /// Repo owner that matched a `[[authors]]` entry in
    /// `trusted_keys.toml`. `None` means no entry matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_policy_matched: Option<String>,
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

pub fn verify_error_kind(e: &VerifyError) -> &'static str {
    match e {
        VerifyError::CosignNotFound { .. } => "CosignNotFound",
        VerifyError::CosignFailed { .. } => "CosignFailed",
        VerifyError::Io(_) => "VerifyIo",
        VerifyError::PolicyRequiresSig { .. } => "PolicyRequiresSig",
        VerifyError::AssetIncomplete { .. } => "AssetIncomplete",
        VerifyError::TrustedKeysParse { .. } => "TrustedKeysParse",
        VerifyError::IdentityRegexpInvalid { .. } => "IdentityRegexpInvalid",
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

async fn download_to_cache(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<(), VerifyError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VerifyError::Io(format!("mkdir cache parent: {e}")))?;
    }
    let bytes = client
        .get(url)
        .send()
        .await
        .map_err(|e| VerifyError::Io(format!("fetch {url}: {e}")))?
        .error_for_status()
        .map_err(|e| VerifyError::Io(format!("http error for {url}: {e}")))?
        .bytes()
        .await
        .map_err(|e| VerifyError::Io(format!("read body of {url}: {e}")))?;
    std::fs::write(dest, &bytes).map_err(|e| VerifyError::Io(format!("write {}: {e}", dest.display())))?;
    Ok(())
}

fn cache_sidecar_path(tarball_path: &Path, suffix: &str) -> PathBuf {
    let mut name = tarball_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    tarball_path.with_file_name(name)
}

/// Resolve the effective trust mode honoring CLI overrides
/// (`--require-signature` / `--skip-signature-verify`) over
/// per-author entry over global default.
fn resolve_effective_mode(
    cfg: &TrustedKeysConfig,
    owner: &str,
    cli_require: bool,
    cli_skip: bool,
) -> (TrustMode, Option<AuthorPolicy>) {
    if cli_skip {
        return (TrustMode::Ignore, None);
    }
    let (mode, hit) = cfg.resolve(owner);
    let policy = hit.cloned();
    if cli_require {
        return (TrustMode::Require, policy);
    }
    (mode, policy)
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
    require_signature: bool,
    skip_signature_verify: bool,
) -> Result<i32> {
    if require_signature && skip_signature_verify {
        let report = PluginInstallErrorReport {
            ok: false,
            kind: "FlagsConflict",
            error:
                "--require-signature and --skip-signature-verify are mutually exclusive"
                    .to_string(),
            available: None,
        };
        if json {
            println!("{}", serde_json::to_string(&report).unwrap_or_default());
        } else {
            eprintln!("✗ {}", report.error);
        }
        return Ok(1);
    }

    let cfg = nexo_config::AppConfig::load(config_dir)
        .with_context(|| format!("failed to load config from {}", config_dir.display()))?;

    let trusted = match TrustedKeysConfig::load(config_dir) {
        Ok(t) => t,
        Err(e) => return Ok(emit_verify_error_and_exit(&e, json)),
    };

    let target = resolve_target(target_override);
    let dest_root = resolve_dest_root(&cfg, dest_override, json)?;
    let state_dir = nexo_project_tracker::state::nexo_state_dir();

    // Coords parse.
    let coords = match PluginCoords::parse(&coords_str) {
        Ok(c) => c,
        Err(e) => return Ok(emit_install_error_and_exit(&e, json)),
    };

    let (effective_mode, matched_policy) =
        resolve_effective_mode(&trusted, &coords.owner, require_signature, skip_signature_verify);

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
    }

    // ── Phase 31.3 — cosign signature verification hook ───────
    let mut signature_verified = false;
    let mut signature_identity: Option<String> = None;
    let mut signature_issuer: Option<String> = None;
    let sig_cache_paths: Option<(PathBuf, PathBuf, Option<PathBuf>)> = match (
        effective_mode,
        installed.entry.signing.as_ref(),
        matched_policy.as_ref(),
    ) {
        (TrustMode::Ignore, _, _) => None,
        (mode, None, _) => {
            if mode == TrustMode::Require {
                let _ = std::fs::remove_file(&installed.tarball_path);
                return Ok(emit_verify_error_and_exit(
                    &VerifyError::PolicyRequiresSig {
                        owner: coords.owner.clone(),
                    },
                    json,
                ));
            }
            if !json {
                eprintln!(
                    "! No signature in release; trust mode is `{}` — proceeding unverified.",
                    mode.as_str()
                );
            }
            None
        }
        (mode, Some(_), None) => {
            if mode == TrustMode::Require {
                let _ = std::fs::remove_file(&installed.tarball_path);
                return Ok(emit_verify_error_and_exit(
                    &VerifyError::PolicyRequiresSig {
                        owner: coords.owner.clone(),
                    },
                    json,
                ));
            }
            if !json {
                eprintln!(
                    "! No `[[authors]]` entry for `{}` in trusted_keys.toml; proceeding unverified.",
                    coords.owner
                );
            }
            None
        }
        (_mode, Some(signing), Some(policy)) => {
            let sig_path = cache_sidecar_path(&installed.tarball_path, ".sig");
            let cert_path = cache_sidecar_path(&installed.tarball_path, ".cert");
            let bundle_path = cache_sidecar_path(&installed.tarball_path, ".bundle");
            if let Err(e) =
                download_to_cache(&client, &signing.cosign_signature_url, &sig_path).await
            {
                let _ = std::fs::remove_file(&installed.tarball_path);
                return Ok(emit_verify_error_and_exit(&e, json));
            }
            if let Err(e) =
                download_to_cache(&client, &signing.cosign_certificate_url, &cert_path).await
            {
                let _ = std::fs::remove_file(&installed.tarball_path);
                let _ = std::fs::remove_file(&sig_path);
                return Ok(emit_verify_error_and_exit(&e, json));
            }
            // bundle is optional — best-effort fetch if the resolver
            // surfaced it (currently ExtSigning carries only sig+cert,
            // bundle URL inference deferred). Skip for v1.
            let bundle_used: Option<PathBuf> = if bundle_path.exists() {
                Some(bundle_path.clone())
            } else {
                None
            };

            let cosign_bin = match discover_cosign_binary(trusted.cosign_binary.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    let _ = std::fs::remove_file(&installed.tarball_path);
                    let _ = std::fs::remove_file(&sig_path);
                    let _ = std::fs::remove_file(&cert_path);
                    return Ok(emit_verify_error_and_exit(&e, json));
                }
            };

            if !json {
                eprintln!("→ Verifying signature against trusted_keys.toml");
            }
            let verified = match verify_plugin_signature(VerifyInput {
                cosign_bin: &cosign_bin,
                tarball_path: &installed.tarball_path,
                sig_path: &sig_path,
                cert_path: &cert_path,
                bundle_path: bundle_used.as_deref(),
                policy,
            })
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    let _ = std::fs::remove_file(&installed.tarball_path);
                    let _ = std::fs::remove_file(&sig_path);
                    let _ = std::fs::remove_file(&cert_path);
                    return Ok(emit_verify_error_and_exit(&e, json));
                }
            };

            signature_verified = true;
            signature_identity = Some(verified.identity);
            signature_issuer = Some(verified.issuer);
            Some((sig_path, cert_path, bundle_used))
        }
    };

    if !json {
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

    // Cleanup cached tarball + signing material on success — extracted tree is source of truth.
    let _ = std::fs::remove_file(&installed.tarball_path);
    if let Some((sig, cert, bundle)) = sig_cache_paths {
        let _ = std::fs::remove_file(sig);
        let _ = std::fs::remove_file(cert);
        if let Some(b) = bundle {
            let _ = std::fs::remove_file(b);
        }
    }

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
        signature_verified,
        signature_identity: signature_identity.clone(),
        signature_issuer,
        trust_mode: effective_mode.as_str(),
        trust_policy_matched: matched_policy.as_ref().map(|p| p.owner.clone()),
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
        if signature_verified {
            if let Some(identity) = &signature_identity {
                eprintln!("✓ Signature verified (identity: {})", identity);
            } else {
                eprintln!("✓ Signature verified");
            }
        }
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

fn emit_verify_error_and_exit(err: &VerifyError, json: bool) -> i32 {
    let kind = verify_error_kind(err);
    if json {
        let report = PluginInstallErrorReport {
            ok: false,
            kind,
            error: err.to_string(),
            available: None,
        };
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        eprintln!("✗ Signature verification failed: {}", err);
        match err {
            VerifyError::CosignNotFound { .. } => {
                eprintln!(
                    "  Hint: install cosign (brew install cosign / apt install cosign / dnf install cosign),"
                );
                eprintln!(
                    "  or set --skip-signature-verify if you accept unverified bytes for this install."
                );
            }
            VerifyError::PolicyRequiresSig { .. } => {
                eprintln!(
                    "  Hint: ask the plugin author to publish cosign signing material, or relax"
                );
                eprintln!(
                    "  trusted_keys.toml `mode` for this owner to `warn`."
                );
            }
            VerifyError::CosignFailed { .. } => {
                eprintln!(
                    "  Hint: the certificate's identity did not match `identity_regexp` in"
                );
                eprintln!(
                    "  trusted_keys.toml. Check the publisher workflow URL or update the regex."
                );
            }
            _ => {}
        }
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
         install <owner>/<repo>[@<tag>] [--dest <path>] [--target <triple>]\n\
                 [--require-signature|--skip-signature-verify] [--json]\n\
             Install a community plugin from GitHub Releases. The CLI resolves the\n\
             release JSON via the GitHub API, downloads the matching\n\
             <id>-<version>-<target>.tar.gz asset, verifies its sha256 against the\n\
             companion .sha256 asset, runs cosign verification per\n\
             config/extensions/trusted_keys.toml, and extracts under\n\
             plugins.discovery.search_paths[0] (or --dest, or the state-dir\n\
             fallback when both are unset). Tag defaults to `latest`.\n\
         \n\
             Trust mode flags (mutually exclusive):\n\
               --require-signature       force `Require` mode for this call.\n\
               --skip-signature-verify   force `Ignore` mode for this call.\n\
             Without either flag the trusted_keys.toml default applies.\n\
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

    fn make_report() -> PluginInstallReport {
        PluginInstallReport {
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
            signature_verified: false,
            signature_identity: None,
            signature_issuer: None,
            trust_mode: "warn",
            trust_policy_matched: None,
        }
    }

    #[test]
    fn report_serializes_to_expected_json_shape() {
        let r = make_report();
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
        assert_eq!(v["signature_verified"], false);
        assert_eq!(v["trust_mode"], "warn");
    }

    #[test]
    fn report_includes_signature_fields_when_verified() {
        let mut r = make_report();
        r.signature_verified = true;
        r.signature_identity =
            Some("https://github.com/foo/bar/.github/workflows/release.yml".into());
        r.signature_issuer = Some("https://token.actions.githubusercontent.com".into());
        r.trust_mode = "require";
        r.trust_policy_matched = Some("foo".into());
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["signature_verified"], true);
        assert!(v["signature_identity"]
            .as_str()
            .unwrap()
            .contains("github.com/foo/bar"));
        assert_eq!(
            v["signature_issuer"],
            "https://token.actions.githubusercontent.com"
        );
        assert_eq!(v["trust_mode"], "require");
        assert_eq!(v["trust_policy_matched"], "foo");
    }

    #[test]
    fn report_omits_signature_identity_when_unverified() {
        let r = make_report();
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("signature_identity").is_none());
        assert!(v.get("signature_issuer").is_none());
        assert!(v.get("trust_policy_matched").is_none());
    }

    #[test]
    fn verify_error_kind_maps_all_variants() {
        use std::path::PathBuf;
        let cases: Vec<(VerifyError, &'static str)> = vec![
            (
                VerifyError::CosignNotFound {
                    searched: vec![PathBuf::from("/x")],
                },
                "CosignNotFound",
            ),
            (
                VerifyError::CosignFailed {
                    stderr: "x".into(),
                },
                "CosignFailed",
            ),
            (VerifyError::Io("x".into()), "VerifyIo"),
            (
                VerifyError::PolicyRequiresSig {
                    owner: "alice".into(),
                },
                "PolicyRequiresSig",
            ),
            (
                VerifyError::AssetIncomplete {
                    present: ".sig",
                    missing: ".cert",
                },
                "AssetIncomplete",
            ),
            (
                VerifyError::TrustedKeysParse {
                    path: PathBuf::from("/x"),
                    reason: "y".into(),
                },
                "TrustedKeysParse",
            ),
            (
                VerifyError::IdentityRegexpInvalid {
                    got: "x".into(),
                    reason: "y".into(),
                },
                "IdentityRegexpInvalid",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(verify_error_kind(&err), expected, "kind for {err:?}");
        }
    }

    #[test]
    fn error_report_serializes_policy_requires_sig() {
        let r = PluginInstallErrorReport {
            ok: false,
            kind: "PolicyRequiresSig",
            error: "trust policy requires signature for `alice`".into(),
            available: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["kind"], "PolicyRequiresSig");
        assert!(v["error"].as_str().unwrap().contains("alice"));
        assert!(v.get("available").is_none());
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
