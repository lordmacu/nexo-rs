//! Item 4: under Strict mode the legacy inline `agents.google_auth`
//! block is rejected outright. Lenient keeps the warn + migration.

use std::path::PathBuf;

use nexo_auth::error::BuildError;
use nexo_auth::resolver::StrictLevel;
use nexo_auth::wire::build_credentials;
use nexo_config::types::agents::{
    AgentConfig, GoogleAuthAgentConfig, HeartbeatConfig, ModelConfig, OutboundAllowlistConfig,
};
use nexo_config::types::credentials::{AgentCredentialsConfig, GoogleAuthConfig};

fn agent_with_inline_google(id: &str) -> AgentConfig {
    AgentConfig {
        id: id.into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "stub".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: Default::default(),
        system_prompt: String::new(),
        workspace: String::new(),
        skills: vec![],
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: vec![],
        inbound_bindings: vec![],
        allowed_tools: vec![],
        sender_rate_limit: None,
        allowed_delegates: vec![],
        accept_delegates_from: vec![],
        description: String::new(),
        google_auth: Some(GoogleAuthAgentConfig {
            client_id: "706186208439-secret".into(),
            client_secret: "gocspx-literal".into(),
            scopes: vec![],
            token_file: "google_tokens.json".into(),
            redirect_port: 8765,
        }),
        outbound_allowlist: OutboundAllowlistConfig::default(),
        credentials: AgentCredentialsConfig::default(),
        language: None,
        context_optimization: None,
        dispatch_policy: Default::default(),
    }
}

#[test]
fn strict_rejects_inline_google_auth() {
    let agent = agent_with_inline_google("ana");
    let err = build_credentials(
        &[agent],
        &[],
        &[],
        &GoogleAuthConfig::default(),
        StrictLevel::Strict,
    )
    .unwrap_err();
    assert!(
        err.iter()
            .any(|e| matches!(e, BuildError::LegacyInlineGoogleAuth { agent } if agent == "ana")),
        "expected LegacyInlineGoogleAuth for ana: {err:#?}"
    );
}

#[test]
fn lenient_accepts_inline_google_auth_with_warning() {
    let agent = agent_with_inline_google("ana");
    let bundle = build_credentials(
        &[agent],
        &[],
        &[],
        &GoogleAuthConfig::default(),
        StrictLevel::Lenient,
    )
    .expect("lenient must not hard-fail");
    assert!(
        bundle
            .warnings
            .iter()
            .any(|w| w.contains("inline google_auth is deprecated")),
        "expected deprecation warn: {:?}",
        bundle.warnings
    );
}

#[test]
fn strict_error_never_leaks_client_id() {
    let agent = agent_with_inline_google("ana");
    let err = build_credentials(
        &[agent],
        &[],
        &[],
        &GoogleAuthConfig::default(),
        StrictLevel::Strict,
    )
    .unwrap_err();
    for e in &err {
        let msg = e.to_string();
        assert!(
            !msg.contains("706186208439"),
            "error leaked client_id: {msg}"
        );
        assert!(
            !msg.contains("gocspx-literal"),
            "error leaked secret: {msg}"
        );
    }
    // Silence unused import warning in strict mode for PathBuf.
    let _ = PathBuf::new();
}
