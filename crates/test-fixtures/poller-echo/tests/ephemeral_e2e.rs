//! Phase 96 — end-to-end test of the ephemeral lifecycle path.
//! Spawns the colocated `poller_echo` fixture binary via the
//! daemon-side `EphemeralPollerProxy::spawn_ephemeral_tick`
//! primitive and asserts the full stdio JSON-RPC round-trip
//! (request envelope → child execution → reply envelope → TickAck
//! decode + cursor base64 round-trip + metrics propagation +
//! error classification + timeout enforcement).
//!
//! Pairs with the unit tests in
//! `crates/pairing/src/plugin_poller.rs::tests` — those cover the
//! pure-fn primitives (request build, reply decode, router
//! register/lookup). This file covers the actual fork/spawn/JSON-RPC
//! pipe round-trip the unit tests can't reach.

use std::time::Duration;

use chrono::Utc;
use nexo_pairing::plugin_poller::{build_tick_request, spawn_ephemeral_tick};
use nexo_poller::PollerError;
use serde_json::json;
use serial_test::serial;
use tokio_util::sync::CancellationToken;

const BINARY: &str = env!("CARGO_BIN_EXE_poller_echo");

fn make_request(
    kind: &'static str,
    job_id: &str,
    cursor: Option<&[u8]>,
    config: serde_json::Value,
) -> nexo_pairing::plugin_poller::TickRequest {
    build_tick_request(
        kind,
        job_id,
        "ana",
        cursor,
        config,
        Utc::now(),
        Duration::from_secs(60),
    )
}

#[tokio::test]
#[serial]
async fn ephemeral_happy_path_round_trips_cursor_and_metrics() {
    // Default `POLLER_ECHO_MODE=ok` path. Fixture echoes
    // `kind|job_id|<cursor_in>` into next_cursor + writes items_seen
    // from `config.echo_items`.
    // Ensure no inherited env biases the run.
    std::env::remove_var("POLLER_ECHO_MODE");
    std::env::remove_var("POLLER_ECHO_SLEEP_MS");
    std::env::remove_var("POLLER_ECHO_CRASH");
    std::env::remove_var("POLLER_ECHO_CRASH_EARLY");

    let req = make_request(
        "echo",
        "ephemeral_happy",
        Some(b"prev-cursor"),
        json!({ "echo_items": 7 }),
    );
    let ack = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await
    .expect("happy path returns TickAck");

    // Metrics propagate from config.
    let metrics = ack.metrics.expect("metrics present");
    assert_eq!(metrics.items_seen, 7);
    assert_eq!(metrics.items_dispatched, 0);

    // Cursor identity round-trips. The fixture echoes the cursor
    // string it received from stdin (which is the URL-safe base64
    // form the daemon stamped) so we assert on that — proves the
    // daemon-side `build_tick_request` base64-encodes raw bytes,
    // the fixture passes it through, and the daemon-side
    // `TickReply::into_tick_ack` base64-decodes back to raw bytes.
    //
    // base64("prev-cursor") = "cHJldi1jdXJzb3I" (URL_SAFE_NO_PAD).
    let cursor_bytes = ack.next_cursor.expect("cursor present");
    let s = std::str::from_utf8(&cursor_bytes).expect("cursor is utf-8");
    assert_eq!(s, "echo|ephemeral_happy|cHJldi1jdXJzb3I");

    assert!(ack.next_interval_hint.is_none());
}

#[tokio::test]
#[serial]
async fn ephemeral_first_run_passes_none_cursor() {
    std::env::remove_var("POLLER_ECHO_MODE");
    let req = make_request("echo", "first_run", None, json!({}));
    let ack = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await
    .expect("first-run with None cursor still round-trips");
    let cursor_bytes = ack.next_cursor.expect("cursor present");
    let s = std::str::from_utf8(&cursor_bytes).unwrap();
    // No prev cursor → trailing `|` with empty suffix.
    assert_eq!(s, "echo|first_run|");
}

#[tokio::test]
#[serial]
async fn ephemeral_permanent_error_classifies_correctly() {
    std::env::set_var("POLLER_ECHO_MODE", "permanent_error");
    let req = make_request("echo", "perm_err", None, json!({}));
    let result = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await;
    std::env::remove_var("POLLER_ECHO_MODE");

    let err = result.expect_err("permanent_error mode must surface PollerError");
    assert!(
        matches!(err, PollerError::Permanent(_)),
        "expected Permanent, got {err:?}"
    );
}

#[tokio::test]
#[serial]
async fn ephemeral_transient_error_classifies_correctly() {
    std::env::set_var("POLLER_ECHO_MODE", "transient_error");
    let req = make_request("echo", "trans_err", None, json!({}));
    let result = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await;
    std::env::remove_var("POLLER_ECHO_MODE");

    let err = result.expect_err("transient_error must surface PollerError");
    assert!(
        matches!(err, PollerError::Transient(_)),
        "expected Transient, got {err:?}"
    );
}

#[tokio::test]
#[serial]
async fn ephemeral_config_error_classifies_correctly() {
    std::env::set_var("POLLER_ECHO_MODE", "config_error");
    let req = make_request("echo", "cfg_err", None, json!({}));
    let result = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await;
    std::env::remove_var("POLLER_ECHO_MODE");

    let err = result.expect_err("config_error must surface PollerError");
    assert!(
        matches!(err, PollerError::Config { .. }),
        "expected Config, got {err:?}"
    );
}

#[tokio::test]
#[serial]
async fn ephemeral_malformed_reply_is_transient() {
    std::env::set_var("POLLER_ECHO_MODE", "malformed");
    let req = make_request("echo", "malformed", None, json!({}));
    let result = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await;
    std::env::remove_var("POLLER_ECHO_MODE");

    let err = result.expect_err("malformed reply must surface error");
    assert!(
        matches!(err, PollerError::Transient(_)),
        "expected Transient on parse failure, got {err:?}"
    );
}

#[tokio::test]
#[serial]
async fn ephemeral_tick_timeout_kills_slow_child() {
    std::env::remove_var("POLLER_ECHO_MODE");
    std::env::set_var("POLLER_ECHO_SLEEP_MS", "5000");
    let req = make_request("echo", "slow", None, json!({}));
    let started = std::time::Instant::now();
    let result = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_millis(500),
        CancellationToken::new(),
    )
    .await;
    std::env::remove_var("POLLER_ECHO_SLEEP_MS");
    let elapsed = started.elapsed();

    let err = result.expect_err("must time out");
    assert!(
        matches!(err, PollerError::Transient(_)),
        "expected Transient on timeout, got {err:?}"
    );
    // Timeout was 500ms — we should be back well under the 5s sleep.
    assert!(
        elapsed < Duration::from_secs(2),
        "timeout took too long: {elapsed:?}",
    );
}

#[tokio::test]
#[serial]
async fn ephemeral_cancel_token_kills_child_during_tick() {
    std::env::remove_var("POLLER_ECHO_MODE");
    std::env::set_var("POLLER_ECHO_SLEEP_MS", "5000");
    let req = make_request("echo", "cancelled", None, json!({}));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });
    let started = std::time::Instant::now();
    let result = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_secs(10),
        cancel,
    )
    .await;
    std::env::remove_var("POLLER_ECHO_SLEEP_MS");
    let elapsed = started.elapsed();

    let err = result.expect_err("cancel token must abort the tick");
    assert!(matches!(err, PollerError::Transient(_)));
    assert!(
        elapsed < Duration::from_secs(2),
        "cancel handled too slowly: {elapsed:?}",
    );
}

#[tokio::test]
#[serial]
async fn ephemeral_child_crash_before_reply_classifies_transient() {
    std::env::set_var("POLLER_ECHO_CRASH", "1");
    let req = make_request("echo", "crash_mid", None, json!({}));
    let result = spawn_ephemeral_tick(
        BINARY,
        "test-echo",
        req,
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await;
    std::env::remove_var("POLLER_ECHO_CRASH");

    let err = result.expect_err("child crash before reply must error");
    assert!(matches!(err, PollerError::Transient(_)));
}

#[tokio::test]
#[serial]
async fn ephemeral_missing_binary_returns_transient() {
    let req = make_request("echo", "no_binary", None, json!({}));
    let result = spawn_ephemeral_tick(
        "/nonexistent/path/to/poller_echo_does_not_exist",
        "test-echo",
        req,
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await;
    let err = result.expect_err("missing binary must error");
    assert!(matches!(err, PollerError::Transient(_)));
}
