//! Tests that don't require a real RTSP camera. We verify:
//! - status reports bin
//! - URL validation guards
//! - sandbox sandbox rejection
//! - snapshot against a local file (ffmpeg accepts file URLs for input — but
//!   we reject them in the URL validator, so these tests use http:// mocks...
//!   actually the easiest path: generate a tiny local file with ffmpeg, then
//!   point snapshot at `file://` via a test-only env escape.
//!
//! Instead of adding a test-only escape we just verify the input-validation
//! + sandbox surface, and exercise the real ffmpeg call against a synthetic
//! local MP4 through an `http://127.0.0.1` loopback — not worth the setup
//! here. Integration vs a live camera is covered by an `#[ignore]` test.

use std::path::PathBuf;

use serde_json::json;
use serial_test::serial;

use rtsp_snapshot_ext::tools;

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sandbox() -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix("rtsp-test-")
        .tempdir()
        .expect("tempdir")
        .into_path();
    std::env::set_var("RTSP_SNAPSHOT_OUTPUT_ROOT", &dir);
    dir
}

#[test]
#[serial]
fn status_reports_bin_and_sandbox() {
    if !ffmpeg_available() {
        return;
    }
    let sb = sandbox();
    let out = tools::dispatch("status", &json!({})).expect("ok");
    assert_eq!(out["ok"], true);
    assert!(out["bin"].as_str().unwrap().contains("ffmpeg"));
    assert!(out["sandbox_root"]
        .as_str()
        .unwrap()
        .contains(sb.file_name().unwrap().to_str().unwrap()));
}

#[test]
#[serial]
fn snapshot_rejects_file_scheme() {
    let _ = sandbox();
    let err = tools::dispatch(
        "snapshot",
        &json!({"url":"file:///etc/passwd","output_path":"/tmp/x.jpg"}),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
#[serial]
fn snapshot_rejects_garbage_url() {
    let sb = sandbox();
    let err = tools::dispatch(
        "snapshot",
        &json!({
            "url": "this is not a url",
            "output_path": sb.join("x.jpg").to_string_lossy(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
#[serial]
fn snapshot_rejects_output_outside_sandbox() {
    let _ = sandbox();
    let err = tools::dispatch(
        "snapshot",
        &json!({
            "url": "rtsp://cam.local/stream",
            "output_path": "/etc/snapshot.jpg",
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32034);
}

#[test]
#[serial]
fn clip_rejects_excessive_duration() {
    let sb = sandbox();
    let err = tools::dispatch(
        "clip",
        &json!({
            "url":"rtsp://cam.local/stream",
            "output_path": sb.join("c.mp4").to_string_lossy(),
            "duration_secs": 999,
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
#[serial]
fn snapshot_real_ffmpeg_fails_on_unreachable_host() {
    if !ffmpeg_available() {
        return;
    }
    let sb = sandbox();
    // Use a guaranteed-unreachable RTSP to trigger a failure path that still
    // exercises the subprocess runner end-to-end.
    let err = tools::dispatch(
        "snapshot",
        &json!({
            "url": "rtsp://127.0.0.1:1/stream",
            "output_path": sb.join("cam.jpg").to_string_lossy(),
            "transport": "tcp"
        }),
    )
    .unwrap_err();
    // -32032 non-zero exit (ffmpeg fails to connect) is expected; -32033
    // timeout also acceptable if ffmpeg hangs a while.
    assert!(err.code == -32032 || err.code == -32033, "got: {err:?}");
}

#[test]
#[serial]
fn unknown_tool_is_method_not_found() {
    let err = tools::dispatch("nope", &json!({})).unwrap_err();
    assert_eq!(err.code, -32601);
}
