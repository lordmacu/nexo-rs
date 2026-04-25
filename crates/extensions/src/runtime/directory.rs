//! Extension registry — announce-based discovery over NATS.
//!
//! `ExtensionDirectory` listens on the registry subjects, materialises a
//! `NatsRuntime` for every live extension, tracks liveness via heartbeats,
//! and streams `DirectoryEvent`s to subscribers (11.5 tool registry,
//! 11.6 hook registry).

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use nexo_broker::{BrokerHandle, Event};

use super::announce::{AnnouncePayload, HeartbeatPayload, RegistryRequestPayload, ShutdownPayload};
use super::nats::{NatsRuntime, NatsRuntimeOptions};
use super::transport::ExtensionTransport;

const EVENT_CHANNEL_CAP: usize = 64;

#[derive(Debug, Clone)]
pub enum RemovalReason {
    HeartbeatLost,
    Announced { new_version: String },
    Shutdown { reason: Option<String> },
}

#[derive(Clone)]
pub enum DirectoryEvent {
    Added {
        id: String,
        version: String,
        runtime: Arc<dyn ExtensionTransport>,
    },
    Removed {
        id: String,
        version: String,
        reason: RemovalReason,
    },
    Refreshed {
        id: String,
        version: String,
    },
    Notification {
        id: String,
        payload: serde_json::Value,
    },
}

impl std::fmt::Debug for DirectoryEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added { id, version, .. } => f
                .debug_struct("Added")
                .field("id", id)
                .field("version", version)
                .finish_non_exhaustive(),
            Self::Removed {
                id,
                version,
                reason,
            } => f
                .debug_struct("Removed")
                .field("id", id)
                .field("version", version)
                .field("reason", reason)
                .finish(),
            Self::Refreshed { id, version } => f
                .debug_struct("Refreshed")
                .field("id", id)
                .field("version", version)
                .finish(),
            Self::Notification { id, payload } => f
                .debug_struct("Notification")
                .field("id", id)
                .field("payload", payload)
                .finish(),
        }
    }
}

#[derive(Clone)]
pub struct DirectoryEntry {
    pub id: String,
    pub version: String,
    pub runtime: Arc<NatsRuntime>,
    pub last_seen: Arc<std::sync::RwLock<Instant>>,
}

type Entries = Arc<DashMap<String, DirectoryEntry>>;

pub struct ExtensionDirectory {
    subject_prefix: String,
    #[allow(dead_code)]
    broker: Arc<dyn BrokerHandle>,
    entries: Entries,
    shutdown: CancellationToken,
    tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl ExtensionDirectory {
    /// Spawn the directory; returns the handle and a receiver of
    /// `DirectoryEvent`s to drive downstream registries.
    pub fn spawn(
        broker: Arc<dyn BrokerHandle>,
        subject_prefix: impl Into<String>,
        opts: NatsRuntimeOptions,
    ) -> (Self, mpsc::Receiver<DirectoryEvent>) {
        let subject_prefix = subject_prefix.into();
        let entries: Entries = Arc::new(DashMap::new());
        let shutdown = CancellationToken::new();
        let (events_tx, events_rx) = mpsc::channel::<DirectoryEvent>(EVENT_CHANNEL_CAP);

        let this = Self {
            subject_prefix: subject_prefix.clone(),
            broker: broker.clone(),
            entries: entries.clone(),
            shutdown: shutdown.clone(),
            tasks: std::sync::Mutex::new(Vec::new()),
        };

        // Announce listener.
        {
            let sub_topic = format!("{subject_prefix}.registry.announce");
            let handle = tokio::spawn(announce_task(
                broker.clone(),
                sub_topic,
                subject_prefix.clone(),
                entries.clone(),
                events_tx.clone(),
                opts.clone(),
                shutdown.clone(),
            ));
            this.tasks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(handle);
        }
        // Heartbeat listener.
        {
            let sub_topic = format!("{subject_prefix}.registry.heartbeat.*");
            let handle = tokio::spawn(heartbeat_task(
                broker.clone(),
                sub_topic,
                entries.clone(),
                events_tx.clone(),
                shutdown.clone(),
            ));
            this.tasks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(handle);
        }
        // Shutdown beacon listener.
        {
            let sub_topic = format!("{subject_prefix}.registry.shutdown.*");
            let handle = tokio::spawn(shutdown_beacon_task(
                broker.clone(),
                sub_topic,
                entries.clone(),
                events_tx.clone(),
                shutdown.clone(),
            ));
            this.tasks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(handle);
        }
        // Extension async event listener.
        {
            let sub_topic = format!("{subject_prefix}.*.event");
            let handle = tokio::spawn(notification_task(
                broker.clone(),
                sub_topic,
                subject_prefix.clone(),
                entries.clone(),
                events_tx.clone(),
                shutdown.clone(),
            ));
            this.tasks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(handle);
        }
        // Liveness sweeper.
        {
            let handle = tokio::spawn(liveness_sweep_task(
                entries.clone(),
                events_tx.clone(),
                opts.clone(),
                shutdown.clone(),
            ));
            this.tasks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(handle);
        }
        // Initial replay request — live extensions re-announce.
        {
            let broker = broker.clone();
            let subj = format!("{subject_prefix}.registry.request");
            let shutdown = shutdown.clone();
            let handle = tokio::spawn(async move {
                // Give subscribers a tick to install before we shout.
                tokio::time::sleep(Duration::from_millis(10)).await;
                if shutdown.is_cancelled() {
                    return;
                }
                let payload = RegistryRequestPayload {
                    requested_by: format!("agent-{}", Uuid::new_v4()),
                };
                let ev = Event::new(
                    &subj,
                    "extension-directory",
                    serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null),
                );
                let _ = broker.publish(&subj, ev).await;
            });
            this.tasks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(handle);
        }

        (this, events_rx)
    }

    pub fn list(&self) -> Vec<DirectoryEntry> {
        self.entries.iter().map(|e| e.value().clone()).collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<NatsRuntime>> {
        self.entries.get(id).map(|e| e.value().runtime.clone())
    }

    pub fn subject_prefix(&self) -> &str {
        &self.subject_prefix
    }

    /// Cancel all tasks and shut every runtime down. Idempotent.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        let tasks: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|p| p.into_inner()));
        for h in tasks {
            h.abort();
        }
        let ids: Vec<String> = self.entries.iter().map(|e| e.key().clone()).collect();
        for id in ids {
            if let Some((_, entry)) = self.entries.remove(&id) {
                entry.runtime.shutdown().await;
            }
        }
    }
}

fn parse_notification_id(topic: &str, prefix: &str) -> Option<String> {
    let rest = topic.strip_prefix(prefix)?;
    let rest = rest.strip_prefix('.')?;
    let id = rest.strip_suffix(".event")?;
    if id.is_empty() || id.contains('.') {
        return None;
    }
    Some(id.to_string())
}

impl Drop for ExtensionDirectory {
    fn drop(&mut self) {
        self.shutdown.cancel();
        let tasks: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|p| p.into_inner()));
        for h in tasks {
            h.abort();
        }
    }
}

// ─── Task loops ───────────────────────────────────────────────────────────────

async fn announce_task(
    broker: Arc<dyn BrokerHandle>,
    subject: String,
    subject_prefix: String,
    entries: Entries,
    events_tx: mpsc::Sender<DirectoryEvent>,
    opts: NatsRuntimeOptions,
    shutdown: CancellationToken,
) {
    let mut sub = match broker.subscribe(&subject).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, subject, "subscribe to announces failed");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            ev = sub.next() => {
                let Some(ev) = ev else { break };
                let announce: AnnouncePayload = match serde_json::from_value(ev.payload) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!(?e, "invalid announce payload");
                        continue;
                    }
                };
                if announce.schema_version
                    > crate::runtime::announce::ANNOUNCE_SCHEMA_VERSION
                {
                    tracing::warn!(
                        ext = %announce.id,
                        ext_version = %announce.version,
                        announce_schema = announce.schema_version,
                        supported = crate::runtime::announce::ANNOUNCE_SCHEMA_VERSION,
                        "rejecting announce: schema_version newer than supported"
                    );
                    continue;
                }
                handle_announce(
                    &broker,
                    &subject_prefix,
                    &entries,
                    &events_tx,
                    &opts,
                    announce,
                ).await;
            }
        }
    }
}

async fn handle_announce(
    broker: &Arc<dyn BrokerHandle>,
    default_prefix: &str,
    entries: &Entries,
    events_tx: &mpsc::Sender<DirectoryEvent>,
    opts: &NatsRuntimeOptions,
    announce: AnnouncePayload,
) {
    let id = announce.id.clone();
    // Dedupe + version-bump handling.
    if let Some(existing) = entries.get(&id).map(|e| e.value().clone()) {
        if existing.version == announce.version {
            // Same version re-announce → just refresh last_seen.
            *existing
                .last_seen
                .write()
                .unwrap_or_else(|p| p.into_inner()) = Instant::now();
            let _ = events_tx
                .send(DirectoryEvent::Refreshed {
                    id: id.clone(),
                    version: existing.version.clone(),
                })
                .await;
            return;
        }
        // Different version — connect the new runtime first. If connect fails
        // keep the old one live to avoid a gap where no version is active.
        let prefix = if announce.subject_prefix.is_empty() {
            default_prefix.to_string()
        } else {
            announce.subject_prefix.clone()
        };
        let runtime =
            match NatsRuntime::connect(broker.clone(), id.clone(), prefix, opts.clone()).await {
                Ok(rt) => Arc::new(rt),
                Err(e) => {
                    tracing::warn!(
                        id,
                        old_version = %existing.version,
                        new_version = %announce.version,
                        ?e,
                        "connect to announced replacement failed; keeping old runtime"
                    );
                    return;
                }
            };
        let replacement = DirectoryEntry {
            id: id.clone(),
            version: announce.version.clone(),
            runtime: runtime.clone(),
            last_seen: Arc::new(std::sync::RwLock::new(Instant::now())),
        };
        entries.insert(id.clone(), replacement);
        existing.runtime.mark_failed("superseded by new announce");
        let _ = events_tx
            .send(DirectoryEvent::Removed {
                id: id.clone(),
                version: existing.version.clone(),
                reason: RemovalReason::Announced {
                    new_version: announce.version.clone(),
                },
            })
            .await;
        let _ = events_tx
            .send(DirectoryEvent::Added {
                id,
                version: announce.version,
                runtime: runtime as Arc<dyn ExtensionTransport>,
            })
            .await;
        return;
    }

    // First version for this id.
    let prefix = if announce.subject_prefix.is_empty() {
        default_prefix.to_string()
    } else {
        announce.subject_prefix.clone()
    };
    let runtime = match NatsRuntime::connect(broker.clone(), id.clone(), prefix, opts.clone()).await
    {
        Ok(rt) => Arc::new(rt),
        Err(e) => {
            tracing::warn!(id, ?e, "connect to announced extension failed");
            return;
        }
    };
    let entry = DirectoryEntry {
        id: id.clone(),
        version: announce.version.clone(),
        runtime: runtime.clone(),
        last_seen: Arc::new(std::sync::RwLock::new(Instant::now())),
    };
    entries.insert(id.clone(), entry);
    let _ = events_tx
        .send(DirectoryEvent::Added {
            id,
            version: announce.version,
            runtime: runtime as Arc<dyn ExtensionTransport>,
        })
        .await;
}

async fn heartbeat_task(
    broker: Arc<dyn BrokerHandle>,
    subject: String,
    entries: Entries,
    events_tx: mpsc::Sender<DirectoryEvent>,
    shutdown: CancellationToken,
) {
    let mut sub = match broker.subscribe(&subject).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, subject, "subscribe to heartbeats failed");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            ev = sub.next() => {
                let Some(ev) = ev else { break };
                let hb: HeartbeatPayload = match serde_json::from_value(ev.payload) {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!(?e, "invalid heartbeat payload");
                        continue;
                    }
                };
                if let Some(entry) = entries.get(&hb.id).map(|e| e.value().clone()) {
                    if entry.version == hb.version {
                        *entry.last_seen.write().unwrap_or_else(|p| p.into_inner()) = Instant::now();
                        let _ = events_tx
                            .send(DirectoryEvent::Refreshed { id: hb.id, version: hb.version })
                            .await;
                    }
                }
            }
        }
    }
}

async fn shutdown_beacon_task(
    broker: Arc<dyn BrokerHandle>,
    subject: String,
    entries: Entries,
    events_tx: mpsc::Sender<DirectoryEvent>,
    shutdown: CancellationToken,
) {
    let mut sub = match broker.subscribe(&subject).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, subject, "subscribe to shutdown beacons failed");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            ev = sub.next() => {
                let Some(ev) = ev else { break };
                let bye: ShutdownPayload = match serde_json::from_value(ev.payload) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(?e, "invalid shutdown payload");
                        continue;
                    }
                };
                if let Some((_, entry)) = entries.remove(&bye.id) {
                    entry.runtime.mark_failed("shutdown beacon received");
                    let _ = events_tx
                        .send(DirectoryEvent::Removed {
                            id: entry.id,
                            version: entry.version,
                            reason: RemovalReason::Shutdown { reason: bye.reason },
                        })
                        .await;
                }
            }
        }
    }
}

async fn notification_task(
    broker: Arc<dyn BrokerHandle>,
    subject: String,
    subject_prefix: String,
    entries: Entries,
    events_tx: mpsc::Sender<DirectoryEvent>,
    shutdown: CancellationToken,
) {
    let mut sub = match broker.subscribe(&subject).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, subject, "subscribe to extension events failed");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            ev = sub.next() => {
                let Some(ev) = ev else { break };
                let Some(id) = parse_notification_id(&ev.topic, &subject_prefix) else {
                    tracing::warn!(topic = %ev.topic, "invalid extension event topic");
                    continue;
                };
                if entries.get(&id).is_none() {
                    tracing::debug!(id, "dropping event from unknown extension");
                    continue;
                }
                let _ = events_tx
                    .send(DirectoryEvent::Notification { id, payload: ev.payload })
                    .await;
            }
        }
    }
}

async fn liveness_sweep_task(
    entries: Entries,
    events_tx: mpsc::Sender<DirectoryEvent>,
    opts: NatsRuntimeOptions,
    shutdown: CancellationToken,
) {
    // Sweep at half the heartbeat interval so detection lag is bounded.
    let tick = (opts.heartbeat_interval / 2).max(Duration::from_millis(50));
    let grace = opts.heartbeat_interval * opts.heartbeat_grace_factor.max(1);
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = ticker.tick() => {
                let now = Instant::now();
                let stale: Vec<String> = entries
                    .iter()
                    .filter_map(|e| {
                        let last = *e.value().last_seen.read().unwrap_or_else(|p| p.into_inner());
                        if now.duration_since(last) > grace {
                            Some(e.key().clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                for id in stale {
                    if let Some((_, entry)) = entries.remove(&id) {
                        entry.runtime.mark_failed("heartbeat lost");
                        let _ = events_tx
                            .send(DirectoryEvent::Removed {
                                id: entry.id,
                                version: entry.version,
                                reason: RemovalReason::HeartbeatLost,
                            })
                            .await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_notification_id;

    #[test]
    fn parse_notification_id_extracts_id() {
        assert_eq!(
            parse_notification_id("ext.weather.event", "ext"),
            Some("weather".into())
        );
        assert_eq!(
            parse_notification_id("ext.ns.weather.event", "ext.ns"),
            Some("weather".into())
        );
    }

    #[test]
    fn parse_notification_id_rejects_invalid_shapes() {
        assert_eq!(parse_notification_id("ext.weather", "ext"), None);
        assert_eq!(parse_notification_id("ext..event", "ext"), None);
        assert_eq!(parse_notification_id("ext.a.b.event", "ext"), None);
        assert_eq!(parse_notification_id("other.weather.event", "ext"), None);
    }
}
