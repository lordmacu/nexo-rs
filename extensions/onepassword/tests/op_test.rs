//! Integration tests for the 1password extension. We never require a real
//! `op` binary or live service-account token — every test builds a bash
//! fake that drops predetermined output and points `OP_BIN_OVERRIDE` at it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use serde_json::json;
use serial_test::serial;

use onepassword_ext::tools;

/// Writes a bash script that switches on `$1` (the op subcommand) and echoes
/// the requested JSON / value fixture. Exit non-zero for unknown invocations.
fn write_fake_op(dir: &PathBuf, body: &str) -> PathBuf {
    let path = dir.join("fake-op.sh");
    fs::write(
        &path,
        format!(
            "#!/usr/bin/env bash\n\
             set -e\n\
             {body}\n\
             echo \"fake-op: unknown args: $@\" 1>&2\n\
             exit 2\n"
        ),
    )
    .expect("write fake op");
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

fn set_env(bin_path: &PathBuf) {
    std::env::set_var("OP_BIN_OVERRIDE", bin_path);
    std::env::set_var("OP_SERVICE_ACCOUNT_TOKEN", "ops_test_fake_token");
    std::env::remove_var("OP_ALLOW_REVEAL");
}

fn scratch() -> PathBuf {
    tempfile::Builder::new()
        .prefix("op-ext-test-")
        .tempdir()
        .expect("tempdir")
        .into_path()
}

#[test]
#[serial]
fn status_reports_bin_and_token() {
    let dir = scratch();
    let bin = write_fake_op(&dir, "exit 0");
    set_env(&bin);

    let out = tools::dispatch("status", &json!({})).expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["token_present"], true);
    assert_eq!(out["reveal_allowed"], false);
    assert!(out["bin"].as_str().unwrap().contains("fake-op.sh"));
}

#[test]
#[serial]
fn whoami_passes_through_json() {
    let dir = scratch();
    let bin = write_fake_op(
        &dir,
        r#"
if [[ "$1" == "whoami" ]]; then
  echo '{"url":"my.1password.com","user_uuid":"U123","account_uuid":"A456"}'
  exit 0
fi"#,
    );
    set_env(&bin);
    let out = tools::dispatch("whoami", &json!({})).expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["whoami"]["user_uuid"], "U123");
}

#[test]
#[serial]
fn list_vaults_returns_id_name() {
    let dir = scratch();
    let bin = write_fake_op(
        &dir,
        r#"
if [[ "$1" == "vault" && "$2" == "list" ]]; then
  echo '[{"id":"v1","name":"Prod","extra":"ignored"},{"id":"v2","name":"Staging"}]'
  exit 0
fi"#,
    );
    set_env(&bin);
    let out = tools::dispatch("list_vaults", &json!({})).expect("ok");
    assert_eq!(out["count"], 2);
    assert_eq!(out["vaults"][0]["id"], "v1");
    assert_eq!(out["vaults"][0]["name"], "Prod");
    // Unexpected fields should be stripped.
    assert!(out["vaults"][0].get("extra").is_none());
}

#[test]
#[serial]
fn list_items_strips_secret_fields() {
    let dir = scratch();
    let bin = write_fake_op(
        &dir,
        r#"
if [[ "$1" == "item" && "$2" == "list" ]]; then
  echo '[{"id":"i1","title":"Stripe","category":"API_CREDENTIAL","fields":[{"value":"SECRET_LEAK"}]}]'
  exit 0
fi"#,
    );
    set_env(&bin);
    let out = tools::dispatch("list_items", &json!({"vault": "Prod"})).expect("ok");
    let items = out["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Stripe");
    assert_eq!(items[0]["category"], "API_CREDENTIAL");
    // Item's internal `fields` array must NOT appear — we strip at serialize time.
    assert!(items[0].get("fields").is_none());
    let json_string = serde_json::to_string(&out).unwrap();
    assert!(
        !json_string.contains("SECRET_LEAK"),
        "secret value leaked into list_items output"
    );
}

#[test]
#[serial]
fn read_secret_denies_value_by_default() {
    let dir = scratch();
    let bin = write_fake_op(
        &dir,
        r#"
if [[ "$1" == "read" ]]; then
  echo -n "sk-live-XXXXXXXXXXXXXXXXXX"
  exit 0
fi"#,
    );
    set_env(&bin);
    std::env::remove_var("OP_ALLOW_REVEAL");

    let out = tools::dispatch(
        "read_secret",
        &json!({"reference": "op://Prod/Stripe/api_key"}),
    )
    .expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["reveal"], false);
    assert!(out.get("value").is_none());
    assert!(out["fingerprint_sha256_prefix"].as_str().unwrap().len() == 16);
    assert_eq!(out["length"], "sk-live-XXXXXXXXXXXXXXXXXX".len() as i64);
    // Never leak the raw value into the JSON output.
    let full = serde_json::to_string(&out).unwrap();
    assert!(!full.contains("sk-live-"));
}

#[test]
#[serial]
fn read_secret_reveals_when_flag_set() {
    let dir = scratch();
    let bin = write_fake_op(
        &dir,
        r#"
if [[ "$1" == "read" ]]; then
  echo -n "sk-live-REVEALED"
  exit 0
fi"#,
    );
    set_env(&bin);
    std::env::set_var("OP_ALLOW_REVEAL", "true");

    let out = tools::dispatch(
        "read_secret",
        &json!({"reference": "op://Prod/Stripe/api_key"}),
    )
    .expect("ok");
    assert_eq!(out["reveal"], true);
    assert_eq!(out["value"], "sk-live-REVEALED");

    std::env::remove_var("OP_ALLOW_REVEAL");
}

#[test]
#[serial]
fn read_secret_rejects_wildcard_ref() {
    let dir = scratch();
    let bin = write_fake_op(&dir, "exit 2");
    set_env(&bin);

    let err = tools::dispatch(
        "read_secret",
        &json!({"reference": "op://Prod/*/api_key"}),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("invalid secret reference"));
}

#[test]
#[serial]
fn read_secret_rejects_non_op_scheme() {
    let dir = scratch();
    let bin = write_fake_op(&dir, "exit 2");
    set_env(&bin);

    let err = tools::dispatch(
        "read_secret",
        &json!({"reference": "secret://Prod/Stripe/api_key"}),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
#[serial]
fn missing_token_surfaces_typed_error() {
    let dir = scratch();
    let bin = write_fake_op(&dir, "exit 2");
    std::env::set_var("OP_BIN_OVERRIDE", &bin);
    std::env::remove_var("OP_SERVICE_ACCOUNT_TOKEN");

    let err = tools::dispatch("whoami", &json!({})).unwrap_err();
    assert_eq!(err.code, -32041);
}

#[test]
#[serial]
fn nonzero_exit_includes_stderr_preview() {
    let dir = scratch();
    let bin = write_fake_op(
        &dir,
        r#"
if [[ "$1" == "whoami" ]]; then
  echo "auth failed: token expired" 1>&2
  exit 7
fi"#,
    );
    set_env(&bin);
    let err = tools::dispatch("whoami", &json!({})).unwrap_err();
    assert_eq!(err.code, -32042);
    assert!(err.message.contains("token expired"), "got: {}", err.message);
}

#[test]
#[serial]
fn unknown_tool_is_method_not_found() {
    let dir = scratch();
    let bin = write_fake_op(&dir, "exit 0");
    set_env(&bin);
    let err = tools::dispatch("not_a_tool", &json!({})).unwrap_err();
    assert_eq!(err.code, -32601);
}
