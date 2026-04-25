//! End-to-end smoke test for `ExtensionDirectory` against a real NATS broker.
//!
//! Gated by `NATS_URL` — skipped otherwise. Validates announce -> connect ->
//! directory Added event, then shutdown beacon -> Removed event.
//!
//! Typical invocation against the docker-compose stack:
//!     NATS_URL=nats://127.0.0.1:4222 cargo test -p nexo-extensions --test directory_nats_e2e_test -- --nocapture

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use nexo_broker::{BrokerHandle, Event, Message, NatsBroker};
use nexo_config::types::broker::{
    BrokerAuthConfig, BrokerFallbackConfig, BrokerInner, BrokerKind, BrokerLimitsConfig,
    BrokerPersistenceConfig,
};
use nexo_extensions::runtime::announce::{AnnounceCapabilities, AnnouncePayload, ShutdownPayload};
use nexo_extensions::runtime::{DirectoryEvent, ExtensionDirectory, NatsRuntimeOptions};
use uuid::Uuid;

fn fast_opts() -> NatsRuntimeOptions {
    NatsRuntimeOptions {
        call_timeout: Duration::from_secs(2),
        handshake_timeout: Duration::from_secs(2),
        heartbeat_interval: Duration::from_millis(250),
        heartbeat_grace_factor: 3,
        shutdown_grace: Duration::from_millis(300),
    }
}

fn nats_cfg(url: String, queue_path: String) -> BrokerInner {
    BrokerInner {
        kind: BrokerKind::Nats,
        url,
        auth: BrokerAuthConfig::default(),
        persistence: BrokerPersistenceConfig {
            enabled: true,
            path: queue_path,
        },
        limits: BrokerLimitsConfig::default(),
        fallback: BrokerFallbackConfig::default(),
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
    tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .expect("timeout waiting for directory event")
}

async fn spawn_handshake_responder(
    client: async_nats::Client,
    subject: String,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let mut sub = client.subscribe(subject).await?;
    Ok(tokio::spawn(async move {
        while let Some(msg) = sub.next().await {
            let Some(reply_to) = msg.reply.clone() else {
                continue;
            };
            let Ok(request): Result<Message, _> = serde_json::from_slice(&msg.payload) else {
                continue;
            };
            let line = match &request.payload {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let Ok(req): Result<serde_json::Value, _> = serde_json::from_str(&line) else {
                continue;
            };
            if req.get("method").and_then(|m| m.as_str()) != Some("initialize") {
                continue;
            }
            let id = req.get("id").cloned().unwrap_or(serde_json::json!(null));
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "result": {"server_version": "e2e", "tools": [], "hooks": []},
                "id": id,
            });
            let reply = Message::new("reply", serde_json::Value::String(response.to_string()));
            let Ok(buf) = serde_json::to_vec(&reply) else {
                continue;
            };
            if client.publish(reply_to, buf.into()).await.is_ok() {
                let _ = client.flush().await;
            }
        }
    }))
}

async fn publish_announce(
    client: &async_nats::Client,
    prefix: &str,
    id: &str,
    version: &str,
) -> anyhow::Result<()> {
    let payload = AnnouncePayload {
        schema_version: 1,
        id: id.into(),
        version: version.into(),
        subject_prefix: prefix.into(),
        manifest_hash: None,
        capabilities: AnnounceCapabilities::default(),
    };
    let topic = format!("{prefix}.registry.announce");
    let ev = Event::new(topic.clone(), "nats-e2e", serde_json::to_value(payload)?);
    client
        .publish(topic, serde_json::to_vec(&ev)?.into())
        .await?;
    client.flush().await?;
    Ok(())
}

async fn publish_shutdown(
    client: &async_nats::Client,
    prefix: &str,
    id: &str,
) -> anyhow::Result<()> {
    let payload = ShutdownPayload {
        id: id.into(),
        reason: Some("e2e-test".into()),
    };
    let topic = format!("{prefix}.registry.shutdown.{id}");
    let ev = Event::new(topic.clone(), "nats-e2e", serde_json::to_value(payload)?);
    client
        .publish(topic, serde_json::to_vec(&ev)?.into())
        .await?;
    client.flush().await?;
    Ok(())
}

#[tokio::test]
async fn directory_discovers_and_removes_extension_over_real_nats() -> anyhow::Result<()> {
    let Ok(nats_url) = std::env::var("NATS_URL") else {
        eprintln!("NATS_URL not set — skipping ExtensionDirectory NATS E2E");
        return Ok(());
    };

    let prefix = format!("ext-e2e-{}", Uuid::new_v4().simple());
    let ext_id = "weather";
    let rpc_subject = format!("{prefix}.{ext_id}.rpc");

    let raw_client = async_nats::connect(&nats_url).await?;
    let _responder = spawn_handshake_responder(raw_client.clone(), rpc_subject).await?;

    let tmp = tempfile::tempdir()?;
    let cfg = nats_cfg(
        nats_url,
        tmp.path().join("queue.sqlite").display().to_string(),
    );
    let broker = NatsBroker::connect(&cfg).await?;
    let broker: Arc<dyn BrokerHandle> = Arc::new(broker);

    let (dir, mut rx) = ExtensionDirectory::spawn(broker, prefix.clone(), fast_opts());

    tokio::time::sleep(Duration::from_millis(100)).await;
    publish_announce(&raw_client, &prefix, ext_id, "1.0.0").await?;

    let added = wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Added { .. })).await;
    match added {
        DirectoryEvent::Added { id, version, .. } => {
            assert_eq!(id, ext_id);
            assert_eq!(version, "1.0.0");
        }
        _ => unreachable!(),
    }

    publish_shutdown(&raw_client, &prefix, ext_id).await?;
    let removed = wait_for(&mut rx, |e| matches!(e, DirectoryEvent::Removed { .. })).await;
    match removed {
        DirectoryEvent::Removed { id, .. } => assert_eq!(id, ext_id),
        _ => unreachable!(),
    }

    dir.shutdown().await;
    Ok(())
}
