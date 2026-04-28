//! Unit tests for the inbound bridge. We drive `bridge_step` directly
//! so the tests don't need a real `wa-agent` session — only a broker.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_config::{
    WhatsappAclConfig, WhatsappBehaviorConfig, WhatsappBridgeConfig, WhatsappDaemonConfig,
    WhatsappPluginConfig, WhatsappRateLimitConfig, WhatsappTranscriberConfig,
};
use nexo_plugin_whatsapp::bridge::{bridge_step, TOPIC_INBOUND};
use nexo_plugin_whatsapp::events::InboundEvent;
use nexo_plugin_whatsapp::session_id::session_id_for_jid;

fn cfg(timeout_ms: u64) -> WhatsappPluginConfig {
    WhatsappPluginConfig {
        enabled: true,
        session_dir: "/tmp/x".into(),
        media_dir: "/tmp/y".into(),
        credentials_file: None,
        acl: WhatsappAclConfig::default(),
        behavior: WhatsappBehaviorConfig::default(),
        rate_limit: WhatsappRateLimitConfig::default(),
        bridge: WhatsappBridgeConfig {
            response_timeout_ms: timeout_ms,
            on_timeout: "noop".into(),
            apology_text: "sorry".into(),
        },
        transcriber: WhatsappTranscriberConfig::default(),
        daemon: WhatsappDaemonConfig::default(),
        public_tunnel: Default::default(),
        instance: None,
        allow_agents: Vec::new(),
    }
}

fn sample_event(jid: &str) -> InboundEvent {
    InboundEvent::Message {
        from: jid.into(),
        chat: jid.into(),
        text: Some("hola".into()),
        reply_to: None,
        reply_to_question_id: None,
        ask_question_id: None,
        is_group: false,
        timestamp: 0,
        msg_id: "M1".into(),
    }
}

#[tokio::test]
async fn bridge_resolves_when_outbound_arrives() {
    let broker = AnyBroker::local();
    let pending: Arc<DashMap<uuid::Uuid, tokio::sync::oneshot::Sender<String>>> =
        Arc::new(DashMap::new());
    let cfg = cfg(5_000);
    let jid = "573111111111@s.whatsapp.net";
    let sid = session_id_for_jid(jid);

    // Simulated core: subscribe to inbound, then resolve the oneshot as
    // soon as the plugin publishes.
    let pending_for_core = pending.clone();
    let mut sub = broker.subscribe(TOPIC_INBOUND).await.unwrap();
    tokio::spawn(async move {
        while let Some(ev) = sub.next().await {
            if let Some(sid) = ev.session_id {
                if let Some((_, tx)) = pending_for_core.remove(&sid) {
                    let _ = tx.send("hi back".to_string());
                    return;
                }
            }
        }
    });

    let got = bridge_step(&broker, &pending, &cfg, sid, sample_event(jid)).await;
    assert_eq!(got.as_deref(), Some("hi back"));
    assert!(pending.is_empty(), "pending drained on success");
}

#[tokio::test]
async fn bridge_times_out_and_publishes_timeout_event() {
    let broker = AnyBroker::local();
    let pending: Arc<DashMap<uuid::Uuid, tokio::sync::oneshot::Sender<String>>> =
        Arc::new(DashMap::new());
    let cfg = cfg(50);
    let jid = "573222222222@s.whatsapp.net";
    let sid = session_id_for_jid(jid);

    // Subscribe first so we can assert both the regular inbound and the
    // timeout event reach the topic.
    let mut sub = broker.subscribe(TOPIC_INBOUND).await.unwrap();
    let collector = tokio::spawn(async move {
        let mut events: Vec<Event> = Vec::new();
        while events.len() < 2 {
            match tokio::time::timeout(Duration::from_secs(1), sub.next()).await {
                Ok(Some(ev)) => events.push(ev),
                _ => break,
            }
        }
        events
    });

    let got = bridge_step(&broker, &pending, &cfg, sid, sample_event(jid)).await;
    assert!(got.is_none(), "timeout returns None");
    assert!(pending.is_empty(), "pending drained on timeout");

    let events = collector.await.unwrap();
    assert_eq!(events.len(), 2, "two events published (message + timeout)");
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e.payload.get("kind").and_then(|v| v.as_str()).unwrap_or(""))
        .collect();
    assert!(kinds.contains(&"message"));
    assert!(kinds.contains(&"bridge_timeout"));
}

#[tokio::test]
async fn newer_inbound_supersedes_waiting_sender() {
    let broker = AnyBroker::local();
    let pending: Arc<DashMap<uuid::Uuid, tokio::sync::oneshot::Sender<String>>> =
        Arc::new(DashMap::new());
    let cfg = cfg(1_000);
    let jid = "573333333333@s.whatsapp.net";
    let sid = session_id_for_jid(jid);

    // Consume inbound events silently so publish doesn't back up.
    let mut sub = broker.subscribe(TOPIC_INBOUND).await.unwrap();
    tokio::spawn(async move { while sub.next().await.is_some() {} });

    let pending_for_second = pending.clone();
    let broker_for_second = broker.clone();
    let cfg_for_second = cfg.clone();

    // Launch first bridge_step — it installs a sender and awaits.
    let first = tokio::spawn({
        let b = broker.clone();
        let p = pending.clone();
        let c = cfg.clone();
        let ev = sample_event(jid);
        async move { bridge_step(&b, &p, &c, sid, ev).await }
    });

    // Give it a moment to insert.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Second inbound on same session replaces the oneshot — first must
    // resolve to None (sender dropped).
    let second = tokio::spawn(async move {
        bridge_step(
            &broker_for_second,
            &pending_for_second,
            &cfg_for_second,
            sid,
            sample_event(jid),
        )
        .await
    });

    let first_out = first.await.unwrap();
    assert!(first_out.is_none(), "superseded sender yields None");
    // Don't block on second (would time out — no resolver); drop it.
    second.abort();
}
