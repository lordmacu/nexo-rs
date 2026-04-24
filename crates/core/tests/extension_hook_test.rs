//! Phase 11.6 — end-to-end: echo_ext declares hook subscriptions, agent
//! registers them in HookRegistry, firing routes through the extension.

use std::path::PathBuf;
use std::sync::Arc;

use agent_core::agent::{ExtensionHook, HookOutcome, HookRegistry};
use agent_extensions::{ExtensionManifest, StdioRuntime};
use serde_json::json;

fn echo_ext_path() -> PathBuf {
    use std::sync::OnceLock;

    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
                        "agent-extensions",
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
        "echo_ext example not built; run `cargo build --example echo_ext -p agent-extensions`. \
         path={}",
        path.display()
    );
    let toml_src = format!(
        r#"
[plugin]
id = "echo-hooks"
version = "0.1.0"

[capabilities]
tools = ["echo"]
hooks = ["before_tool_call"]

[transport]
kind = "stdio"
command = "{}"
"#,
        path.display()
    );
    ExtensionManifest::from_str(&toml_src).expect("manifest parses")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_hook_fires_and_continues() {
    let manifest = manifest_for_echo();
    let rt = StdioRuntime::spawn(&manifest, std::env::temp_dir())
        .await
        .expect("spawn");
    let rt = Arc::new(rt);

    // Sanity: handshake advertises the hooks.
    assert!(rt.handshake().hooks.iter().any(|h| h == "before_tool_call"));

    let registry = HookRegistry::new();
    registry.register(
        "before_tool_call",
        manifest.id(),
        ExtensionHook::new(manifest.id(), Arc::clone(&rt)),
    );

    let outcome = registry
        .fire("before_tool_call", json!({"agent_id": "kate", "tool_name": "memory"}))
        .await;
    assert_eq!(outcome, HookOutcome::Continue);

    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_hook_aborts_when_event_requests() {
    let manifest = manifest_for_echo();
    let rt = StdioRuntime::spawn(&manifest, std::env::temp_dir())
        .await
        .expect("spawn");
    let rt = Arc::new(rt);

    let registry = HookRegistry::new();
    registry.register(
        "before_tool_call",
        manifest.id(),
        ExtensionHook::new(manifest.id(), Arc::clone(&rt)),
    );

    let outcome = registry
        .fire(
            "before_tool_call",
            json!({"agent_id": "kate", "tool_name": "memory", "test_abort": true}),
        )
        .await;
    match outcome {
        HookOutcome::Aborted { plugin_id, reason } => {
            assert_eq!(plugin_id, "echo-hooks");
            assert_eq!(reason.as_deref(), Some("test abort"));
        }
        HookOutcome::Continue => panic!("expected abort"),
    }

    rt.shutdown().await;
}
