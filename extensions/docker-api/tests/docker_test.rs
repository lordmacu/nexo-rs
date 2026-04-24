//! Integration smoke tests for docker-api. We avoid depending on specific
//! containers being present — we verify the tool plumbing against real
//! `docker` CLI.

use serde_json::json;
use serial_test::serial;

use docker_api_ext::tools;

fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("--version")
        .output()
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
#[serial]
fn status_reports_bin() {
    if !docker_available() { return; }
    let out = tools::dispatch("status", &json!({})).expect("ok");
    assert_eq!(out["ok"], true);
    assert!(out["bin"].as_str().unwrap().contains("docker"));
}

#[test]
#[serial]
fn ps_returns_container_list_json() {
    if !docker_available() { return; }
    let out = tools::dispatch("ps", &json!({})).expect("ok");
    assert_eq!(out["ok"], true);
    assert!(out["containers"].is_array());
}

#[test]
#[serial]
fn name_validation_rejects_injection() {
    if !docker_available() { return; }
    let err = tools::dispatch("inspect", &json!({"target":"a; rm -rf /"}))
        .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
#[serial]
fn start_without_write_flag_denied() {
    if !docker_available() { return; }
    std::env::remove_var("DOCKER_API_ALLOW_WRITE");
    let err = tools::dispatch("start", &json!({"target":"nginx"}))
        .unwrap_err();
    assert_eq!(err.code, -32043);
}

#[test]
#[serial]
fn inspect_missing_target_returns_docker_error() {
    if !docker_available() { return; }
    let err = tools::dispatch("inspect", &json!({"target":"nonexistent-xyz-12345"}))
        .unwrap_err();
    // Docker's own "No such object" → -32081 NonZeroExit
    assert_eq!(err.code, -32081);
}

#[test]
#[serial]
fn unknown_tool_method_not_found() {
    let err = tools::dispatch("nope", &json!({})).unwrap_err();
    assert_eq!(err.code, -32601);
}
