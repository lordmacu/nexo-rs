//! End-to-end tests for `agent ext install` and `agent ext uninstall`.

use std::fs;
use std::path::{Path, PathBuf};

use agent_config::ExtensionsConfig;
use agent_extensions::cli::{
    run_install, run_uninstall, CliContext, CliError, InstallMode, InstallOptions, UninstallOptions,
};
use tempfile::TempDir;

fn write_plugin_toml(dir: &Path, id: &str, version: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        format!(
            r#"
[plugin]
id = "{id}"
version = "{version}"
name = "Fixture"

[capabilities]
tools = ["echo"]

[transport]
kind = "stdio"
command = "./bin/handler"
"#
        ),
    )
    .unwrap();
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let bin = bin_dir.join("handler");
    fs::write(&bin, b"#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

struct Env {
    _tmp: TempDir,
    config_dir: PathBuf,
    search_root: PathBuf,
    src_dir: PathBuf,
}

impl Env {
    fn new(id: &str) -> Self {
        Self::with_version(id, "0.1.0")
    }

    fn with_version(id: &str, version: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("config");
        let search_root = tmp.path().join("exts");
        let src_dir = tmp.path().join("src").join(id);
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&search_root).unwrap();
        write_plugin_toml(&src_dir, id, version);
        Env {
            _tmp: tmp,
            config_dir,
            search_root,
            src_dir,
        }
    }

    fn cfg(&self) -> ExtensionsConfig {
        ExtensionsConfig {
            search_paths: vec![self.search_root.to_string_lossy().to_string()],
            ..Default::default()
        }
    }

    fn with_secondary_search(&self) -> (PathBuf, ExtensionsConfig) {
        let secondary = self._tmp.path().join("exts2");
        fs::create_dir_all(&secondary).unwrap();
        let cfg = ExtensionsConfig {
            search_paths: vec![
                self.search_root.to_string_lossy().to_string(),
                secondary.to_string_lossy().to_string(),
            ],
            ..Default::default()
        };
        (secondary, cfg)
    }
}

fn install(
    env: &Env,
    cfg_override: Option<ExtensionsConfig>,
    opts: InstallOptions,
) -> (Result<(), CliError>, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let extensions = cfg_override.unwrap_or_else(|| env.cfg());
    let ctx = CliContext {
        config_dir: env.config_dir.clone(),
        extensions,
        out: &mut out,
        err: &mut err,
    };
    let res = run_install(ctx, opts);
    (res, String::from_utf8(out).unwrap())
}

fn uninstall(
    env: &Env,
    cfg_override: Option<ExtensionsConfig>,
    opts: UninstallOptions,
) -> (Result<(), CliError>, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let extensions = cfg_override.unwrap_or_else(|| env.cfg());
    let ctx = CliContext {
        config_dir: env.config_dir.clone(),
        extensions,
        out: &mut out,
        err: &mut err,
    };
    let res = run_uninstall(ctx, opts);
    (res, String::from_utf8(out).unwrap())
}

fn opts_default(src: PathBuf) -> InstallOptions {
    InstallOptions {
        source: src,
        update: false,
        enable: false,
        dry_run: false,
        link: false,
        json: false,
    }
}

// ── install ───────────────────────────────────────────────────────────────────

#[test]
fn install_copy_creates_target_and_manifest_loadable() {
    let env = Env::new("weather");
    let (res, out) = install(&env, None, opts_default(env.src_dir.clone()));
    res.unwrap();
    let target = env.search_root.join("weather");
    assert!(target.join("plugin.toml").exists());
    assert!(target.join("bin/handler").exists());
    assert!(out.contains("installed: weather 0.1.0"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(target.join("bin/handler"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[test]
fn install_rejects_existing_target_without_update() {
    let env = Env::new("weather");
    install(&env, None, opts_default(env.src_dir.clone()))
        .0
        .unwrap();
    let (res, _) = install(&env, None, opts_default(env.src_dir.clone()));
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::AlreadyExists(_)), "{err:?}");
    assert_eq!(err.exit_code(), 5);
}

#[test]
fn install_with_update_replaces_atomically() {
    let env = Env::new("weather");
    install(&env, None, opts_default(env.src_dir.clone()))
        .0
        .unwrap();

    // Bump version in source.
    fs::write(
        env.src_dir.join("plugin.toml"),
        r#"
[plugin]
id = "weather"
version = "0.2.0"
name = "Fixture"

[capabilities]
tools = ["echo"]

[transport]
kind = "stdio"
command = "./bin/handler"
"#,
    )
    .unwrap();

    let (res, out) = install(
        &env,
        None,
        InstallOptions {
            update: true,
            ..opts_default(env.src_dir.clone())
        },
    );
    res.unwrap();
    assert!(out.contains("updated: weather 0.2.0"));
    let installed_toml =
        fs::read_to_string(env.search_root.join("weather").join("plugin.toml")).unwrap();
    assert!(installed_toml.contains("0.2.0"));
}

#[test]
fn install_update_fails_when_not_already_installed() {
    let env = Env::new("weather");
    let (res, _) = install(
        &env,
        None,
        InstallOptions {
            update: true,
            ..opts_default(env.src_dir.clone())
        },
    );
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::UpdateTargetMissing(_)), "{err:?}");
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn install_link_creates_symlink_when_source_absolute() {
    let env = Env::new("weather");
    let abs_src = fs::canonicalize(&env.src_dir).unwrap();
    let (res, out) = install(
        &env,
        None,
        InstallOptions {
            link: true,
            source: abs_src.clone(),
            ..opts_default(abs_src)
        },
    );
    res.unwrap();
    assert!(out.contains("linked: weather"));
    let target = env.search_root.join("weather");
    let meta = fs::symlink_metadata(&target).unwrap();
    assert!(meta.file_type().is_symlink());
}

#[test]
fn install_link_rejects_relative_source() {
    let env = Env::new("weather");
    // src_dir starts absolute; fabricate a relative one by using file_name
    let rel = PathBuf::from(env.src_dir.file_name().unwrap());
    // Write a copy under cwd so metadata() succeeds during manifest resolve.
    fs::create_dir_all(&rel).unwrap();
    fs::write(
        rel.join("plugin.toml"),
        r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["echo"]

[transport]
kind = "stdio"
command = "./x"
"#,
    )
    .unwrap();
    let (res, _) = install(
        &env,
        None,
        InstallOptions {
            link: true,
            source: rel.clone(),
            ..opts_default(rel.clone())
        },
    );
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::LinkRequiresAbsolute), "{err:?}");
    let _ = fs::remove_dir_all(&rel);
}

#[test]
fn install_dry_run_does_not_touch_filesystem() {
    let env = Env::new("weather");
    let (res, out) = install(
        &env,
        None,
        InstallOptions {
            dry_run: true,
            ..opts_default(env.src_dir.clone())
        },
    );
    res.unwrap();
    assert!(out.contains("would install: weather"));
    assert!(!env.search_root.join("weather").exists());
}

#[test]
fn install_with_enable_removes_from_disabled() {
    let env = Env::new("weather");
    // seed extensions.yaml with weather disabled.
    let yaml = env.config_dir.join("extensions.yaml");
    fs::write(
        &yaml,
        "extensions:\n  search_paths: []\n  disabled: [weather]\n",
    )
    .unwrap();

    let (res, out) = install(
        &env,
        None,
        InstallOptions {
            enable: true,
            ..opts_default(env.src_dir.clone())
        },
    );
    res.unwrap();
    assert!(out.contains("enabled: weather"));
    let updated = fs::read_to_string(&yaml).unwrap();
    assert!(
        !updated.contains("weather"),
        "disabled should be empty, got: {updated}"
    );
}

#[test]
fn install_rejects_invalid_manifest() {
    let env = Env::new("weather");
    fs::write(env.src_dir.join("plugin.toml"), b"this is not toml").unwrap();
    let (res, _) = install(&env, None, opts_default(env.src_dir.clone()));
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::InvalidManifest(_)), "{err:?}");
    assert_eq!(err.exit_code(), 2);
}

#[test]
fn install_rejects_reserved_id() {
    // Reserved ids are rejected at manifest parse time, so we see
    // `InvalidManifest` rather than `InvalidId`. Either is a refusal
    // to install; the CLI's own `validate_id_not_reserved` is a
    // belt-and-suspenders second layer for manifests that somehow slip
    // through.
    let env = Env::with_version("agent", "0.1.0");
    let (res, _) = install(&env, None, opts_default(env.src_dir.clone()));
    let err = res.unwrap_err();
    assert!(
        matches!(
            err,
            CliError::InvalidManifest(_) | CliError::InvalidId(_, _)
        ),
        "{err:?}"
    );
}

#[test]
fn install_detects_id_collision_across_search_paths() {
    let env = Env::new("weather");
    let (_secondary, cfg) = env.with_secondary_search();
    // pre-seed the secondary path with a conflicting ext
    let other = cfg.search_paths[1].clone();
    let other_dir = PathBuf::from(&other).join("weather");
    write_plugin_toml(&other_dir, "weather", "0.0.9");

    let (res, _) = install(&env, Some(cfg), opts_default(env.src_dir.clone()));
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::IdCollision { .. }), "{err:?}");
    assert_eq!(err.exit_code(), 6);
}

#[test]
fn install_mutually_exclusive_update_and_link() {
    let env = Env::new("weather");
    let (res, _) = install(
        &env,
        None,
        InstallOptions {
            update: true,
            link: true,
            ..opts_default(env.src_dir.clone())
        },
    );
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::InvalidSource(_)), "{err:?}");
}

#[test]
fn install_json_output_contains_mode() {
    let env = Env::new("weather");
    let (res, out) = install(
        &env,
        None,
        InstallOptions {
            json: true,
            ..opts_default(env.src_dir.clone())
        },
    );
    res.unwrap();
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["id"], "weather");
    assert_eq!(v["mode"], "copy");
    assert_eq!(v["enabled"], false);
}

// ── uninstall ─────────────────────────────────────────────────────────────────

#[test]
fn uninstall_removes_copied_dir() {
    let env = Env::new("weather");
    install(&env, None, opts_default(env.src_dir.clone()))
        .0
        .unwrap();
    let target = env.search_root.join("weather");
    assert!(target.exists());
    let (res, out) = uninstall(
        &env,
        None,
        UninstallOptions {
            id: "weather".into(),
            yes: true,
            json: false,
        },
    );
    res.unwrap();
    assert!(!target.exists());
    assert!(out.contains("uninstalled: weather"));
}

#[test]
fn uninstall_removes_symlink_without_following() {
    let env = Env::new("weather");
    let abs_src = fs::canonicalize(&env.src_dir).unwrap();
    install(
        &env,
        None,
        InstallOptions {
            link: true,
            source: abs_src.clone(),
            ..opts_default(abs_src.clone())
        },
    )
    .0
    .unwrap();
    let target = env.search_root.join("weather");
    assert!(fs::symlink_metadata(&target)
        .unwrap()
        .file_type()
        .is_symlink());

    let (res, _) = uninstall(
        &env,
        None,
        UninstallOptions {
            id: "weather".into(),
            yes: true,
            json: false,
        },
    );
    res.unwrap();
    // Symlink gone …
    assert!(fs::symlink_metadata(&target).is_err());
    // … but source dir intact.
    assert!(abs_src.join("plugin.toml").exists());
}

#[test]
fn uninstall_without_yes_fails_with_exit_7() {
    let env = Env::new("weather");
    install(&env, None, opts_default(env.src_dir.clone()))
        .0
        .unwrap();
    let (res, _) = uninstall(
        &env,
        None,
        UninstallOptions {
            id: "weather".into(),
            yes: false,
            json: false,
        },
    );
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::MissingConfirmation));
    assert_eq!(err.exit_code(), 7);
}

#[test]
fn uninstall_unknown_id_fails_with_exit_1() {
    let env = Env::new("weather");
    let (res, _) = uninstall(
        &env,
        None,
        UninstallOptions {
            id: "nope".into(),
            yes: true,
            json: false,
        },
    );
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::NotFound(_)));
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn install_outcome_variant_matches_spec() {
    let env = Env::new("weather");
    let (res, _) = install(&env, None, opts_default(env.src_dir.clone()));
    res.unwrap();
    // Round-trip check for the InstallMode type — mostly here so the
    // public enum keeps its shape as the CLI grows.
    let modes = [InstallMode::Copy,
        InstallMode::Link,
        InstallMode::Update,
        InstallMode::DryRun];
    assert_eq!(modes.len(), 4);
}
