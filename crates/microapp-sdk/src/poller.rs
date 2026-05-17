//! Phase 96 — child-side helpers for plugin v2 poller subprocesses.
//!
//! A poller plugin's `main.rs` typically:
//!
//! ```ignore
//! use nexo_microapp_sdk::poller::{PollerHandler, serve_one_tick};
//! use nexo_microapp_sdk::plugin::PluginAdapter;
//!
//! struct MyHandler;
//!
//! #[async_trait::async_trait]
//! impl PollerHandler for MyHandler {
//!     async fn tick(
//!         &self,
//!         req: TickRequest,
//!         host: std::sync::Arc<dyn nexo_poller::PollerHost>,
//!     ) -> Result<nexo_poller::TickAck, nexo_poller::PollerError> {
//!         /* poller logic */ Ok(Default::default())
//!     }
//! }
//!
//! PluginAdapter::new(MANIFEST, env!("CARGO_PKG_VERSION"))
//!     .on_broker_event(/* dispatch tick topics to serve_one_tick */)
//!     .run_stdio().await?;
//! ```
//!
//! The adapter wires the broker subscription via `on_broker_event`;
//! `serve_one_tick` does the heavy lifting: parses [`TickRequest`],
//! constructs a [`BrokerPollerHost`], invokes the user's
//! [`PollerHandler`], encodes the [`TickAck`] back into the wire
//! shape, publishes to the message's `reply_to` topic.
//!
//! Why no `PollerPluginAdapter` wrapper? `PluginAdapter` already
//! owns the broker handle + stdio loop; bolting on a parallel
//! sub-adapter would double the surface. The functional helper is
//! ~30 LOC and composes with whatever transport the plugin author
//! prefers.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use nexo_broker::{AnyBroker, BrokerHandle, Event, Message};
use nexo_poller::{
    HostError, LlmInvokeRequest, LlmInvokeResponse, LogLevel, PollerError, PollerHost, TickAck,
    TickMetrics,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// What a poller plugin handler implements. One method — `tick` —
/// because schedule/cursor/retry semantics live with the daemon's
/// runner; the subprocess is pure fetch + transform.
#[async_trait]
pub trait PollerHandler: Send + Sync + 'static {
    /// Execute one tick. The runner persists the returned cursor +
    /// honors the interval hint; outbound (broker publish, LLM call,
    /// credential lookup) goes through `host`.
    async fn tick(
        &self,
        req: TickRequest,
        host: Arc<dyn PollerHost>,
    ) -> Result<TickAck, PollerError>;
}

/// Tick request payload sent by the daemon's `PluginPollerRouter`
/// over broker JSON-RPC. Mirrors the runtime `PollContext` minus
/// the in-process handle types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickRequest {
    /// Poller kind discriminator (matches `[plugin.poller].kinds`).
    pub kind: String,
    /// Stable job id from `pollers.yaml`.
    pub job_id: String,
    /// Agent owning this job (for credential resolution).
    pub agent_id: String,
    /// URL-safe base64 (no padding). Decoded into bytes by
    /// [`Self::cursor_bytes`].
    #[serde(default)]
    pub cursor: Option<String>,
    /// Per-job `config:` block from `pollers.yaml`.
    pub config: Value,
    /// RFC3339 timestamp captured at the daemon's dispatch moment.
    pub now: String,
    /// Schedule's nominal interval in seconds before jitter.
    #[serde(default)]
    pub interval_hint_secs: u64,
}

impl TickRequest {
    /// Decode the base64 cursor into raw bytes. `Ok(None)` when no
    /// cursor was provided (first run / after reset).
    pub fn cursor_bytes(&self) -> Result<Option<Vec<u8>>, PollerError> {
        match self.cursor.as_deref() {
            None => Ok(None),
            Some(s) => base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s.trim_end_matches('='))
                .map(Some)
                .map_err(|e| {
                    PollerError::Config {
                        job: self.job_id.clone(),
                        reason: format!("cursor base64 decode: {e}"),
                    }
                }),
        }
    }
}

/// Wire shape of the success reply published to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TickReply {
    /// New cursor as URL-safe base64. `None` keeps the previous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Override the next interval just for the upcoming slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_interval_secs: Option<u64>,
    /// Optional telemetry counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<TickMetrics>,
}

impl TickReply {
    /// Encode the runner-facing [`TickAck`] into the wire shape.
    pub fn from_tick_ack(ack: TickAck) -> Self {
        Self {
            next_cursor: ack.next_cursor.as_deref().map(|b| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
            }),
            next_interval_secs: ack.next_interval_hint.map(|d| d.as_secs()),
            metrics: ack.metrics,
        }
    }
}

/// Subprocess-side [`PollerHost`] implementation. Forwards every
/// trait method to the daemon via broker reverse-RPC published on
/// `daemon.rpc.<plugin_id>` (matching the daemon-side subscription
/// the runner wires up in Phase 96.7).
pub struct BrokerPollerHost {
    plugin_id: String,
    agent_id: String,
    job_id: String,
    broker: AnyBroker,
    reverse_rpc_timeout: Duration,
}

impl BrokerPollerHost {
    /// Construct a fresh host scoped to a single tick. Cheap — only
    /// `Arc` clones on the broker handle.
    pub fn new(
        plugin_id: impl Into<String>,
        agent_id: impl Into<String>,
        job_id: impl Into<String>,
        broker: AnyBroker,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            agent_id: agent_id.into(),
            job_id: job_id.into(),
            broker,
            reverse_rpc_timeout: Duration::from_secs(10),
        }
    }

    /// Override the default 10s reverse-RPC timeout. Useful for
    /// long-running credential refreshes the daemon may serialise.
    pub fn with_reverse_rpc_timeout(mut self, t: Duration) -> Self {
        self.reverse_rpc_timeout = t;
        self
    }

    fn rpc_topic(&self) -> String {
        format!("daemon.rpc.{}", self.plugin_id)
    }

    async fn rpc_call(&self, method: &str, params: Value) -> Result<Value, HostError> {
        let topic = self.rpc_topic();
        let payload = json!({
            "method": method,
            "params": params,
            "agent_id": self.agent_id,
            "job_id": self.job_id,
        });
        let msg = Message::new(topic.clone(), payload);
        let reply = self
            .broker
            .request(&topic, msg, self.reverse_rpc_timeout)
            .await
            .map_err(|e| HostError::BrokerUnavailable(e.to_string()))?;

        if let Some(error) = reply.payload.get("error") {
            let code = error
                .get("code")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32)
                .unwrap_or(-32603);
            let message = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("rpc error")
                .to_string();
            return Err(HostError::Rpc { code, message });
        }

        Ok(reply
            .payload
            .get("result")
            .cloned()
            .unwrap_or(Value::Null))
    }
}

#[async_trait]
impl PollerHost for BrokerPollerHost {
    async fn broker_publish(&self, topic: String, payload: Vec<u8>) -> Result<(), HostError> {
        // Publish directly to the broker (Phase 92 stdio_bridge or
        // NATS). The daemon enforces the manifest publish allowlist
        // downstream; reverse-RPC would be opt-in if allowlist
        // enforcement at the SDK layer becomes desirable later.
        let value: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
        let event = Event::new(&topic, "plugin.poller", value);
        self.broker
            .publish(&topic, event)
            .await
            .map_err(|e| HostError::BrokerUnavailable(e.to_string()))
    }

    async fn credentials_get(&self, channel: String) -> Result<Value, HostError> {
        self.rpc_call(
            "credentials_get",
            json!({ "channel": channel, "agent_id": self.agent_id }),
        )
        .await
    }

    async fn log(
        &self,
        level: LogLevel,
        message: String,
        fields: Value,
    ) -> Result<(), HostError> {
        self.rpc_call(
            "log",
            json!({
                "level": level,
                "message": message,
                "fields": fields,
            }),
        )
        .await
        .map(|_| ())
    }

    async fn metric_inc(&self, name: String, labels: Value) -> Result<(), HostError> {
        self.rpc_call("metric_inc", json!({ "name": name, "labels": labels }))
            .await
            .map(|_| ())
    }

    async fn llm_invoke(
        &self,
        request: LlmInvokeRequest,
    ) -> Result<LlmInvokeResponse, HostError> {
        let reply = self
            .rpc_call("llm_invoke", serde_json::to_value(request).unwrap_or(Value::Null))
            .await?;
        serde_json::from_value::<LlmInvokeResponse>(reply).map_err(|e| HostError::Other(e.into()))
    }
}

/// Serve a single tick request. Wires the [`PollerHandler`] to a
/// freshly-constructed [`BrokerPollerHost`], invokes the handler,
/// and publishes the reply (success or JSON-RPC error envelope) to
/// `reply_to`. The plugin author calls this from inside their
/// `on_broker_event` closure whenever a message arrives on
/// `<broker_topic_prefix>.tick`.
///
/// Returns `Ok(())` even on poller errors — error envelopes are sent
/// back to the daemon via `reply_to`. `Err` is reserved for broker
/// failures (no `reply_to`, corrupt envelope) the caller may want
/// to log.
pub async fn serve_one_tick(
    plugin_id: &str,
    broker: AnyBroker,
    handler: Arc<dyn PollerHandler>,
    request_payload: Value,
    reply_to: Option<&str>,
) -> Result<(), ServeError> {
    let reply_topic = reply_to.ok_or(ServeError::MissingReplyTo)?.to_string();

    let request_envelope = request_payload
        .get("params")
        .cloned()
        .unwrap_or(request_payload.clone());
    let request: TickRequest = match serde_json::from_value(request_envelope) {
        Ok(r) => r,
        Err(e) => {
            let err = json!({
                "error": { "code": -32602, "message": format!("malformed TickRequest: {e}") }
            });
            let _ = broker
                .publish(&reply_topic, Event::new(&reply_topic, "plugin.poller", err))
                .await;
            return Ok(());
        }
    };

    let host = Arc::new(BrokerPollerHost::new(
        plugin_id,
        request.agent_id.clone(),
        request.job_id.clone(),
        broker.clone(),
    )) as Arc<dyn PollerHost>;

    let payload = match handler.tick(request, host).await {
        Ok(ack) => {
            let reply = TickReply::from_tick_ack(ack);
            json!({ "result": reply })
        }
        Err(e) => {
            let (code, message) = poller_error_to_rpc(e);
            json!({ "error": { "code": code, "message": message } })
        }
    };

    let _ = broker
        .publish(
            &reply_topic,
            Event::new(&reply_topic, "plugin.poller", payload),
        )
        .await;
    Ok(())
}

fn poller_error_to_rpc(err: PollerError) -> (i32, String) {
    use nexo_poller::error::ErrorClass;
    let msg = err.to_string();
    let code = match err.classify() {
        ErrorClass::Transient => -32001,
        ErrorClass::Permanent => -32002,
        ErrorClass::Config => -32602,
    };
    (code, msg)
}

/// Top-level error returned by [`serve_one_tick`]. Reserved for
/// transport-level problems the caller may want to log; poller
/// failures are serialised back to the daemon via the reply topic.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The incoming JSON-RPC envelope omitted `reply_to`. Without it
    /// the subprocess has nowhere to send the response.
    #[error("incoming tick request has no reply_to topic; cannot respond")]
    MissingReplyTo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_request_cursor_bytes_round_trip() {
        let raw = b"hello world";
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let req = TickRequest {
            kind: "rss".into(),
            job_id: "j1".into(),
            agent_id: "ana".into(),
            cursor: Some(b64),
            config: Value::Null,
            now: "2026-05-17T10:00:00Z".into(),
            interval_hint_secs: 60,
        };
        let bytes = req.cursor_bytes().unwrap().unwrap();
        assert_eq!(bytes, raw);
    }

    #[test]
    fn tick_request_cursor_none() {
        let req = TickRequest {
            kind: "rss".into(),
            job_id: "j1".into(),
            agent_id: "ana".into(),
            cursor: None,
            config: Value::Null,
            now: "2026-05-17T10:00:00Z".into(),
            interval_hint_secs: 0,
        };
        assert!(req.cursor_bytes().unwrap().is_none());
    }

    #[test]
    fn tick_request_bad_cursor_errors() {
        let req = TickRequest {
            kind: "rss".into(),
            job_id: "j1".into(),
            agent_id: "ana".into(),
            cursor: Some("!!not_b64!!".into()),
            config: Value::Null,
            now: "2026-05-17T10:00:00Z".into(),
            interval_hint_secs: 0,
        };
        let err = req.cursor_bytes().unwrap_err();
        assert!(matches!(err, PollerError::Config { .. }));
    }

    #[test]
    fn tick_reply_from_tick_ack_encodes_cursor() {
        let ack = TickAck {
            next_cursor: Some(b"world".to_vec()),
            next_interval_hint: Some(Duration::from_secs(120)),
            metrics: Some(TickMetrics {
                items_seen: 3,
                items_dispatched: 1,
            }),
        };
        let reply = TickReply::from_tick_ack(ack);
        assert_eq!(reply.next_cursor.as_deref(), Some("d29ybGQ"));
        assert_eq!(reply.next_interval_secs, Some(120));
        let m = reply.metrics.unwrap();
        assert_eq!(m.items_seen, 3);
    }

    #[test]
    fn poller_error_classification_into_rpc_code() {
        let (c, _) = poller_error_to_rpc(PollerError::Transient(anyhow::anyhow!("503")));
        assert_eq!(c, -32001);
        let (c, _) = poller_error_to_rpc(PollerError::Permanent(anyhow::anyhow!("revoked")));
        assert_eq!(c, -32002);
        let (c, _) = poller_error_to_rpc(PollerError::Config {
            job: "x".into(),
            reason: "y".into(),
        });
        assert_eq!(c, -32602);
    }
}
