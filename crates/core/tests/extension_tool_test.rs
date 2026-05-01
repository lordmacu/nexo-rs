//! Phase 11.5 — end-to-end: spawn `echo_ext`, wrap in Arc, register in
//! `ToolRegistry` via `ExtensionTool`, dispatch a call through the registry.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexo_broker::AnyBroker;
use nexo_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use nexo_core::agent::{AgentContext, ExtensionTool, ToolHandler, ToolRegistry};
use nexo_core::session::SessionManager;
use nexo_extensions::{ExtensionManifest, StdioRuntime};
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
        "echo_ext example not built; run `cargo build --example echo_ext -p nexo-extensions`. \
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
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
        context_optimization: None,
        dispatch_policy: Default::default(),
        plan_mode: Default::default(),
        remote_triggers: Vec::new(),
        lsp: nexo_config::types::lsp::LspPolicy::default(),
        config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
        team: nexo_config::types::team::TeamPolicy::default(),
        proactive: Default::default(),
        repl: Default::default(),
        auto_dream: None,
        assistant_mode: None,
        away_summary: None,
        brief: None,
        channels: None,
        auto_approve: false,
        extract_memories: None,
            event_subscribers: Vec::new(),
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

// ───────────────────────────────────────────────────────────────────
// Phase 82.1 Step 8 — e2e BindingContext propagation
// ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_tool_passthrough_emits_nexo_binding_block() {
    use nexo_core::agent::context::BindingContext;
    use uuid::Uuid;

    let manifest = manifest_for_echo();
    let rt = StdioRuntime::spawn(&manifest, std::env::temp_dir())
        .await
        .expect("spawn");
    let rt = Arc::new(rt);

    // Build the ExtensionTool with passthrough ON — same wiring an
    // operator gets via `[context] passthrough = true` in
    // `plugin.toml` (Phase 11.5 follow-up).
    let pid = manifest.id();
    let desc = rt.handshake().tools[0].clone();
    let handler = ExtensionTool::new(pid, desc.name.clone(), Arc::clone(&rt))
        .with_context_passthrough(true);

    // Simulate what the runtime intake does when an inbound message
    // matches a Phase 17 binding: AgentContext gets a populated
    // BindingContext carrying (channel, account_id, binding_id).
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let session = Uuid::nil();
    let mut ctx = AgentContext::new("ana", agent_cfg(), broker, sessions).with_session_id(session);
    let mut binding = BindingContext::agent_only("ana");
    binding.session_id = Some(session);
    binding.channel = Some("whatsapp".into());
    binding.account_id = Some("personal".into());
    binding.binding_id = Some("whatsapp:personal".into());
    ctx.binding = Some(binding);

    let result = handler
        .call(&ctx, json!({"to": "+5491100", "body": "hola"}))
        .await
        .expect("call ok");

    let echoed = &result["echoed"];

    // Original args reach the extension untouched.
    assert_eq!(echoed["to"], "+5491100");
    assert_eq!(echoed["body"], "hola");

    // Legacy flat block (backward-compat surface).
    assert_eq!(echoed["_meta"]["agent_id"], "ana");
    assert_eq!(echoed["_meta"]["session_id"], session.to_string());

    // Nested binding block — Phase 82.1 contract.
    let binding = &echoed["_meta"]["nexo"]["binding"];
    assert_eq!(binding["agent_id"], "ana");
    assert_eq!(binding["channel"], "whatsapp");
    assert_eq!(binding["account_id"], "personal");
    assert_eq!(binding["binding_id"], "whatsapp:personal");
    assert!(binding.get("mcp_channel_source").is_none());

    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_tool_passthrough_off_omits_meta_entirely() {
    let manifest = manifest_for_echo();
    let rt = StdioRuntime::spawn(&manifest, std::env::temp_dir())
        .await
        .expect("spawn");
    let rt = Arc::new(rt);

    // passthrough left at default (false) — no _meta should leak,
    // even when the AgentContext does carry a binding.
    let pid = manifest.id();
    let desc = rt.handshake().tools[0].clone();
    let handler = ExtensionTool::new(pid, desc.name.clone(), Arc::clone(&rt));

    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let mut ctx = AgentContext::new("ana", agent_cfg(), broker, sessions);
    ctx.binding = Some(nexo_core::agent::context::BindingContext::agent_only("ana"));

    let result = handler.call(&ctx, json!({"x": 1})).await.expect("call ok");

    assert_eq!(result["echoed"]["x"], 1);
    // Defense for the deny-unknown-fields case described in the
    // ExtensionContextConfig docstring: extensions with strict
    // schemas must never see an unsolicited _meta key.
    assert!(result["echoed"].get("_meta").is_none());

    rt.shutdown().await;
}
