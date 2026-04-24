use std::sync::Arc;
use std::time::Duration;

use async_nats::connection::State;
use async_nats::ConnectOptions;
use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use agent_config::types::broker::BrokerInner;
use agent_resilience::{CircuitBreaker, CircuitBreakerConfig};

use crate::disk_queue::DiskQueue;
use crate::handle::{BrokerHandle, Subscription};
use crate::types::{BrokerError, Event, Message};

// ── NatsBroker ────────────────────────────────────────────────────────────────

pub struct NatsBroker {
    client: async_nats::Client,
    circuit: Arc<CircuitBreaker>,
    disk_queue: Arc<DiskQueue>,
    shutdown: CancellationToken,
}

impl NatsBroker {
    pub async fn connect(cfg: &BrokerInner) -> anyhow::Result<Self> {
        let options: ConnectOptions = if cfg.auth.enabled {
            match &cfg.auth.nkey_file {
                Some(path) => {
                    let seed = tokio::fs::read_to_string(path)
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to read nkey file {path}: {e}"))?;
                    ConnectOptions::with_nkey(seed.trim().to_string())
                }
                None => ConnectOptions::default(),
            }
        } else {
            ConnectOptions::default()
        };

        let client = options
            .connect(&cfg.url)
            .await
            .map_err(|e| anyhow::anyhow!("NATS connect failed: {e}"))?;

        let disk_queue =
            Arc::new(DiskQueue::new(&cfg.persistence.path, cfg.limits.max_pending).await?);

        let circuit = Arc::new(CircuitBreaker::new(
            "nats",
            CircuitBreakerConfig {
                failure_threshold: 1,
                success_threshold: 1,
                initial_backoff: Duration::from_secs(10),
                max_backoff: Duration::from_secs(120),
            },
        ));
        let shutdown = CancellationToken::new();

        let broker = Self {
            client,
            circuit,
            disk_queue,
            shutdown,
        };
        broker.spawn_state_monitor();
        Ok(broker)
    }

    fn spawn_state_monitor(&self) {
        let client = self.client.clone();
        let circuit = Arc::clone(&self.circuit);
        let disk_queue = Arc::clone(&self.disk_queue);
        let shutdown = self.shutdown.clone();
        let client2 = self.client.clone();

        tokio::spawn(async move {
            let mut last_state = client.connection_state();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    _ = shutdown.cancelled() => break,
                }

                let state = client.connection_state();
                match (&last_state, &state) {
                    (State::Connected, State::Disconnected | State::Pending) => {
                        tracing::warn!("NATS disconnected — opening circuit breaker");
                        circuit.trip();
                    }
                    (State::Disconnected | State::Pending, State::Connected) => {
                        tracing::info!("NATS reconnected — draining disk queue");
                        circuit.reset();
                        // Drain disk queue in background
                        let dq = Arc::clone(&disk_queue);
                        let c = client2.clone();
                        tokio::spawn(async move {
                            match dq.drain_nats(&c).await {
                                Ok(n) if n > 0 => {
                                    tracing::info!(count = n, "drained pending events")
                                }
                                Ok(_) => {}
                                Err(e) => tracing::error!(error = %e, "disk queue drain failed"),
                            }
                        });
                    }
                    _ => {}
                }
                last_state = state;
            }
        });
    }

    pub async fn stop(&self) {
        self.shutdown.cancel();
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.client.connection_state(), State::Connected)
    }
}

#[async_trait]
impl BrokerHandle for NatsBroker {
    async fn publish(&self, topic: &str, event: Event) -> Result<(), BrokerError> {
        // Fast-path: circuit is open → straight to disk queue. Prevents
        // hammering a known-broken NATS.
        if !self.circuit.allow() {
            return self
                .disk_queue
                .enqueue(topic, &event)
                .await
                .map_err(|e| BrokerError::SendError(e.to_string()));
        }

        let payload =
            serde_json::to_vec(&event).map_err(|e| BrokerError::SendError(e.to_string()))?;

        let topic_owned = topic.to_string();

        match self.client.publish(topic_owned, Bytes::from(payload)).await {
            Ok(()) => {
                self.circuit.on_success();
                Ok(())
            }
            Err(e) => {
                // The first failure is the one that trips the circuit —
                // persist the event on the disk queue so reconnect can
                // drain it instead of returning SendError with the event
                // lost forever. Disk-queue enqueue failure is then the
                // only terminal error we surface.
                self.circuit.trip();
                tracing::warn!(
                    topic,
                    error = %e,
                    "NATS publish failed — enqueuing to disk for drain-on-reconnect"
                );
                match self.disk_queue.enqueue(topic, &event).await {
                    Ok(()) => Ok(()),
                    Err(enqueue_err) => Err(BrokerError::SendError(format!(
                        "publish failed ({e}) and disk-queue enqueue failed ({enqueue_err})"
                    ))),
                }
            }
        }
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription, BrokerError> {
        let topic_owned = topic.to_string();
        let mut nats_sub = self
            .client
            .subscribe(topic_owned.clone())
            .await
            .map_err(|e| BrokerError::SubscribeError(e.to_string()))?;

        let (tx, rx) = mpsc::channel(256);

        tokio::spawn(async move {
            use futures::StreamExt;
            while let Some(msg) = nats_sub.next().await {
                let event: Event = match serde_json::from_slice(&msg.payload) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(
                            subject = %msg.subject,
                            error = %e,
                            "failed to deserialize NATS message — skipping"
                        );
                        continue;
                    }
                };
                if tx.send(event).await.is_err() {
                    break; // Subscription receiver dropped
                }
            }
        });

        Ok(Subscription::new(topic_owned, rx))
    }

    async fn request(
        &self,
        topic: &str,
        msg: Message,
        timeout: Duration,
    ) -> Result<Message, BrokerError> {
        let topic_owned = topic.to_string();
        let payload =
            serde_json::to_vec(&msg).map_err(|e| BrokerError::SendError(e.to_string()))?;

        let reply = tokio::time::timeout(
            timeout,
            self.client
                .request(topic_owned.clone(), Bytes::from(payload)),
        )
        .await
        .map_err(|_| BrokerError::RequestTimeout(topic_owned))?
        .map_err(|e| BrokerError::SendError(e.to_string()))?;

        serde_json::from_slice(&reply.payload)
            .map_err(|e| BrokerError::SendError(format!("failed to deserialize reply: {e}")))
    }
}
