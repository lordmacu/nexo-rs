//! Phase 81.18.b.e2e-mock-binary — daemon-side end-to-end coverage
//! of the subprocess plugin spawn wire shape. Pairs the proyecto
//! daemon's `SubprocessNexoPlugin` adapter with a synthetic mock
//! binary (`tests/fixtures/mock_subprocess_plugin.rs`) so the test
//! can exercise:
//!
//! - `initialize` JSON-RPC handshake replies with the right
//!   manifest + server_version shape.
//! - `with_spawn_env` populates the child process env via
//!   `Command::env_clear().envs(&map)` (defense-in-depth — daemon
//!   sentinel vars don't leak).
//! - `with_instance_label` distinguishes N concurrent instances.
//! - `shutdown` reply round-trips cleanly + the child exits with
//!   status 0.
//! - Multi-instance isolation: two simultaneous spawns see
//!   independent env dicts (no cross-contamination).
//! - Malformed JSON-RPC input doesn't kill the dispatch loop —
//!   the mock continues servicing valid frames after a parse
//!   error.
//!
//! These tests run the mock binary directly through
//! `Command::spawn` (no broker, no init-loop). The full
//! daemon-side `factory_registry` walk is unit-tested in
//! `proyecto/src/main.rs::tests::seed_*_subprocess_env_for_*` (8
//! tests) and `crates/core/src/agent/nexo_plugin_registry/subprocess.rs::tests::with_spawn_env_*`
//! (3 tests); this e2e suite covers the actual fork/spawn/JSON-RPC
//! round-trip the unit tests can't reach.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::serial;

const BINARY: &str = env!("CARGO_BIN_EXE_mock_subprocess_plugin");

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Send a JSON-RPC frame and read one reply line. Times out
/// rather than hanging forever — CI runners can be slow but a
/// 5s window is far past the mock binary's actual response
/// latency (single-digit ms).
fn rpc_round_trip(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut BufReader<std::process::ChildStdout>,
    frame: Value,
) -> Value {
    let line = serde_json::to_string(&frame).expect("frame serialises");
    stdin
        .write_all(line.as_bytes())
        .expect("write request line");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush stdin");

    let mut buf = String::new();
    let started = Instant::now();
    loop {
        if started.elapsed() > HANDSHAKE_TIMEOUT {
            panic!(
                "mock subprocess plugin: no reply within {HANDSHAKE_TIMEOUT:?} for frame {line}",
            );
        }
        match stdout.read_line(&mut buf) {
            Ok(0) => panic!("mock subprocess plugin: stdout EOF before reply"),
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(e) => panic!("mock subprocess plugin: read_line error: {e}"),
        }
    }
    serde_json::from_str(buf.trim())
        .unwrap_or_else(|e| panic!("mock subprocess plugin: reply not JSON: {e} (raw: {buf:?})"))
}

/// Spawn the mock binary with a fully-cleared env (mirrors
/// `SubprocessNexoPlugin::spawn_one_attempt`'s
/// `Command::env_clear().envs(&map)` path) and return the child
/// + stdin/stdout handles for direct JSON-RPC poking.
fn spawn_with_env_clear(env: &[(&str, &str)]) -> std::process::Child {
    let mut cmd = Command::new(BINARY);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.spawn().expect("spawn mock subprocess plugin")
}

fn drain_child(mut child: std::process::Child) -> (std::process::ExitStatus, String) {
    let stderr = child.stderr.take();
    let status = match child.try_wait() {
        Ok(Some(s)) => s,
        _ => {
            let started = Instant::now();
            loop {
                if started.elapsed() > SHUTDOWN_TIMEOUT {
                    let _ = child.kill();
                    break child.wait().expect("kill+wait");
                }
                if let Ok(Some(s)) = child.try_wait() {
                    break s;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    let stderr_text = stderr
        .map(|mut s| {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut s, &mut buf).ok();
            buf
        })
        .unwrap_or_default();
    (status, stderr_text)
}

#[test]
#[serial]
fn initialize_replies_with_manifest_and_server_version() {
    let mut child = spawn_with_env_clear(&[("PATH", "/usr/bin")]);
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    let reply = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }),
    );

    assert_eq!(reply["jsonrpc"], "2.0", "got: {reply:?}");
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["server_version"], "mock-0.0.1");
    assert_eq!(
        reply["result"]["manifest"]["plugin"]["id"], "mock_subprocess_plugin",
        "manifest id should match the bundled toml; got: {reply:?}",
    );
    assert!(
        reply["result"]["tools"]
            .as_array()
            .is_some_and(|a| a.is_empty()),
        "mock binary advertises zero tools by design",
    );

    // Shutdown — verify clean exit.
    let shutdown = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    assert_eq!(shutdown["result"]["ok"], true);
    drop(stdin);
    let (status, _stderr) = drain_child(child);
    assert!(
        status.success(),
        "mock should exit clean after shutdown; status={status:?}"
    );
}

#[test]
#[serial]
fn env_clear_passes_only_explicit_vars_to_child() {
    // Set a sentinel in the parent's env that should NOT survive
    // `Command::env_clear()`. Pairs with the daemon's
    // `seed_*_subprocess_env_for` defense-in-depth contract that
    // unrelated daemon secrets don't leak into the subprocess.
    std::env::set_var("__NEXO_SUBPROCESS_LEAK_SENTINEL__", "do-not-leak");

    let mut child = spawn_with_env_clear(&[
        ("PATH", "/usr/bin"),
        ("NEXO_PLUGIN_TOKEN", "expected-token"),
        ("MOCK_SUBPROCESS_PLUGIN_ECHO_ENV", "1"),
    ]);
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    let _init = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let _ = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    drop(stdin);

    let (status, stderr) = drain_child(child);
    assert!(
        status.success(),
        "mock should exit clean; status={status:?}"
    );

    // The mock's MOCK_ENV stderr line enumerates every NEXO_PLUGIN_*
    // env var the child actually saw. Sentinel must NOT appear.
    assert!(
        !stderr.contains("__NEXO_SUBPROCESS_LEAK_SENTINEL__"),
        "daemon env should not leak into subprocess; stderr: {stderr}",
    );
    assert!(
        stderr.contains("NEXO_PLUGIN_TOKEN=expected-token"),
        "explicit env var should be visible to child; stderr: {stderr}",
    );

    std::env::remove_var("__NEXO_SUBPROCESS_LEAK_SENTINEL__");
}

#[test]
#[serial]
fn multi_instance_spawns_have_independent_env() {
    // Spawn two mocks simultaneously, each with a distinct
    // NEXO_PLUGIN_INSTANCE marker. Both must echo their own
    // marker — no cross-contamination, no shared global state.

    let mut child_a = spawn_with_env_clear(&[
        ("PATH", "/usr/bin"),
        ("NEXO_PLUGIN_INSTANCE", "bot_a"),
        ("MOCK_SUBPROCESS_PLUGIN_ECHO_ENV", "1"),
    ]);
    let mut child_b = spawn_with_env_clear(&[
        ("PATH", "/usr/bin"),
        ("NEXO_PLUGIN_INSTANCE", "bot_b"),
        ("MOCK_SUBPROCESS_PLUGIN_ECHO_ENV", "1"),
    ]);

    let mut stdin_a = child_a.stdin.take().expect("stdin a");
    let mut stdout_a = BufReader::new(child_a.stdout.take().expect("stdout a"));
    let mut stdin_b = child_b.stdin.take().expect("stdin b");
    let mut stdout_b = BufReader::new(child_b.stdout.take().expect("stdout b"));

    // Initialize both — interleaved so a global-state bug would
    // show up as a's reply leaking into b's stream.
    let init_a = rpc_round_trip(
        &mut stdin_a,
        &mut stdout_a,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let init_b = rpc_round_trip(
        &mut stdin_b,
        &mut stdout_b,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    assert_eq!(init_a["id"], 1);
    assert_eq!(init_b["id"], 1);

    let _ = rpc_round_trip(
        &mut stdin_a,
        &mut stdout_a,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    let _ = rpc_round_trip(
        &mut stdin_b,
        &mut stdout_b,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    drop(stdin_a);
    drop(stdin_b);

    let (status_a, stderr_a) = drain_child(child_a);
    let (status_b, stderr_b) = drain_child(child_b);
    assert!(
        status_a.success() && status_b.success(),
        "both mocks should exit clean"
    );

    assert!(
        stderr_a.contains("NEXO_PLUGIN_INSTANCE=bot_a")
            && !stderr_a.contains("NEXO_PLUGIN_INSTANCE=bot_b"),
        "instance a env isolation broken; stderr: {stderr_a}",
    );
    assert!(
        stderr_b.contains("NEXO_PLUGIN_INSTANCE=bot_b")
            && !stderr_b.contains("NEXO_PLUGIN_INSTANCE=bot_a"),
        "instance b env isolation broken; stderr: {stderr_b}",
    );
}

#[test]
#[serial]
fn unknown_method_returns_jsonrpc_error_without_killing_loop() {
    let mut child = spawn_with_env_clear(&[("PATH", "/usr/bin")]);
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    // 1. Bogus method.
    let err_reply = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":42,"method":"does.not.exist","params":{}}),
    );
    assert_eq!(err_reply["id"], 42);
    assert_eq!(err_reply["error"]["code"], -32601);
    assert!(
        err_reply["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("not supported"),
        "got: {err_reply:?}",
    );

    // 2. The dispatch loop MUST keep working — verify with a
    // valid initialize after the error frame.
    let ok_reply = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":43,"method":"initialize","params":{}}),
    );
    assert_eq!(ok_reply["id"], 43);
    assert_eq!(ok_reply["result"]["server_version"], "mock-0.0.1");

    let _ = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    drop(stdin);
    let (status, _) = drain_child(child);
    assert!(
        status.success(),
        "mock should still exit clean after error frame"
    );
}

#[test]
#[serial]
fn malformed_json_input_does_not_kill_dispatch_loop() {
    let mut child = spawn_with_env_clear(&[("PATH", "/usr/bin")]);
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    // Garbage line — mock logs MOCK_ERR to stderr but keeps the
    // loop alive (no panic, no exit).
    stdin
        .write_all(b"this is not json\n")
        .expect("write garbage");
    stdin.flush().expect("flush");

    // Valid frame after garbage — should still get a reply.
    let ok = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}),
    );
    assert_eq!(ok["id"], 7);

    let _ = rpc_round_trip(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    drop(stdin);
    let (status, stderr) = drain_child(child);
    assert!(status.success(), "mock should still exit clean");
    assert!(
        stderr.contains("MOCK_ERR: bad json"),
        "garbage frame should produce stderr trace; got: {stderr}",
    );
}
