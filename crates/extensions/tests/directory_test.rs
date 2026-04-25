//! Integration tests for Phase 11.4 — `ExtensionDirectory` discovers
//! extensions via NATS announces, tracks them by heartbeat, and removes
//! them on shutdown beacon or heartbeat-lost.

use std::sync::Arc;
use std::time::Duration;

use nexo_broker::{BrokerHandle, Event, LocalBroker, Message};
use nexo_extensions::runtime::announce::{
    AnnounceCapabilities, AnnouncePayload, HeartbeatPayload, ShutdownPayload,
};
use nexo_extensions::runtime::{
    DirectoryEvent, ExtensionDirectory, NatsRuntimeOptions, RemovalReason,
};

/// Answer `initialize` requests so `NatsRuntime::connect` succeeds. Other
/// methods are dropped — this test only drives discovery, not calls.
async fn spawn_handshake_responder(
    broker: Arc<dyn BrokerHandle>,
    subject: String,
) -> tokio::task::JoinHandle<()> {
    let mut sub = broker.subscribe(&subject).await.expect("subscribe");
    let b = broker.clone();
    tokio::spawn(async move {
        while let Some(ev) = sub.next().await {
            let msg: Message = match serde_json::from_value(ev.payload) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let Some(reply_to) = msg.reply_to.clone() else {
                continue;
            };
            let line = match &msg.payload {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let req: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if req.get("method").and_then(|m| m.as_str()) != Some("initialize") {
                continue;
            }
            let id = req.get("id").cloned().unwrap_or(serde_json::json!(null));
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "result": {"server_version": "0", "tools": [], "hooks": []},
                "id": id,
            });
            let reply_payload = Message::new(
                reply_to.clone(),
                serde_json::Value::String(response.to_string()),
            );
            let reply_event = Event::new(
                reply_to.clone(),
                "mock",
                serde_json::to_value(&reply_payload).unwrap(),
            );
            let _ = b.publish(&reply_to, reply_event).await;
        }
    })
}

/// Answer exactly one `initialize` request, then exit. Useful to simulate
/// version-bump failure where v1 is alive but v2 never handshakes.
async fn spawn_one_shot_handshake_responder(
    broker: Arc<dyn BrokerHandle>,
    subject: String,
) -> tokio::task::JoinHandle<()> {
    let mut sub = broker.subscribe(&subject).await.expect("subscribe");
    let b = broker.clone();
    tokio::spawn(async move {
        let Some(ev) = sub.next().await else { return };
        let msg: Message = match serde_json::from_value(ev.payload) {
            Ok(m) => m,
            Err(_) => return,
        };
        let Some(reply_to) = msg.reply_to.clone() else {
            return;
        };
        let line = match &msg.payload {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => return,
        };
        if req.get("method").and_then(|m| m.as_str()) != Some("initialize") {
            return;
        }
        let id = req.get("id").cloned().unwrap_or(serde_json::json!(null));
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "result": {"server_version": "0", "tools": [], "hooks": []},
            "id": id,
        });
        let reply_payload = Message::new(
            reply_to.clone(),
            serde_json::Value::String(response.to_string()),
        );
        let reply_event = Event::new(
            reply_to.clone(),
            "mock",
            serde_json::to_value(&reply_payload).unwrap(),
        );
        let _ = b.publish(&reply_to, reply_event).await;
    })
}

async fn publish_announce(broker: &Arc<dyn BrokerHandle>, id: &str, version: &str) {
    let payload = AnnouncePayload {
        schema_version: 1,
        id: id.into(),
        version: version.into(),
        subject_prefix: "ext".into(),
        manifest_hash: None,
        capabilities: AnnounceCapabilities::default(),
    };
    let ev = Event::new(
        "ext.registry.announce",
        "mock-extension",
        serde_json::to_value(&payload).unwrap(),
    );
    broker.publish("ext.registry.announce", ev).await.unwrap();
}

async fn publish_heartbeat(broker: &Arc<dyn BrokerHandle>, id: &str, version: &str) {
    let payload = HeartbeatPayload {
        id: id.into(),
        version: version.into(),
        uptime_secs: 1,
    };
    let subject = format!("ext.registry.heartbeat.{id}");
    let ev = Event::new(
        &subject,
        "mock-extension",
        serde_json::to_value(&payload).unwrap(),
    );
    broker.publish(&subject, ev).await.unwrap();
}

async fn publish_shutdown(broker: &Arc<dyn BrokerHandle>, id: &str) {
    let payload = ShutdownPayload {
        id: id.into(),
        reason: Some("test".into()),
    };
    let subject = format!("ext.registry.shutdown.{id}");
    let ev = Event::new(
        &subject,
        "mock-extension",
        serde_json::to_value(&payload).unwrap(),
    );
    broker.publish(&subject, ev).await.unwrap();
}

async fn publish_event(broker: &Arc<dyn BrokerHandle>, id: &str, payload: serde_json::Value) {
    let subject = format!("ext.{id}.event");
    let ev = Event::new(&subject, "mock-extension", payload);
    broker.publish(&subject, ev).await.unwrap();
}

fn fast_opts() -> NatsRuntimeOptions {
    NatsRuntimeOptions {
        call_timeout: Duration::from_millis(200),
        handshake_timeout: Duration::from_millis(200),
        heartbeat_interval: Duration::from_millis(80),
        heartbeat_grace_factor: 2,
        shutdown_grace: Duration::from_millis(50),
    }
}

async fn wait_for<F>(
    rx: &mut tokio::sync::mpsc::Receiver<DirectoryEvent>,
    mut pred: F,
) -> DirectoryEvent
where
    F: FnMut(&DirectoryEvent) -> bool,
{
    let fut = async {
        loop {
            let ev = rx.recv().await.expect("directory stream closed");
            if pred(&ev) {
                return ev;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(2), fut)
        .await
        .expect("timeout waiting for directory event")
}

#[tokio::test]
async fn announce_adds_extension_and_list_reflects_it() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    let _mock = spawn_handshake_responder(broker.clone(), "ext.weather.rpc".into()).await;

    let (dir, mut rx) = ExtensionDirectory::spawn(broker.clone(), "ext", fast_opts());
    // Give directory tasks a tick to install subscriptions before publishing.
    tokio::time::sleep(Duration::from_millis(20)).await;
    publish_announce(&broker, "weather", "0.3.1").await;

    let ev = wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;
    match ev {
        DirectoryEvent::Added { id, version, .. } => {
            assert_eq!(id, "weather");
            assert_eq!(version, "0.3.1");
        }
        _ => unreachable!(),
    }

    let entries = dir.list();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "weather");
    assert_eq!(entries[0].version, "0.3.1");

    dir.shutdown().await;
}

#[tokio::test]
async fn heartbeat_lost_removes_extension() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    let _mock = spawn_handshake_responder(broker.clone(), "ext.gone.rpc".into()).await;

    let (dir, mut rx) = ExtensionDirectory::spawn(broker.clone(), "ext", fast_opts());
    tokio::time::sleep(Duration::from_millis(20)).await;
    publish_announce(&broker, "gone", "1.0.0").await;
    wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;

    // Skip heartbeats — grace = 80ms * 2 = 160ms. Wait past it.
    tokio::time::sleep(Duration::from_millis(250)).await;

    let ev = wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Removed { .. })).await;
    match ev {
        DirectoryEvent::Removed { id, reason, .. } => {
            assert_eq!(id, "gone");
            assert!(matches!(reason, RemovalReason::HeartbeatLost));
        }
        _ => unreachable!(),
    }
    assert!(dir.list().is_empty());
    dir.shutdown().await;
}

#[tokio::test]
async fn heartbeat_keeps_extension_alive() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    let _mock = spawn_handshake_responder(broker.clone(), "ext.alive.rpc".into()).await;

    let (dir, mut rx) = ExtensionDirectory::spawn(broker.clone(), "ext", fast_opts());
    tokio::time::sleep(Duration::from_millis(20)).await;
    publish_announce(&broker, "alive", "1.0.0").await;
    wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;

    // Pulse heartbeats under grace window (80ms * 2 = 160ms).
    for _ in 0..6 {
        publish_heartbeat(&broker, "alive", "1.0.0").await;
        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    assert_eq!(dir.list().len(), 1, "alive extension evicted prematurely");
    dir.shutdown().await;
}

#[tokio::test]
async fn shutdown_beacon_removes_extension() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    let _mock = spawn_handshake_responder(broker.clone(), "ext.bye.rpc".into()).await;

    let (dir, mut rx) = ExtensionDirectory::spawn(broker.clone(), "ext", fast_opts());
    tokio::time::sleep(Duration::from_millis(20)).await;
    publish_announce(&broker, "bye", "1.0.0").await;
    wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;

    publish_shutdown(&broker, "bye").await;
    let ev = wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Removed { .. })).await;
    match ev {
        DirectoryEvent::Removed { id, reason, .. } => {
            assert_eq!(id, "bye");
            assert!(matches!(reason, RemovalReason::Shutdown { .. }));
        }
        _ => unreachable!(),
    }
    dir.shutdown().await;
}

#[tokio::test]
async fn version_bump_replaces_runtime() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    let _mock = spawn_handshake_responder(broker.clone(), "ext.vbump.rpc".into()).await;

    let (dir, mut rx) = ExtensionDirectory::spawn(broker.clone(), "ext", fast_opts());
    tokio::time::sleep(Duration::from_millis(20)).await;
    publish_announce(&broker, "vbump", "1.0.0").await;
    wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;

    publish_announce(&broker, "vbump", "1.1.0").await;
    let removed = wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Removed { .. })).await;
    match removed {
        DirectoryEvent::Removed {
            id,
            version,
            reason,
        } => {
            assert_eq!(id, "vbump");
            assert_eq!(version, "1.0.0");
            assert!(matches!(reason, RemovalReason::Announced { .. }));
        }
        _ => unreachable!(),
    }
    let added = wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;
    match added {
        DirectoryEvent::Added { version, .. } => assert_eq!(version, "1.1.0"),
        _ => unreachable!(),
    }

    let entries = dir.list();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].version, "1.1.0");
    dir.shutdown().await;
}

#[tokio::test]
async fn failed_version_bump_keeps_previous_runtime_live() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    let _mock = spawn_one_shot_handshake_responder(broker.clone(), "ext.sticky.rpc".into()).await;

    let mut opts = fast_opts();
    // Avoid liveness sweep eviction while asserting the failed bump behavior.
    opts.heartbeat_interval = Duration::from_secs(5);
    opts.heartbeat_grace_factor = 2;
    let (dir, mut rx) = ExtensionDirectory::spawn(broker.clone(), "ext", opts);

    tokio::time::sleep(Duration::from_millis(20)).await;
    publish_announce(&broker, "sticky", "1.0.0").await;
    wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;

    // Second announce cannot connect (one-shot handshake responder already exited).
    publish_announce(&broker, "sticky", "1.1.0").await;
    tokio::time::sleep(Duration::from_millis(350)).await;

    // No remove/add events should fire for failed replacement.
    let maybe_event = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        maybe_event.is_err(),
        "unexpected directory event after failed bump"
    );

    let entries = dir.list();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "sticky");
    assert_eq!(entries[0].version, "1.0.0");

    dir.shutdown().await;
}

#[tokio::test]
async fn extension_event_is_forwarded_as_notification() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    let _mock = spawn_handshake_responder(broker.clone(), "ext.ev.rpc".into()).await;

    let (dir, mut rx) = ExtensionDirectory::spawn(broker.clone(), "ext", fast_opts());
    tokio::time::sleep(Duration::from_millis(20)).await;
    publish_announce(&broker, "ev", "1.0.0").await;
    wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;

    publish_event(
        &broker,
        "ev",
        serde_json::json!({"kind":"progress","pct":40}),
    )
    .await;
    let ev = wait_for(&mut rx, |e| {
        matches!(e, DirectoryEvent::Notification { .. })
    })
    .await;
    match ev {
        DirectoryEvent::Notification { id, payload } => {
            assert_eq!(id, "ev");
            assert_eq!(payload["kind"], "progress");
            assert_eq!(payload["pct"], 40);
        }
        _ => unreachable!(),
    }

    dir.shutdown().await;
}
