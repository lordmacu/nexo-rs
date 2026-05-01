//! Phase 82.11 — end-to-end integration test for the agent
//! event firehose.
//!
//! Wires the production stack: `TranscriptWriter` with a
//! redactor + a `BroadcastAgentEventEmitter` from
//! `AdminRpcBootstrap::event_emitter()`, a granted microapp
//! that holds `transcripts_subscribe`, and a captured outbound
//! channel bound via `bind_writer`. Appending a transcript
//! entry with embedded PII must surface as a
//! `nexo/notify/agent_event` JSON-RPC frame on the microapp's
//! outbound queue with the body already redacted.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use nexo_config::types::extensions::{ExtensionEntry, ExtensionsConfig};
use nexo_config::types::transcripts::RedactionConfig;
use nexo_core::agent::redaction::Redactor;
use nexo_core::agent::transcripts::{TranscriptEntry, TranscriptRole, TranscriptWriter};
use nexo_plugin_manifest::manifest::AdminCapabilities;
use nexo_setup::admin_bootstrap::{AdminBootstrapInputs, AdminRpcBootstrap};
use tokio::sync::mpsc;
use uuid::Uuid;

fn empty_extensions_cfg() -> ExtensionsConfig {
    serde_yaml::from_str("enabled: false").expect("yaml")
}

fn extensions_cfg_with_grant(id: &str, caps: &[&str]) -> ExtensionsConfig {
    let mut cfg = empty_extensions_cfg();
    let entry = ExtensionEntry {
        capabilities_grant: caps.iter().map(|s| s.to_string()).collect(),
        allow_external_bind: false,
    };
    cfg.entries.insert(id.to_string(), entry);
    cfg
}

fn admin_caps(required: &[&str], optional: &[&str]) -> AdminCapabilities {
    AdminCapabilities {
        required: required.iter().map(|s| s.to_string()).collect(),
        optional: optional.iter().map(|s| s.to_string()).collect(),
    }
}

fn user_entry(text: &str) -> TranscriptEntry {
    TranscriptEntry {
        timestamp: Utc::now(),
        role: TranscriptRole::User,
        content: text.into(),
        message_id: None,
        source_plugin: "whatsapp".into(),
        sender_id: Some("wa.55".into()),
    }
}

#[tokio::test]
async fn firehose_delivers_redacted_frame_to_subscribed_microapp() {
    // --- 1. Build the bootstrap with a granted microapp ----------------
    let cfg = extensions_cfg_with_grant(
        "agent-creator",
        &["agents_crud", "transcripts_subscribe"],
    );
    let mut manifests: BTreeMap<String, AdminCapabilities> = BTreeMap::new();
    manifests.insert(
        "agent-creator".into(),
        admin_caps(&["agents_crud"], &["transcripts_subscribe"]),
    );
    let dir = tempfile::tempdir().unwrap();
    let bootstrap = AdminRpcBootstrap::build_with_firehose(
        AdminBootstrapInputs {
            config_dir: dir.path(),
            secrets_root: dir.path(),
            audit_db: None,
            extensions_cfg: &cfg,
            admin_capabilities: &manifests,
            http_server_capabilities: &BTreeMap::new(),
            reload_signal: Arc::new(|| {}),
            transcript_reader: None,
        },
        true,
    )
    .await
    .unwrap()
    .expect("admin wires built");
    assert_eq!(bootstrap.subscribe_task_count(), 1);

    // --- 2. Bind a captured outbound mpsc to the granted microapp -----
    // 16 frames is more than enough for the single entry this test
    // appends; bound > 1 so we don't accidentally exercise the lag
    // path here (covered by the Step 4 unit tests).
    let (tx, mut rx) = mpsc::channel::<String>(16);
    bootstrap.bind_writer("agent-creator", tx);

    // --- 3. Wire a TranscriptWriter that fires the same emitter ------
    let cfg = RedactionConfig {
        enabled: true,
        use_builtins: true,
        extra_patterns: Vec::new(),
    };
    let redactor = Arc::new(Redactor::from_config(&cfg).unwrap());
    let writer = TranscriptWriter::with_extras(
        dir.path().join("transcripts"),
        "agent-creator",
        redactor,
        None,
    )
    .with_emitter(bootstrap.event_emitter());

    // --- 4. Append an entry carrying a real-shaped OpenAI key --------
    let session_id = Uuid::new_v4();
    writer
        .append_entry(
            session_id,
            user_entry("leak sk-abcdefghijklmnopqrstuvwx0123 here"),
        )
        .await
        .unwrap();

    // --- 5. The microapp's outbound queue receives one frame ---------
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("frame within 2s")
        .expect("channel open");
    let parsed: serde_json::Value = serde_json::from_str(&frame).expect("valid json");
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["method"], "nexo/notify/agent_event");
    assert!(parsed.get("id").is_none(), "notifications carry no id");
    assert_eq!(parsed["params"]["kind"], "transcript_appended");
    assert_eq!(parsed["params"]["agent_id"], "agent-creator");
    assert_eq!(parsed["params"]["session_id"], session_id.to_string());
    assert_eq!(parsed["params"]["seq"], 0);
    assert_eq!(parsed["params"]["role"], "user");
    let body = parsed["params"]["body"].as_str().unwrap();
    assert!(body.contains("[REDACTED:"), "body is redacted: {body}");
    assert!(!body.contains("sk-abcdef"), "raw secret leaked: {body}");

    // Second append → seq=1, body unchanged (no PII).
    writer
        .append_entry(session_id, user_entry("plain follow-up"))
        .await
        .unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("frame within 2s")
        .expect("channel open");
    let parsed2: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(parsed2["params"]["seq"], 1);
    assert_eq!(parsed2["params"]["body"], "plain follow-up");
}

#[tokio::test]
async fn microapp_without_subscribe_capability_receives_no_frames() {
    // Same bootstrap shape but the microapp has only
    // `agents_crud` — no transcripts_subscribe and no
    // agent_events_subscribe_all. The append should not produce
    // a frame on its outbound queue.
    let cfg = extensions_cfg_with_grant("agent-creator", &["agents_crud"]);
    let mut manifests: BTreeMap<String, AdminCapabilities> = BTreeMap::new();
    manifests.insert(
        "agent-creator".into(),
        admin_caps(&["agents_crud"], &[]),
    );
    let dir = tempfile::tempdir().unwrap();
    let bootstrap = AdminRpcBootstrap::build_with_firehose(
        AdminBootstrapInputs {
            config_dir: dir.path(),
            secrets_root: dir.path(),
            audit_db: None,
            extensions_cfg: &cfg,
            admin_capabilities: &manifests,
            http_server_capabilities: &BTreeMap::new(),
            reload_signal: Arc::new(|| {}),
            transcript_reader: None,
        },
        true,
    )
    .await
    .unwrap()
    .expect("admin wires built");
    assert_eq!(
        bootstrap.subscribe_task_count(),
        0,
        "no subscribe tasks without the capability",
    );

    let (tx, mut rx) = mpsc::channel::<String>(8);
    bootstrap.bind_writer("agent-creator", tx);

    let writer = TranscriptWriter::new(dir.path().join("transcripts"), "agent-creator")
        .with_emitter(bootstrap.event_emitter());
    writer
        .append_entry(Uuid::new_v4(), user_entry("hello"))
        .await
        .unwrap();

    // Brief grace window for any rogue forward; queue must stay
    // empty.
    let timeout = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        timeout.is_err(),
        "no frame should reach a microapp without transcripts_subscribe",
    );
}
