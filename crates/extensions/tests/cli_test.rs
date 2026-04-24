//! Phase 11.7 — integration tests for `agent ext` subcommands.
//!
//! Uses `tempfile::TempDir` to build a fixture workspace with manifests and
//! an `extensions.yaml`, then exercises the CLI entry points directly.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use agent_config::ExtensionsConfig;
use agent_extensions::cli::{
    run_disable, run_doctor, run_enable, run_info, run_list, run_validate, CliContext,
};

/// Build a temp `config_dir` (with optional `extensions.yaml`) plus one or
/// more manifest fixtures under `<config_dir>/../extensions/`.
struct Fixture {
    _td: tempfile::TempDir,
    config_dir: PathBuf,
    ext_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let td = tempfile::tempdir().unwrap();
        let config_dir = td.path().join("config");
        let ext_dir = td.path().join("extensions");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&ext_dir).unwrap();
        Self {
            _td: td,
            config_dir,
            ext_dir,
        }
    }

    fn base_cfg(&self) -> ExtensionsConfig {
        let mut cfg = ExtensionsConfig::default();
        cfg.search_paths = vec![self.ext_dir.display().to_string()];
        cfg
    }

    fn write_manifest(&self, id: &str, body: &str) -> PathBuf {
        let dir = self.ext_dir.join(id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugin.toml");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        dir
    }
}

fn valid_manifest(id: &str, version: &str) -> String {
    format!(
        r#"
[plugin]
id = "{id}"
version = "{version}"
name = "Fixture"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"
"#
    )
}

fn broken_manifest() -> String {
    "this is = not valid toml ==".to_string()
}

fn read_stdout<F>(f: F) -> String
where
    F: FnOnce(&mut Vec<u8>, &mut Vec<u8>),
{
    let mut out = Vec::new();
    let mut err = Vec::new();
    f(&mut out, &mut err);
    String::from_utf8(out).unwrap()
}

#[test]
fn list_renders_table_with_enabled_and_disabled() {
    let fx = Fixture::new();
    fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    fx.write_manifest("calendar", &valid_manifest("calendar", "0.2.0"));

    let mut cfg = fx.base_cfg();
    cfg.disabled.push("calendar".into());

    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: fx.config_dir.clone(),
        extensions: cfg,
        out: &mut out,
        err: &mut err,
    };
    run_list(ctx, false).expect("list");
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("weather"));
    assert!(s.contains("0.1.0"));
    assert!(s.contains("enabled"));
    assert!(s.contains("calendar"));
    assert!(s.contains("disabled"));
}

#[test]
fn list_json_schema_stable() {
    let fx = Fixture::new();
    fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    let cfg = fx.base_cfg();

    let s = read_stdout(|out, err| {
        let ctx = CliContext {
            config_dir: fx.config_dir.clone(),
            extensions: cfg,
            out,
            err,
        };
        run_list(ctx, true).unwrap();
    });
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["rows"][0]["id"], "weather");
    assert_eq!(v["rows"][0]["status"], "enabled");
    assert_eq!(v["rows"][0]["tools"], 1);
    assert_eq!(v["rows"][0]["transport"], "stdio");
}

#[test]
fn info_happy_path() {
    let fx = Fixture::new();
    fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    let cfg = fx.base_cfg();

    let s = read_stdout(|out, err| {
        let ctx = CliContext {
            config_dir: fx.config_dir.clone(),
            extensions: cfg,
            out,
            err,
        };
        run_info(ctx, "weather", false).unwrap();
    });
    assert!(s.contains("id:"));
    assert!(s.contains("weather"));
    assert!(s.contains("stdio"));
}

#[test]
fn info_not_found_returns_error_with_exit_code_1() {
    let fx = Fixture::new();
    fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    let cfg = fx.base_cfg();

    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: fx.config_dir.clone(),
        extensions: cfg,
        out: &mut out,
        err: &mut err,
    };
    let result = run_info(ctx, "ghost", false);
    let e = result.unwrap_err();
    assert_eq!(e.exit_code(), 1);
}

#[test]
fn validate_ok_on_good_manifest() {
    let fx = Fixture::new();
    let dir = fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    let cfg = fx.base_cfg();

    let s = read_stdout(|out, err| {
        let ctx = CliContext {
            config_dir: fx.config_dir.clone(),
            extensions: cfg,
            out,
            err,
        };
        run_validate(ctx, &dir).unwrap();
    });
    assert!(s.starts_with("ok:"));
}

#[test]
fn validate_error_on_broken_toml() {
    let fx = Fixture::new();
    let dir = fx.write_manifest("broken", &broken_manifest());
    let cfg = fx.base_cfg();

    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: fx.config_dir.clone(),
        extensions: cfg,
        out: &mut out,
        err: &mut err,
    };
    let e = run_validate(ctx, &dir).unwrap_err();
    assert_eq!(e.exit_code(), 2);
}

#[test]
fn doctor_reports_errors_for_broken_manifests() {
    let fx = Fixture::new();
    fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    fx.write_manifest("broken", &broken_manifest());
    let cfg = fx.base_cfg();

    let s = read_stdout(|out, err| {
        let ctx = CliContext {
            config_dir: fx.config_dir.clone(),
            extensions: cfg,
            out,
            err,
        };
        run_doctor(ctx).unwrap();
    });
    assert!(s.contains("candidates"));
    assert!(
        s.contains("ERRORS") || s.contains("error"),
        "should flag the broken manifest: {s}"
    );
}

#[test]
fn disable_then_enable_round_trip() {
    let fx = Fixture::new();
    fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));

    // disable
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: fx.config_dir.clone(),
        extensions: fx.base_cfg(),
        out: &mut out,
        err: &mut err,
    };
    run_disable(ctx, "weather").expect("disable");
    let yaml_path = fx.config_dir.join("extensions.yaml");
    assert!(yaml_path.exists(), "yaml was not written");
    let yaml = fs::read_to_string(&yaml_path).unwrap();
    assert!(yaml.contains("weather"));
    assert!(yaml.contains("disabled"));

    // reload cfg (simulate CLI re-invocation) and enable
    let mut cfg2 = fx.base_cfg();
    cfg2.disabled.push("weather".into());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: fx.config_dir.clone(),
        extensions: cfg2,
        out: &mut out,
        err: &mut err,
    };
    run_enable(ctx, "weather").expect("enable");
    let yaml = fs::read_to_string(&yaml_path).unwrap();
    let f: agent_config::ExtensionsConfigFile = serde_yaml::from_str(&yaml).unwrap();
    assert!(
        f.extensions.disabled.is_empty(),
        "weather should be enabled again"
    );
}

#[test]
fn enable_unknown_id_is_not_found() {
    let fx = Fixture::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: fx.config_dir.clone(),
        extensions: fx.base_cfg(),
        out: &mut out,
        err: &mut err,
    };
    let e = run_enable(ctx, "nonexistent").unwrap_err();
    assert_eq!(e.exit_code(), 1);
}

#[test]
fn disable_reserved_id_is_invalid() {
    let fx = Fixture::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: fx.config_dir.clone(),
        extensions: fx.base_cfg(),
        out: &mut out,
        err: &mut err,
    };
    let e = run_disable(ctx, "agent").unwrap_err();
    assert_eq!(e.exit_code(), 4);
}

#[test]
fn disable_creates_yaml_when_missing() {
    let fx = Fixture::new();
    fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    let yaml_path = fx.config_dir.join("extensions.yaml");
    assert!(!yaml_path.exists());

    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: fx.config_dir.clone(),
        extensions: fx.base_cfg(),
        out: &mut out,
        err: &mut err,
    };
    run_disable(ctx, "weather").unwrap();
    assert!(yaml_path.exists(), "yaml was not materialised");
    let body = fs::read_to_string(&yaml_path).unwrap();
    assert!(body.contains("weather"));
}

#[test]
fn doctor_clean_when_no_issues() {
    let fx = Fixture::new();
    fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    let cfg = fx.base_cfg();

    let s = read_stdout(|out, err| {
        let ctx = CliContext {
            config_dir: fx.config_dir.clone(),
            extensions: cfg,
            out,
            err,
        };
        run_doctor(ctx).unwrap();
    });
    assert!(s.contains("no diagnostics"));
}

// Used to check exit-code helper isn't drifting away from the spec.
#[test]
fn exit_codes_are_documented() {
    use agent_extensions::cli::CliError;
    assert_eq!(CliError::NotFound("x".into()).exit_code(), 1);
    assert_eq!(CliError::InvalidManifest("x".into()).exit_code(), 2);
    assert_eq!(CliError::ConfigWrite("x".into()).exit_code(), 3);
    assert_eq!(CliError::InvalidId("x".into(), "y".into()).exit_code(), 4);
}

// Ensure tests that expect a specific path structure don't silently break.
#[test]
fn fixture_layout_sanity() {
    let fx = Fixture::new();
    let dir = fx.write_manifest("weather", &valid_manifest("weather", "0.1.0"));
    assert!(dir.exists());
    assert!(Path::new(&dir).join("plugin.toml").exists());
}

#[test]
fn info_includes_mcp_servers_when_declared() {
    let fx = Fixture::new();
    let body = r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.api]
transport = "streamable_http"
url = "https://example.com/mcp"
"#;
    fx.write_manifest("weather", body);
    let cfg = fx.base_cfg();

    let s = read_stdout(|out, err| {
        let ctx = CliContext {
            config_dir: fx.config_dir.clone(),
            extensions: cfg,
            out,
            err,
        };
        run_info(ctx, "weather", false).unwrap();
    });
    assert!(s.contains("mcp_servers:"));
    assert!(s.contains("api (streamable_http)"));
    assert!(s.contains("https://example.com/mcp"));
}

#[test]
fn info_json_exposes_mcp_servers_key() {
    let fx = Fixture::new();
    let body = r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.local]
transport = "stdio"
command = "/bin/true"
"#;
    fx.write_manifest("weather", body);
    let cfg = fx.base_cfg();

    let s = read_stdout(|out, err| {
        let ctx = CliContext {
            config_dir: fx.config_dir.clone(),
            extensions: cfg,
            out,
            err,
        };
        run_info(ctx, "weather", true).unwrap();
    });
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert!(v["mcp_servers"].is_object());
    assert!(v["mcp_servers"]["local"].is_object());
}
