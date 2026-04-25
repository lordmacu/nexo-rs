use onepassword_ext::tools;
use serde_json::{json, Value};
use serial_test::serial;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Build a fake `op` shell script that handles the subcommands the
/// extension actually invokes (`inject` reading stdin and `read`).
/// Returns a directory to prepend to PATH for the test.
fn fake_op_dir(version_suffix: &str) -> PathBuf {
    let dir = tempfile::tempdir().unwrap().keep();
    let path = dir.join("op");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "#!/bin/sh").unwrap();
    writeln!(f, "case \"$1\" in").unwrap();
    writeln!(f, "  inject)").unwrap();
    // Emulate `op inject` by replacing every `{{ op://... }}` with a
    // deterministic fake value. POSIX sed: substitute the placeholder
    // with `RESOLVED_<suffix>`.
    writeln!(
        f,
        "    sed 's|{{{{ *op://[^}}]*}}}}|RESOLVED_{version_suffix}|g'"
    )
    .unwrap();
    writeln!(f, "    ;;").unwrap();
    writeln!(f, "  read)").unwrap();
    writeln!(f, "    echo READ_VALUE_{version_suffix}").unwrap();
    writeln!(f, "    ;;").unwrap();
    writeln!(f, "  *)").unwrap();
    writeln!(f, "    echo unsupported >&2; exit 2").unwrap();
    writeln!(f, "    ;;").unwrap();
    writeln!(f, "esac").unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    dir
}

fn set_path_with(extra: &std::path::Path) {
    let orig = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{orig}", extra.display()));
}

fn fresh_audit_path() -> PathBuf {
    let dir = tempfile::tempdir().unwrap().keep();
    dir.join("audit.jsonl")
}

#[test]
#[serial]
fn inject_template_only_returns_fingerprint_when_reveal_off() {
    let dir = fake_op_dir("xyz");
    set_path_with(&dir);
    let audit_path = fresh_audit_path();
    std::env::set_var("OP_AUDIT_LOG_PATH", &audit_path);
    std::env::set_var("OP_SERVICE_ACCOUNT_TOKEN", "fake");
    std::env::remove_var("OP_ALLOW_REVEAL");

    let out = tools::dispatch(
        "inject_template",
        &json!({"template": "value: {{ op://Prod/X/y }}"}),
    )
    .unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(out["reveal"], false);
    assert!(out.get("rendered").is_none());
    assert!(out["fingerprint_sha256_prefix"].is_string());
    assert_eq!(
        out["references"],
        Value::Array(vec![Value::String("op://Prod/X/y".into())])
    );

    // audit row written
    let raw = std::fs::read_to_string(&audit_path).unwrap();
    let line: Value = serde_json::from_str(raw.trim()).unwrap();
    assert_eq!(line["action"], "inject_template");
    assert_eq!(line["ok"], true);
    assert_eq!(line["reveal_allowed"], false);
}

#[test]
#[serial]
fn inject_template_only_returns_value_when_reveal_on() {
    let dir = fake_op_dir("rev1");
    set_path_with(&dir);
    let audit_path = fresh_audit_path();
    std::env::set_var("OP_AUDIT_LOG_PATH", &audit_path);
    std::env::set_var("OP_SERVICE_ACCOUNT_TOKEN", "fake");
    std::env::set_var("OP_ALLOW_REVEAL", "true");

    let out = tools::dispatch(
        "inject_template",
        &json!({"template": "header={{ op://A/B/c }}"}),
    )
    .unwrap();
    assert_eq!(out["reveal"], true);
    let rendered = out["rendered"].as_str().unwrap();
    assert!(rendered.contains("RESOLVED_rev1"));

    std::env::remove_var("OP_ALLOW_REVEAL");
}

#[test]
#[serial]
fn inject_exec_rejected_when_command_not_in_allowlist() {
    let dir = fake_op_dir("aa");
    set_path_with(&dir);
    let audit_path = fresh_audit_path();
    std::env::set_var("OP_AUDIT_LOG_PATH", &audit_path);
    std::env::set_var("OP_SERVICE_ACCOUNT_TOKEN", "fake");
    std::env::set_var("OP_INJECT_COMMAND_ALLOWLIST", "psql,curl");

    let err = tools::dispatch(
        "inject_template",
        &json!({"template": "x", "command": "rm"}),
    )
    .err()
    .unwrap();
    assert!(err.message.contains("not in OP_INJECT_COMMAND_ALLOWLIST"));

    let raw = std::fs::read_to_string(&audit_path).unwrap();
    let line: Value = serde_json::from_str(raw.trim()).unwrap();
    assert_eq!(line["ok"], false);
    assert_eq!(line["error"], "command_not_in_allowlist");

    std::env::remove_var("OP_INJECT_COMMAND_ALLOWLIST");
}

#[test]
#[serial]
fn inject_exec_pipes_to_allowlisted_command() {
    let dir = fake_op_dir("piped");
    set_path_with(&dir);
    let audit_path = fresh_audit_path();
    std::env::set_var("OP_AUDIT_LOG_PATH", &audit_path);
    std::env::set_var("OP_SERVICE_ACCOUNT_TOKEN", "fake");
    std::env::set_var("OP_INJECT_COMMAND_ALLOWLIST", "cat");

    let out = tools::dispatch(
        "inject_template",
        &json!({
            "template": "tok={{ op://P/X/y }}",
            "command": "cat",
            "args": []
        }),
    )
    .unwrap();
    assert_eq!(out["ok"], true);
    let stdout = out["stdout"].as_str().unwrap();
    // `cat` echoes whatever `op inject` wrote; our fake injects RESOLVED_piped.
    assert!(stdout.contains("RESOLVED_piped"));
    assert_eq!(out["exit_code"], 0);

    std::env::remove_var("OP_INJECT_COMMAND_ALLOWLIST");
}

#[test]
#[serial]
fn inject_dry_run_validates_references_without_running_op() {
    // No fake op needed — dry_run does not invoke the binary for resolution.
    std::env::set_var("OP_SERVICE_ACCOUNT_TOKEN", "fake");
    let audit_path = fresh_audit_path();
    std::env::set_var("OP_AUDIT_LOG_PATH", &audit_path);

    let out = tools::dispatch(
        "inject_template",
        &json!({
            "template": "a={{ op://V/I/f }} b={{ op://V2/I2/f2 }}",
            "dry_run": true
        }),
    )
    .unwrap();
    assert_eq!(out["dry_run"], true);
    let refs = out["references_validated"].as_array().unwrap();
    assert_eq!(refs.len(), 2);

    let raw = std::fs::read_to_string(&audit_path).unwrap();
    let line: Value = serde_json::from_str(raw.trim()).unwrap();
    assert_eq!(line["dry_run"], true);
    assert_eq!(line["ok"], true);
}

#[test]
#[serial]
fn inject_dry_run_rejects_invalid_reference() {
    std::env::set_var("OP_SERVICE_ACCOUNT_TOKEN", "fake");
    let audit_path = fresh_audit_path();
    std::env::set_var("OP_AUDIT_LOG_PATH", &audit_path);

    let err = tools::dispatch(
        "inject_template",
        &json!({
            "template": "a={{ op://malformed }}",
            "dry_run": true
        }),
    )
    .err()
    .unwrap();
    assert!(err.message.contains("invalid op://"));
}

#[test]
#[serial]
fn extract_template_references_finds_all() {
    use onepassword_ext::op_cli;
    let refs = op_cli::extract_template_references(
        "first={{ op://A/B/c }} second={{op://X/Y/z}} third={{ op://M/N/o }}",
    );
    assert_eq!(refs, vec!["op://A/B/c", "op://X/Y/z", "op://M/N/o"]);
}
