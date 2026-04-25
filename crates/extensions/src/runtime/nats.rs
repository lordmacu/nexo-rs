//! NATS extension runtime — speaks JSON-RPC 2.0 over NATS request/reply.
//!
//! The extension process lives outside the agent: the agent never spawns or
//! restarts it. We observe liveness via `{prefix}.registry.heartbeat.{id}`
//! beacons collected by `ExtensionDirectory` and guard outgoing calls with a
//! circuit breaker so a hung extension can't stall the agent.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use nexo_broker::{BrokerHandle, Message};
use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};

use super::transport::ExtensionTransport;
use super::wire;
use super::{CallError, HandshakeInfo, RuntimeState, StartError, ToolDescriptor};

pub const DEFAULT_SUBJECT_PREFIX: &str = "ext";

#[derive(Debug, Clone)]
pub struct NatsRuntimeOptions {
    pub call_timeout: Duration,
    pub handshake_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub heartbeat_grace_factor: u32,
    pub shutdown_grace: Duration,
}

impl Default for NatsRuntimeOptions {
    fn default() -> Self {
        Self {
            call_timeout: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(10),
            heartbeat_interval: Duration::from_secs(15),
            heartbeat_grace_factor: 3,
            shutdown_grace: Duration::from_secs(2),
        }
    }
}

pub struct NatsRuntime {
    extension_id: String,
    subject_prefix: String,
    rpc_subject: String,
    broker: Arc<dyn BrokerHandle>,
    state: Arc<RwLock<RuntimeState>>,
    handshake: HandshakeInfo,
    breaker: Arc<CircuitBreaker>,
    next_id: Arc<AtomicU64>,
    opts: NatsRuntimeOptions,
    shutdown: CancellationToken,
}

impl NatsRuntime {
    /// Connect to an extension identified by `id`/`subject_prefix`; the
    /// caller is typically `ExtensionDirectory` responding to an announce.
    pub async fn connect(
        broker: Arc<dyn BrokerHandle>,
        extension_id: impl Into<String>,
        subject_prefix: impl Into<String>,
        opts: NatsRuntimeOptions,
    ) -> Result<Self, StartError> {
        let extension_id = extension_id.into();
        let subject_prefix = subject_prefix.into();
        let rpc_subject = format!("{}.{}.rpc", subject_prefix, extension_id);
        let state = Arc::new(RwLock::new(RuntimeState::Spawning));
        let breaker = Arc::new(CircuitBreaker::new(
            format!("ext:nats:{extension_id}"),
            CircuitBreakerConfig::default(),
        ));
        let next_id = Arc::new(AtomicU64::new(1));

        // Handshake uses reserved id=0; regular calls start at 1.
        let line = wire::encode(
            "initialize",
            serde_json::json!({
                "agent_version": env!("CARGO_PKG_VERSION"),
                "extension_id": extension_id,
            }),
            0,
        )
        .map_err(StartError::DecodeHandshake)?;
        let msg = Message::new(
            rpc_subject.clone(),
            serde_json::Value::String(line.trim_end().to_string()),
        );

        let reply = tokio::time::timeout(
            opts.handshake_timeout,
            broker.request(&rpc_subject, msg, opts.handshake_timeout),
        )
        .await
        .map_err(|_| StartError::HandshakeTimeout(opts.handshake_timeout))?
        .map_err(|e| StartError::Broker(e.to_string()))?;

        let handshake = decode_jsonrpc_result(&reply)?;
        let handshake: HandshakeInfo =
            serde_json::from_value(handshake).map_err(StartError::DecodeHandshake)?;

        *state.write().unwrap_or_else(|p| p.into_inner()) = RuntimeState::Ready;

        Ok(Self {
            extension_id,
            subject_prefix,
            rpc_subject,
            broker,
            state,
            handshake,
            breaker,
            next_id,
            opts,
            shutdown: CancellationToken::new(),
        })
    }

    pub fn extension_id(&self) -> &str {
        &self.extension_id
    }

    pub fn subject_prefix(&self) -> &str {
        &self.subject_prefix
    }

    pub fn handshake(&self) -> &HandshakeInfo {
        &self.handshake
    }

    pub fn state(&self) -> RuntimeState {
        self.state.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn breaker(&self) -> Arc<CircuitBreaker> {
        self.breaker.clone()
    }

    /// Force the runtime into a terminal failed state. Used by the directory
    /// when heartbeats stop arriving so subsequent calls short-circuit.
    pub(crate) fn mark_failed(&self, reason: impl Into<String>) {
        *self.state.write().unwrap_or_else(|p| p.into_inner()) = RuntimeState::Failed {
            reason: reason.into(),
        };
    }

    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        if !matches!(self.state(), RuntimeState::Ready) {
            return Err(CallError::NotReady(format!("{:?}", self.state())));
        }
        if !self.breaker.allow() {
            return Err(CallError::BreakerOpen);
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = wire::encode(method, &params, id).map_err(CallError::Encode)?;
        let msg = Message::new(
            self.rpc_subject.clone(),
            serde_json::Value::String(line.trim_end().to_string()),
        );

        let reply = match self
            .broker
            .request(&self.rpc_subject, msg, self.opts.call_timeout)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                self.breaker.on_failure();
                let s = e.to_string();
                if s.contains("timed out") || s.contains("timeout") {
                    return Err(CallError::Timeout(self.opts.call_timeout));
                }
                return Err(CallError::Broker(s));
            }
        };

        match decode_jsonrpc_result(&reply) {
            Ok(v) => {
                self.breaker.on_success();
                Ok(v)
            }
            Err(StartError::Handshake(rpc)) => {
                self.breaker.on_failure();
                Err(CallError::Rpc(rpc))
            }
            Err(StartError::DecodeHandshake(e)) => {
                self.breaker.on_failure();
                Err(CallError::Decode(e))
            }
            Err(e) => {
                self.breaker.on_failure();
                Err(CallError::Broker(e.to_string()))
            }
        }
    }

    pub async fn tools_list(&self) -> Result<Vec<ToolDescriptor>, CallError> {
        let v = self.call("tools/list", serde_json::json!({})).await?;
        if let Some(arr) = v.get("tools").cloned() {
            serde_json::from_value(arr).map_err(CallError::Decode)
        } else {
            serde_json::from_value(v).map_err(CallError::Decode)
        }
    }

    pub async fn tools_call(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        self.call(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// Best-effort graceful shutdown: notifies the extension, cancels
    /// supervised tasks, waits `shutdown_grace`. Safe to call twice.
    pub async fn shutdown(&self) {
        self.shutdown_with_reason("client shutdown").await
    }

    /// Phase 11.3 follow-up — variant that threads a human-readable
    /// `reason` into the `shutdown` notification params so extensions
    /// can differentiate SIGTERM vs config reload vs explicit shutdown.
    pub async fn shutdown_with_reason(&self, reason: &str) {
        let reason = sanitize_reason(reason);
        if matches!(self.state(), RuntimeState::Ready) {
            if let Ok(line) = wire::encode("shutdown", serde_json::json!({ "reason": reason }), 0) {
                let msg = Message::new(
                    self.rpc_subject.clone(),
                    serde_json::Value::String(line.trim_end().to_string()),
                );
                let _ = self
                    .broker
                    .request(&self.rpc_subject, msg, self.opts.shutdown_grace)
                    .await;
            }
        }
        self.shutdown.cancel();
        *self.state.write().unwrap_or_else(|p| p.into_inner()) = RuntimeState::Shutdown;
    }
}

fn sanitize_reason(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

impl Drop for NatsRuntime {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[async_trait]
impl ExtensionTransport for NatsRuntime {
    fn extension_id(&self) -> &str {
        NatsRuntime::extension_id(self)
    }

    fn handshake(&self) -> &HandshakeInfo {
        NatsRuntime::handshake(self)
    }

    fn state(&self) -> RuntimeState {
        NatsRuntime::state(self)
    }

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        NatsRuntime::call(self, method, params).await
    }

    async fn tools_list(&self) -> Result<Vec<ToolDescriptor>, CallError> {
        NatsRuntime::tools_list(self).await
    }

    async fn tools_call(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        NatsRuntime::tools_call(self, name, arguments).await
    }

    async fn shutdown(&self) {
        NatsRuntime::shutdown(self).await
    }

    async fn shutdown_with_reason(&self, reason: &str) {
        NatsRuntime::shutdown_with_reason(self, reason).await
    }
}

/// Decode a NATS reply `Message` whose payload is a JSON-RPC 2.0 response
/// line (we ship it as a JSON string to keep framing symmetric with stdio).
fn decode_jsonrpc_result(reply: &Message) -> Result<serde_json::Value, StartError> {
    // Payload should be a string containing the JSON-RPC line. Accept either
    // a raw object (legacy) or a string.
    let raw: String = match &reply.payload {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let response = wire::decode_response(&raw).map_err(StartError::DecodeHandshake)?;
    match (response.result, response.error) {
        (Some(v), _) => Ok(v),
        (None, Some(e)) => Err(StartError::Handshake(e)),
        (None, None) => Ok(serde_json::Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let o = NatsRuntimeOptions::default();
        assert_eq!(o.heartbeat_grace_factor, 3);
        assert_eq!(o.call_timeout, Duration::from_secs(30));
    }

    #[test]
    fn decode_reply_accepts_string_payload() {
        let msg = Message::new(
            "ext.x.rpc",
            serde_json::Value::String(
                "{\"jsonrpc\":\"2.0\",\"result\":{\"ok\":true},\"id\":1}".into(),
            ),
        );
        let v = decode_jsonrpc_result(&msg).unwrap();
        assert_eq!(v, serde_json::json!({"ok": true}));
    }

    #[test]
    fn decode_reply_surfaces_rpc_error() {
        let msg = Message::new(
            "ext.x.rpc",
            serde_json::Value::String(
                "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-1,\"message\":\"bad\"},\"id\":1}".into(),
            ),
        );
        let err = decode_jsonrpc_result(&msg).unwrap_err();
        let is_handshake = matches!(err, StartError::Handshake(ref r) if r.code == -1);
        assert!(is_handshake, "expected Handshake, got {err:?}");
    }
}
