//! Real tmux required. Every test uses a unique ephemeral session name
//! under a scratch socket so tests don't clobber the operator's tmux.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use serial_test::serial;

use tmux_remote_ext::tools;

fn tmux_available() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

static SEQ: AtomicU64 = AtomicU64::new(0);

fn prepare_socket() -> PathBuf {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("agent-rs-tmux-test-{t}-{n}.sock"));
    std::env::set_var("TMUX_REMOTE_SOCKET", &path);
    path
}

fn session(prefix: &str) -> String {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    format!("{prefix}-{n}")
}

fn teardown(session_name: &str) {
    let _ = tools::dispatch(
        "kill_session",
        &json!({"session": session_name}),
    );
}

#[test]
#[serial]
fn status_reports_bin() {
    if !tmux_available() {
        return;
    }
    let _ = prepare_socket();
    let out = tools::dispatch("status", &json!({})).expect("ok");
    assert_eq!(out["ok"], true);
    assert!(out["bin"].as_str().unwrap().contains("tmux"));
}

#[test]
#[serial]
fn new_session_and_list() {
    if !tmux_available() {
        return;
    }
    let _ = prepare_socket();
    let name = session("t");
    let out = tools::dispatch("new_session", &json!({"name": name})).expect("ok");
    assert_eq!(out["ok"], true);

    let listed = tools::dispatch("list_sessions", &json!({})).expect("ok");
    let names: Vec<&str> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&name.as_str()), "got {names:?}");
    teardown(&name);
}

#[test]
#[serial]
fn send_keys_then_capture() {
    if !tmux_available() {
        return;
    }
    let _ = prepare_socket();
    let name = session("k");
    tools::dispatch("new_session", &json!({"name": name.clone()})).expect("create");
    // Run something visible.
    tools::dispatch(
        "send_keys",
        &json!({"session": name, "keys": "echo HOLA_MARKER_42", "enter": true}),
    )
    .expect("send");
    // tmux prints async — give it a moment.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let cap = tools::dispatch(
        "capture_pane",
        &json!({"session": name, "lines": 50}),
    )
    .expect("capture");
    let output = cap["output"].as_str().unwrap();
    assert!(output.contains("HOLA_MARKER_42"), "got:\n{output}");
    teardown(&name);
}

#[test]
#[serial]
fn invalid_session_name_rejected() {
    let _ = prepare_socket();
    let err = tools::dispatch(
        "new_session",
        &json!({"name": "bad; rm -rf /"}),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
#[serial]
fn list_sessions_empty_is_ok() {
    if !tmux_available() {
        return;
    }
    // Fresh socket with no server running.
    let sock = std::env::temp_dir().join(format!(
        "agent-rs-tmux-empty-{}.sock",
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::env::set_var("TMUX_REMOTE_SOCKET", &sock);
    let out = tools::dispatch("list_sessions", &json!({})).expect("ok");
    assert_eq!(out["count"], 0);
    assert_eq!(out["sessions"].as_array().unwrap().len(), 0);
}

#[test]
#[serial]
fn kill_session_removes_it() {
    if !tmux_available() {
        return;
    }
    let _ = prepare_socket();
    let name = session("kill");
    tools::dispatch("new_session", &json!({"name": name})).expect("create");
    tools::dispatch("kill_session", &json!({"session": name})).expect("kill");
    let listed = tools::dispatch("list_sessions", &json!({})).expect("list");
    let names: Vec<&str> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&name.as_str()));
}

#[test]
#[serial]
fn capture_lines_bounds_enforced() {
    if !tmux_available() {
        return;
    }
    let _ = prepare_socket();
    let name = session("cap");
    tools::dispatch("new_session", &json!({"name": name.clone()})).expect("create");
    let err = tools::dispatch(
        "capture_pane",
        &json!({"session": name, "lines": 99999}),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
    teardown(&name);
}

#[test]
#[serial]
fn unknown_tool_is_method_not_found() {
    let _ = prepare_socket();
    let err = tools::dispatch("nope", &json!({})).unwrap_err();
    assert_eq!(err.code, -32601);
}
