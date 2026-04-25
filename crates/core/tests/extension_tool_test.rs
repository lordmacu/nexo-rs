//! Phase 11.5 — end-to-end: spawn `echo_ext`, wrap in Arc, register in
//! `ToolRegistry` via `ExtensionTool`, dispatch a call through the registry.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_broker::AnyBroker;
use agent_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use agent_core::agent::{AgentContext, ExtensionTool, ToolRegistry};
use agent_core::session::SessionManager;
use agent_extensions::{ExtensionManifest, StdioRuntime};
use serde_json::json;

fn echo_ext_path() -> PathBuf {
    use std::sync::OnceLock;

    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            // crates/core/.. → workspace root → target/debug/examples/echo_ext
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

fn agent_cfg() -> Arc<AgentConfig> {
    Arc::new(AgentConfig {
        id: "kate".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: String::new(),
        workspace: String::new(),
        skills: vec![],
        skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        inbound_bindings: Vec::new(),
        allowed_tools: Vec::new(),
        sender_rate_limit: None,
        allowed_delegates: Vec::new(),
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: Default::default(),
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
            language: None,
        context_optimization: None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_tool_registers_and_dispatches() {
    let manifest = manifest_for_echo();
    let rt = StdioRuntime::spawn(&manifest, std::env::temp_dir())
        .await
        .expect("spawn");
    let rt = Arc::new(rt);

    // Register the single declared tool with descriptor metadata so
    // hooks / introspection code can read what the extension advertised.
    let registry = ToolRegistry::new();
    for desc in &rt.handshake().tools {
        let pid = manifest.id();
        let def = ExtensionTool::tool_def(desc, pid);
        let handler = ExtensionTool::new(pid, desc.name.clone(), Arc::clone(&rt))
            .with_descriptor_metadata(desc.description.clone(), desc.input_schema.clone());
        assert_eq!(handler.description(), Some(desc.description.as_str()));
        assert_eq!(handler.input_schema(), Some(&desc.input_schema));
        registry.register(def, handler);
    }

    let prefixed = ExtensionTool::prefixed_name("echo-test", "echo");
    let (_def, handler) = registry.get(&prefixed).expect("registered");

    // Build an AgentContext; ExtensionTool doesn't use it but the trait requires it.
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("kate", agent_cfg(), broker, sessions);

    let result = handler
        .call(&ctx, json!({"x": 1, "msg": "hola"}))
        .await
        .expect("call ok");
    assert_eq!(result["echoed"]["x"], 1);
    assert_eq!(result["echoed"]["msg"], "hola");

    rt.shutdown().await;
}
