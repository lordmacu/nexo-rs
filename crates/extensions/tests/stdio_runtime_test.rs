//! Phase 11.3 — stdio extension runtime integration tests. Spawns the
//! companion `echo_ext` example binary and exercises handshake + tools/call.

use std::path::PathBuf;
use std::time::Duration;

use nexo_extensions::{ExtensionManifest, StdioRuntime, StdioSpawnOptions};

/// Locate `target/debug/examples/echo_ext` relative to this crate.
/// Builds it on demand the first time any test needs it, so `cargo test`
/// alone is sufficient — no out-of-band `cargo build --example echo_ext`.
fn echo_ext_path() -> PathBuf {
    use std::sync::OnceLock;
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let target = manifest_dir
                .join("..")
                .join("..")
                .join("target")
                .join("debug")
                .join("examples")
                .join("echo_ext");
            if !target.exists() {
                let status = std::process::Command::new(env!("CARGO"))
                    .args([
                        "build",
                        "--quiet",
                        "-p",
                        "nexo-extensions",
                        "--example",
                        "echo_ext",
                    ])
                    .status()
                    .expect("spawn cargo build --example echo_ext");
                assert!(status.success(), "cargo build --example echo_ext failed");
            }
            target
        })
        .clone()
}

fn manifest_for_echo() -> ExtensionManifest {
    let path = echo_ext_path();
    assert!(
        path.exists(),
        "echo_ext example not built; run `cargo build --example echo_ext -p nexo-extensions` before tests. path={}",
        path.display()
    );
    let toml_src = format!(
        r#"
[plugin]
id = "echo-test"
version = "0.1.0"

[capabilities]
tools = ["echo"]

[transport]
kind = "stdio"
command = "{}"
"#,
        path.display()
    );
    ExtensionManifest::from_str(&toml_src).expect("manifest parses")
}

fn manifest_with_command(command: &str) -> ExtensionManifest {
    let toml_src = format!(
        r#"
[plugin]
id = "custom-test"
version = "0.1.0"

[capabilities]
tools = ["x"]

[transport]
kind = "stdio"
command = "{command}"
"#
    );
    ExtensionManifest::from_str(&toml_src).expect("manifest parses")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_lists_echo_tool() {
    let m = manifest_for_echo();
    let rt = StdioRuntime::spawn(&m, std::env::temp_dir()).await.unwrap();
    let hs = rt.handshake();
    assert_eq!(hs.tools.len(), 1);
    assert_eq!(hs.tools[0].name, "echo");
    assert_eq!(hs.server_version.as_deref(), Some("echo-0.1.0"));
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_call_roundtrip() {
    let m = manifest_for_echo();
    let rt = StdioRuntime::spawn(&m, std::env::temp_dir()).await.unwrap();
    let out = rt
        .tools_call("echo", serde_json::json!({"x": 1, "y": "hola"}))
        .await
        .expect("call ok");
    assert_eq!(out["echoed"]["x"], 1);
    assert_eq!(out["echoed"]["y"], "hola");
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_times_out_on_hang() {
    // `sleep 30` never answers JSON-RPC. Handshake itself should time out.
    let m = manifest_with_command("/bin/sh");
    // Give a tight handshake timeout so the test is fast.
    let opts = StdioSpawnOptions {
        cwd: std::env::temp_dir(),
        handshake_timeout: Duration::from_millis(200),
        ..Default::default()
    };
    // Override args to make /bin/sh read stdin forever but never respond.
    // Since StdioSpawnOptions doesn't expose args, build a tiny wrapper.
    // We rewrite the manifest to use sh -c "cat > /dev/null" so it reads
    // stdin silently.
    let _ = m;
    let toml_src = r#"
[plugin]
id = "hang"
version = "0.1.0"

[capabilities]
tools = ["x"]

[transport]
kind = "stdio"
command = "/bin/sh"
args = ["-c", "cat > /dev/null"]
"#;
    let m = ExtensionManifest::from_str(toml_src).unwrap();
    let result = StdioRuntime::spawn_with(&m, opts).await;
    match result {
        Err(nexo_extensions::StartError::HandshakeTimeout(_)) => {}
        Err(other) => panic!("expected handshake timeout, got {other:?}"),
        Ok(_) => panic!("expected failure, got Ok"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_is_clean() {
    let m = manifest_for_echo();
    let rt = StdioRuntime::spawn(&m, std::env::temp_dir()).await.unwrap();
    // Just ensure shutdown completes without panicking.
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_fails_for_missing_binary() {
    let m = manifest_with_command("/definitely/not/a/real/path-xyz");
    let result = StdioRuntime::spawn(&m, std::env::temp_dir()).await;
    match result {
        Err(nexo_extensions::StartError::Spawn(_)) => {}
        Err(other) => panic!("expected Spawn error, got {other:?}"),
        Ok(_) => panic!("expected failure, got Ok"),
    }
}
