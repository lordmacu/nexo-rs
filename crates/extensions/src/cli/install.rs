//! Phase 11.7 follow-up — `agent ext install <path>` and `agent ext uninstall <id>`.
//!
//! Install copies (or symlinks) a local extension directory into the first
//! configured `search_paths[0]`. Everything is validated before any bytes
//! hit disk: manifest parses, id is not reserved, no collision with another
//! discovered extension, target does not already exist (unless `--update`).
//!
//! Writes always land via a sibling tmp dir + `rename` so in-progress
//! copies are never visible to the extension watcher. `--link` requires an
//! absolute source — a relative symlink is a footgun if cwd changes.
//! Uninstall is script-friendly: no interactive prompt, `--yes` required.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::discovery::ExtensionDiscovery;
use crate::manifest::{is_reserved_id, ExtensionManifest, MANIFEST_FILENAME};

use super::yaml_edit::{load_or_default, write_atomic};
use super::{extensions_yaml_path, CliContext, CliError};

// ── Public types ──────────────────────────────────────────────────────────────

pub struct InstallOptions {
    pub source: PathBuf,
    pub update: bool,
    pub enable: bool,
    pub dry_run: bool,
    pub link: bool,
    pub json: bool,
}

pub struct UninstallOptions {
    pub id: String,
    pub yes: bool,
    pub json: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InstallMode {
    Copy,
    Link,
    Update,
    #[serde(rename = "dry-run")]
    DryRun,
}

#[derive(Debug, Serialize)]
pub struct InstallOutcome {
    pub id: String,
    pub version: String,
    pub target: PathBuf,
    pub mode: InstallMode,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct UninstallOutcome {
    pub id: String,
    pub removed: PathBuf,
    pub was_symlink: bool,
}

// ── run_install ───────────────────────────────────────────────────────────────

pub fn run_install(ctx: CliContext<'_>, opts: InstallOptions) -> Result<(), CliError> {
    if opts.update && opts.link {
        return Err(CliError::InvalidSource(
            "--update and --link are mutually exclusive".into(),
        ));
    }

    let (src_dir, manifest) = resolve_manifest_and_source(&opts.source)?;
    let id = manifest.id().to_string();
    let version = manifest.version().to_string();
    validate_id_not_reserved(&id)?;

    let target = compute_target(&ctx.extensions, &id)?;

    detect_collision(&ctx.extensions, &id, &target)?;

    let target_exists = target.symlink_metadata().is_ok();
    if opts.update {
        if !target_exists {
            return Err(CliError::UpdateTargetMissing(id));
        }
    } else if target_exists {
        return Err(CliError::AlreadyExists(target));
    }

    if opts.dry_run {
        let outcome = InstallOutcome {
            id,
            version,
            target,
            mode: InstallMode::DryRun,
            enabled: false,
        };
        return emit_install_outcome(ctx.out, &outcome, opts.json);
    }

    let mode = if opts.link {
        install_link(&src_dir, &target)?;
        InstallMode::Link
    } else if opts.update {
        install_update(&src_dir, &target)?;
        InstallMode::Update
    } else {
        install_copy(&src_dir, &target)?;
        InstallMode::Copy
    };

    let mut enabled = false;
    if opts.enable {
        match enable_id(&ctx.config_dir, &id) {
            Ok(changed) => enabled = changed || !id_is_disabled(&ctx.config_dir, &id),
            Err(e) => {
                writeln!(ctx.err, "warning: install succeeded but enable failed: {e}").ok();
            }
        }
    }

    let outcome = InstallOutcome {
        id,
        version,
        target,
        mode,
        enabled,
    };
    emit_install_outcome(ctx.out, &outcome, opts.json)
}

// ── run_uninstall ─────────────────────────────────────────────────────────────

pub fn run_uninstall(ctx: CliContext<'_>, opts: UninstallOptions) -> Result<(), CliError> {
    if !opts.yes {
        return Err(CliError::MissingConfirmation);
    }

    // Prefer discovery (finds real directory installs). Falls back to
    // symlink lookup under search_paths[*]/<id> because discovery does
    // not follow symlinks — an extension installed via `--link` would
    // otherwise be undetectable here.
    let path = {
        let discovery = build_discovery(&ctx.extensions);
        let report = discovery.discover();
        if let Some(c) = report
            .candidates
            .into_iter()
            .find(|c| c.manifest.id() == opts.id)
        {
            c.root_dir
        } else {
            find_symlinked_install(&ctx.extensions, &opts.id)
                .ok_or_else(|| CliError::NotFound(opts.id.clone()))?
        }
    };
    let meta = fs::symlink_metadata(&path)
        .map_err(|e| CliError::CopyFailed(format!("stat {}: {e}", path.display())))?;
    let was_symlink = meta.file_type().is_symlink();

    if was_symlink {
        fs::remove_file(&path).map_err(|e| CliError::CopyFailed(format!("remove_file: {e}")))?;
    } else {
        fs::remove_dir_all(&path)
            .map_err(|e| CliError::CopyFailed(format!("remove_dir_all: {e}")))?;
    }

    let yaml_path = extensions_yaml_path(&ctx.config_dir);
    if yaml_path.exists() {
        let mut cfg = load_or_default(&yaml_path)?;
        let before = cfg.disabled.len();
        cfg.disabled.retain(|x| x != &opts.id);
        if cfg.disabled.len() != before {
            write_atomic(&yaml_path, &cfg)?;
        }
    }

    let outcome = UninstallOutcome {
        id: opts.id,
        removed: path,
        was_symlink,
    };
    if opts.json {
        serde_json::to_writer_pretty(&mut *ctx.out, &outcome)
            .map_err(|e| CliError::CopyFailed(format!("json: {e}")))?;
        writeln!(ctx.out)?;
    } else {
        writeln!(
            ctx.out,
            "uninstalled: {} ({})",
            outcome.id,
            outcome.removed.display()
        )?;
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn resolve_manifest_and_source(src: &Path) -> Result<(PathBuf, ExtensionManifest), CliError> {
    let meta = fs::metadata(src)
        .map_err(|e| CliError::InvalidSource(format!("{}: {e}", src.display())))?;
    let (src_dir, manifest_path) = if meta.is_dir() {
        (src.to_path_buf(), src.join(MANIFEST_FILENAME))
    } else if meta.is_file() && src.file_name().and_then(|n| n.to_str()) == Some(MANIFEST_FILENAME)
    {
        let parent = src
            .parent()
            .ok_or_else(|| CliError::InvalidSource("plugin.toml has no parent dir".into()))?
            .to_path_buf();
        (parent, src.to_path_buf())
    } else {
        return Err(CliError::InvalidSource(format!(
            "{}: must be a directory or a plugin.toml path",
            src.display()
        )));
    };

    let manifest = ExtensionManifest::from_path(&manifest_path)
        .map_err(|e| CliError::InvalidManifest(format!("{}: {e}", manifest_path.display())))?;
    Ok((src_dir, manifest))
}

fn validate_id_not_reserved(id: &str) -> Result<(), CliError> {
    if is_reserved_id(id) {
        return Err(CliError::InvalidId(id.into(), "reserved native id".into()));
    }
    Ok(())
}

fn compute_target(cfg: &agent_config::ExtensionsConfig, id: &str) -> Result<PathBuf, CliError> {
    let first = cfg
        .search_paths
        .first()
        .ok_or_else(|| CliError::InvalidSource("no search_paths configured".into()))?;
    let base = PathBuf::from(first);
    if !base.exists() {
        fs::create_dir_all(&base)
            .map_err(|e| CliError::CopyFailed(format!("mkdir {}: {e}", base.display())))?;
    }
    Ok(base.join(id))
}

fn build_discovery(cfg: &agent_config::ExtensionsConfig) -> ExtensionDiscovery {
    let search_paths: Vec<PathBuf> = cfg.search_paths.iter().map(PathBuf::from).collect();
    ExtensionDiscovery::new(
        search_paths,
        cfg.ignore_dirs.clone(),
        Vec::new(),
        cfg.allowlist.clone(),
        cfg.max_depth,
    )
    .with_follow_links(cfg.follow_links)
}

fn detect_collision(
    cfg: &agent_config::ExtensionsConfig,
    id: &str,
    target: &Path,
) -> Result<(), CliError> {
    let discovery = build_discovery(cfg);
    let report = discovery.discover();
    let target_canon = canonicalize_lossy(target);
    for c in report.candidates {
        if c.manifest.id() != id {
            continue;
        }
        let other_canon = canonicalize_lossy(&c.root_dir);
        if other_canon != target_canon {
            return Err(CliError::IdCollision {
                id: id.into(),
                found_at: target_canon,
                other_at: other_canon,
            });
        }
    }
    Ok(())
}

fn find_symlinked_install(cfg: &agent_config::ExtensionsConfig, id: &str) -> Option<PathBuf> {
    for base in &cfg.search_paths {
        let p = PathBuf::from(base).join(id);
        if let Ok(meta) = fs::symlink_metadata(&p) {
            if meta.file_type().is_symlink() {
                return Some(p);
            }
        }
    }
    None
}

fn canonicalize_lossy(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn install_copy(src: &Path, target: &Path) -> Result<(), CliError> {
    let tmp = sibling_tmp(target);
    cleanup_path_if_exists(&tmp)?;
    copy_dir_all(src, &tmp).map_err(|e| CliError::CopyFailed(format!("copy to tmp: {e}")))?;
    fs::rename(&tmp, target).map_err(|e| {
        let _ = cleanup_path_if_exists(&tmp);
        map_rename_err(e)
    })?;
    Ok(())
}

fn install_link(src: &Path, target: &Path) -> Result<(), CliError> {
    if !src.is_absolute() {
        return Err(CliError::LinkRequiresAbsolute);
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, target)
            .map_err(|e| CliError::CopyFailed(format!("symlink: {e}")))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(CliError::CopyFailed(
            "--link is only supported on Unix".into(),
        ))
    }
}

fn install_update(src: &Path, target: &Path) -> Result<(), CliError> {
    let tmp = sibling_tmp(target);
    cleanup_path_if_exists(&tmp)?;
    copy_dir_all(src, &tmp).map_err(|e| CliError::CopyFailed(format!("copy to tmp: {e}")))?;

    let old_side = with_suffix(target, ".old-");
    cleanup_path_if_exists(&old_side)?;
    fs::rename(target, &old_side).map_err(|e| {
        let _ = cleanup_path_if_exists(&tmp);
        map_rename_err(e)
    })?;
    if let Err(e) = fs::rename(&tmp, target) {
        let _ = fs::rename(&old_side, target);
        let _ = cleanup_path_if_exists(&tmp);
        return Err(map_rename_err(e));
    }
    let _ = cleanup_path_if_exists(&old_side);
    Ok(())
}

fn map_rename_err(e: std::io::Error) -> CliError {
    if e.raw_os_error() == Some(18) {
        CliError::CopyFailed(
            "cross-device rename not supported; source and target must be on same filesystem"
                .into(),
        )
    } else {
        CliError::CopyFailed(format!("rename: {e}"))
    }
}

fn sibling_tmp(target: &Path) -> PathBuf {
    let pid = std::process::id();
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("ext");
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{name}.tmp-{pid}"))
}

fn with_suffix(target: &Path, suffix: &str) -> PathBuf {
    let pid = std::process::id();
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("ext");
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{name}{suffix}{pid}"))
}

fn cleanup_path_if_exists(p: &Path) -> Result<(), CliError> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => fs::remove_file(p)
            .map_err(|e| CliError::CopyFailed(format!("cleanup link {}: {e}", p.display()))),
        Ok(m) if m.is_dir() => fs::remove_dir_all(p)
            .map_err(|e| CliError::CopyFailed(format!("cleanup dir {}: {e}", p.display()))),
        Ok(_) => fs::remove_file(p)
            .map_err(|e| CliError::CopyFailed(format!("cleanup file {}: {e}", p.display()))),
        Err(_) => Ok(()),
    }
}

/// Recursive copy, preserving Unix mode bits. Symlinks inside the source
/// are dereferenced (consistent with `cp -L`).
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let src_mode = fs::metadata(src)?.permissions().mode();
        fs::set_permissions(dst, fs::Permissions::from_mode(src_mode))?;
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&src_path)?.permissions().mode();
                fs::set_permissions(&dst_path, fs::Permissions::from_mode(mode))?;
            }
        } else if file_type.is_symlink() {
            let target = fs::read_link(&src_path)?;
            let abs = if target.is_absolute() {
                target
            } else {
                src_path.parent().unwrap_or(Path::new(".")).join(target)
            };
            let meta = fs::metadata(&abs)?;
            if meta.is_dir() {
                copy_dir_all(&abs, &dst_path)?;
            } else {
                fs::copy(&abs, &dst_path)?;
            }
        }
    }
    Ok(())
}

fn enable_id(config_dir: &Path, id: &str) -> Result<bool, CliError> {
    let yaml_path = extensions_yaml_path(config_dir);
    let mut cfg = load_or_default(&yaml_path)?;
    let before = cfg.disabled.len();
    cfg.disabled.retain(|x| x != id);
    if cfg.disabled.len() == before {
        return Ok(false);
    }
    cfg.disabled.sort();
    cfg.disabled.dedup();
    write_atomic(&yaml_path, &cfg)?;
    Ok(true)
}

fn id_is_disabled(config_dir: &Path, id: &str) -> bool {
    let yaml_path = extensions_yaml_path(config_dir);
    match load_or_default(&yaml_path) {
        Ok(cfg) => cfg.disabled.iter().any(|x| x == id),
        Err(_) => false,
    }
}

fn emit_install_outcome(
    out: &mut dyn Write,
    outcome: &InstallOutcome,
    json: bool,
) -> Result<(), CliError> {
    if json {
        serde_json::to_writer_pretty(&mut *out, outcome)
            .map_err(|e| CliError::CopyFailed(format!("json: {e}")))?;
        writeln!(out)?;
    } else {
        let verb = match outcome.mode {
            InstallMode::Copy => "installed",
            InstallMode::Link => "linked",
            InstallMode::Update => "updated",
            InstallMode::DryRun => "would install",
        };
        writeln!(
            out,
            "{verb}: {} {} → {}",
            outcome.id,
            outcome.version,
            outcome.target.display()
        )?;
        if outcome.enabled {
            writeln!(out, "enabled: {}", outcome.id)?;
        }
    }
    Ok(())
}

// ── Unit tests (fs-touching tests live in tests/install_test.rs) ──────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_fixture(dir: &Path, id: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("plugin.toml"),
            format!(
                r#"
[plugin]
id = "{id}"
version = "0.1.0"
name = "Fixture"

[capabilities]
tools = ["echo"]

[transport]
kind = "stdio"
command = "/bin/true"
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn resolves_dir_source_ok() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("ext");
        write_fixture(&dir, "weather");
        let (src, m) = resolve_manifest_and_source(&dir).unwrap();
        assert_eq!(src, dir);
        assert_eq!(m.id(), "weather");
    }

    #[test]
    fn resolves_plugin_toml_path_ok() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("ext");
        write_fixture(&dir, "weather");
        let (src, m) = resolve_manifest_and_source(&dir.join("plugin.toml")).unwrap();
        assert_eq!(src, dir);
        assert_eq!(m.id(), "weather");
    }

    #[test]
    fn rejects_nonexistent_source() {
        let err = resolve_manifest_and_source(Path::new("/no/such/path-xyz-123")).unwrap_err();
        assert!(matches!(err, CliError::InvalidSource(_)));
    }

    #[test]
    fn computes_target_under_first_search_path() {
        let tmp = TempDir::new().unwrap();
        let cfg = agent_config::ExtensionsConfig {
            search_paths: vec![tmp.path().join("exts").to_string_lossy().to_string()],
            ..Default::default()
        };
        let t = compute_target(&cfg, "weather").unwrap();
        assert_eq!(t, tmp.path().join("exts").join("weather"));
        assert!(tmp.path().join("exts").exists());
    }

    #[test]
    fn rejects_empty_search_paths() {
        let cfg = agent_config::ExtensionsConfig {
            search_paths: vec![],
            ..Default::default()
        };
        let err = compute_target(&cfg, "weather").unwrap_err();
        assert!(matches!(err, CliError::InvalidSource(_)));
    }

    #[test]
    fn copy_dir_preserves_executable_bit() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        let bin = src.join("run.sh");
        fs::write(&bin, b"#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        copy_dir_all(&src, &dst).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dst.join("run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn atomic_swap_happy_path() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        let target = base.join("weather");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("old.txt"), b"old").unwrap();

        let src = base.join("src");
        write_fixture(&src, "weather");
        fs::write(src.join("new.txt"), b"new").unwrap();

        install_update(&src, &target).unwrap();
        assert!(target.join("new.txt").exists());
        assert!(!target.join("old.txt").exists());
        for entry in fs::read_dir(base).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(
                !name.contains(".tmp-") && !name.contains(".old-"),
                "leftover: {name}"
            );
        }
    }
}
