//! Phase 48 follow-up #3 (partial — in-process). True greenmail
//! e2e against a real IMAP/SMTP server still requires Docker
//! compose in CI; this harness covers everything from
//! `OutboundDispatcher::enqueue_for_instance` down through the
//! JSONL queue + Message-ID idempotency and from a synthetic
//! `.eml` blob up through `parse_eml` + `resolve_thread_root` +
//! the bounce-warning loop without dialling out.
//!
//! What this *does not* exercise: the actual SMTP `DATA` round-
//! trip and IMAP IDLE / FETCH / MOVE wire calls. Those land
//! when greenmail joins CI.

use std::sync::Arc;
use std::time::Duration;

use nexo_broker::AnyBroker;
use nexo_config::types::plugins::EmailPluginConfigFile;
use nexo_plugin_email::{
    bounce_store::BounceStore,
    dsn::{BounceClassification, BounceEvent},
    events::{AddressEntry, EmailMeta, OutboundCommand},
    mime_parse::{parse_eml, ParseConfig},
    threading::{enrich_reply_threading, resolve_thread_root, session_id_for_thread},
    OutboundDispatcher,
};

use std::collections::BTreeMap;

fn cfg(declared: &[(&str, &str)]) -> nexo_config::types::plugins::EmailPluginConfig {
    let mut yaml = String::from("email:\n  accounts:\n");
    for (instance, address) in declared {
        yaml.push_str(&format!(
            "    - instance: {instance}\n      address: {address}\n      imap: {{ host: imap.example.com, port: 993 }}\n      smtp: {{ host: smtp.example.com, port: 587 }}\n"
        ));
    }
    let f: EmailPluginConfigFile = serde_yaml::from_str(&yaml).unwrap();
    f.email
}

fn empty_meta(from_address: &str) -> EmailMeta {
    EmailMeta {
        message_id: None,
        in_reply_to: None,
        references: vec![],
        from: AddressEntry {
            address: from_address.into(),
            name: None,
        },
        to: vec![],
        cc: vec![],
        subject: String::new(),
        body_text: String::new(),
        body_html: None,
        date: 0,
        headers_extra: BTreeMap::new(),
        body_truncated: false,
    }
}

#[tokio::test]
async fn outbound_enqueue_through_dispatcher_returns_message_id() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_cfg = cfg(&[("ops", "ops@example.com")]);
    let broker = AnyBroker::local();
    let creds = Arc::new(nexo_auth::email::EmailCredentialStore::empty());
    let google = Arc::new(nexo_auth::google::GoogleCredentialStore::empty());
    let health: nexo_plugin_email::inbound::HealthMap = Arc::new(dashmap::DashMap::new());

    let dispatcher =
        OutboundDispatcher::start(&plugin_cfg, creds, google, broker, tmp.path(), health)
            .await
            .unwrap();

    let cmd = OutboundCommand {
        to: vec!["alice@x".into()],
        cc: vec![],
        bcc: vec![],
        subject: "Hi".into(),
        body: "hello".into(),
        in_reply_to: None,
        references: vec![],
        attachments: vec![],
    };
    let msg_id = dispatcher.enqueue_for_instance("ops", cmd).await.unwrap();
    assert!(msg_id.starts_with('<') && msg_id.ends_with('>'));
    assert!(
        msg_id.contains("@example.com"),
        "message_id should embed the From domain: {msg_id}"
    );

    let queue_path = tmp.path().join("email").join("outbound").join("ops.jsonl");
    let body = std::fs::read_to_string(&queue_path).unwrap();
    assert_eq!(body.lines().filter(|l| !l.is_empty()).count(), 1);
    assert!(body.contains(&msg_id));

    tokio::time::timeout(Duration::from_secs(2), dispatcher.stop())
        .await
        .unwrap();
}

#[tokio::test]
async fn enqueue_unknown_instance_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_cfg = cfg(&[("ops", "ops@example.com")]);
    let broker = AnyBroker::local();
    let creds = Arc::new(nexo_auth::email::EmailCredentialStore::empty());
    let google = Arc::new(nexo_auth::google::GoogleCredentialStore::empty());
    let health: nexo_plugin_email::inbound::HealthMap = Arc::new(dashmap::DashMap::new());
    let dispatcher =
        OutboundDispatcher::start(&plugin_cfg, creds, google, broker, tmp.path(), health)
            .await
            .unwrap();

    let cmd = OutboundCommand {
        to: vec!["alice@x".into()],
        cc: vec![],
        bcc: vec![],
        subject: "Hi".into(),
        body: "ok".into(),
        in_reply_to: None,
        references: vec![],
        attachments: vec![],
    };
    let err = dispatcher
        .enqueue_for_instance("ghost", cmd)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown email instance"));
    dispatcher.stop().await;
}

#[tokio::test]
async fn parse_then_threading_then_session_id_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let raw = b"From: alice@example.com\r\n\
To: ops@example.com\r\n\
Subject: Re: report\r\n\
Message-ID: <reply@example.com>\r\n\
In-Reply-To: <root@example.com>\r\n\
References: <root@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
yes\r\n";
    let cfg = ParseConfig {
        max_body_bytes: 4096,
        max_attachment_bytes: 4096,
        attachments_dir: dir.path().to_path_buf(),
        fallback_internal_date: 0,
    };
    let parsed = parse_eml(raw, &cfg).await.unwrap();
    let root = resolve_thread_root(&parsed.meta, 1, "ops@example.com");
    assert_eq!(root, "<root@example.com>");
    let id_a = session_id_for_thread(&root);
    let id_b = session_id_for_thread(&root);
    assert_eq!(id_a, id_b);

    let mut cmd = OutboundCommand {
        to: vec!["alice@example.com".into()],
        cc: vec![],
        bcc: vec![],
        subject: "Re: report".into(),
        body: "thanks".into(),
        in_reply_to: None,
        references: vec![],
        attachments: vec![],
    };
    enrich_reply_threading(&parsed.meta, &mut cmd);
    assert_eq!(cmd.in_reply_to.as_deref(), Some("reply@example.com"));
    assert!(
        cmd.references
            .iter()
            .any(|r| r.contains("root@example.com")),
        "got {:?}",
        cmd.references
    );
    assert!(
        cmd.references
            .iter()
            .any(|r| r.contains("reply@example.com")),
        "got {:?}",
        cmd.references
    );
}

#[tokio::test]
async fn bounce_event_persists_and_increments_count() {
    let tmp = tempfile::tempdir().unwrap();
    let bounce_path = tmp.path().join("bounces.db");
    let bounce_store = BounceStore::open_path(&bounce_path).await.unwrap();

    let event = BounceEvent {
        account_id: "ops@example.com".into(),
        instance: "ops".into(),
        original_message_id: Some("<orig@x>".into()),
        recipient: Some("ghost@x.com".into()),
        status_code: Some("5.1.1".into()),
        action: Some("failed".into()),
        reason: Some("user unknown".into()),
        classification: BounceClassification::Permanent,
    };
    bounce_store.record(&event).await.unwrap();

    let status = bounce_store
        .get("ops", "ghost@x.com")
        .await
        .unwrap()
        .expect("recorded bounce");
    assert_eq!(status.classification, BounceClassification::Permanent);
    assert_eq!(status.count, 1);
    assert_eq!(status.status_code.as_deref(), Some("5.1.1"));

    bounce_store.record(&event).await.unwrap();
    let status = bounce_store
        .get("ops", "ghost@x.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.count, 2);
}

#[tokio::test]
async fn synthetic_self_bounce_skips_via_loop_prevention() {
    use nexo_config::types::plugins::LoopPreventionCfg;
    let mut meta = empty_meta("ops@example.com");
    let cfg = LoopPreventionCfg::default();
    let reason = nexo_plugin_email::loop_prevent::should_skip(&meta, "ops@example.com", &cfg);
    assert!(reason.is_some(), "self-from must trigger SkipReason");
    meta.from.address = "alice@example.com".into();
    assert!(nexo_plugin_email::loop_prevent::should_skip(&meta, "ops@example.com", &cfg).is_none());
}
