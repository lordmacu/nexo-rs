//! Phase 96 — daemon-side plugin poller router.
//!
//! Plugins declare `[plugin.poller] kinds = [...]` in their
//! `nexo-plugin.toml`. The daemon's poller runner consults this
//! router for every job in `pollers.yaml`: if the job's `kind`
//! belongs to a registered plugin, the router builds the broker
//! request topic `<broker_topic_prefix>.tick` and forwards the tick
//! payload via JSON-RPC; the subprocess replies with the same
//! `TickAck` shape the in-process trait returns.
//!
//! Mirrors [`crate::plugin_admin`] / [`crate::plugin_http`]:
//! interior-mutability via `RwLock` so the daemon can construct
//! an empty router at boot and populate it AFTER plugin manifests
//! load + supervisor wires.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nexo_broker::{AnyBroker, BrokerHandle, Message};
use nexo_plugin_manifest::poller::PollerLifecycle;
use nexo_poller::{PollContext, Poller, PollerError, TickAck, TickMetrics};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One registered plugin's poller capability.
#[derive(Debug, Clone)]
pub struct PluginPollerHandle {
    pub plugin_id: String,
    pub kinds: Vec<String>,
    pub broker_topic_prefix: String,
    pub lifecycle: PollerLifecycle,
    pub max_concurrent_ticks: u32,
    pub tick_timeout: Duration,
    /// `[plugin.entrypoint].command` — binary path the daemon
    /// spawns. Required for `ephemeral` lifecycle (each tick spawns
    /// a fresh subprocess). Optional for `long_lived` (the broker
    /// path doesn't need it).
    pub entrypoint_command: Option<String>,
}

impl PluginPollerHandle {
    pub fn tick_topic(&self) -> String {
        format!("{}.tick", self.broker_topic_prefix)
    }
}

/// Daemon-side router. Looks up which plugin owns a `kind` and
/// forwards tick requests via broker JSON-RPC.
#[derive(Debug, Default)]
pub struct PluginPollerRouter {
    /// All registered handles. Lookup is by kind, but we store
    /// handles (not kind→handle map) so a single plugin owning N
    /// kinds is one row.
    handles: std::sync::RwLock<Vec<Arc<PluginPollerHandle>>>,
}

impl PluginPollerRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin's poller capability. Returns an error if
    /// any of the requested `kinds` is already owned by a different
    /// plugin (cross-manifest uniqueness — boot-time fail-fast).
    /// Re-registration of the same `plugin_id` is allowed and
    /// replaces the previous entry (hot-spawn restart).
    pub fn register(
        &self,
        handle: PluginPollerHandle,
    ) -> Result<(), PollerRouteRegistrationError> {
        let mut all = self.handles.write().expect("router lock poisoned");
        for existing in all.iter() {
            if existing.plugin_id == handle.plugin_id {
                // Hot-spawn restart — replaced below.
                continue;
            }
            for k in &handle.kinds {
                if existing.kinds.iter().any(|ek| ek == k) {
                    return Err(PollerRouteRegistrationError::DuplicateKind {
                        kind: k.clone(),
                        existing_plugin: existing.plugin_id.clone(),
                        new_plugin: handle.plugin_id.clone(),
                    });
                }
            }
        }
        all.retain(|h| h.plugin_id != handle.plugin_id);
        all.push(Arc::new(handle));
        Ok(())
    }

    /// Drop the handle for a given plugin id (hot-remove).
    pub fn unregister(&self, plugin_id: &str) -> bool {
        let mut all = self.handles.write().expect("router lock poisoned");
        let before = all.len();
        all.retain(|h| h.plugin_id != plugin_id);
        all.len() != before
    }

    /// Look up the handle that owns `kind`. Returns an `Arc` clone
    /// so the caller can drop the read lock immediately.
    pub fn handle_for_kind(&self, kind: &str) -> Option<Arc<PluginPollerHandle>> {
        let all = self.handles.read().expect("router lock poisoned");
        all.iter()
            .find(|h| h.kinds.iter().any(|k| k == kind))
            .cloned()
    }

    /// Look up the handle owned by `plugin_id`. Used by the daemon
    /// boot loop to wire one `PluginPollerProxy` per declared kind.
    pub fn handles_for_plugin(&self, plugin_id: &str) -> Option<Arc<PluginPollerHandle>> {
        let all = self.handles.read().expect("router lock poisoned");
        all.iter().find(|h| h.plugin_id == plugin_id).cloned()
    }

    /// True when no plugins are registered. Used by the runner's
    /// fast-path skip.
    pub fn is_empty(&self) -> bool {
        self.handles
            .read()
            .expect("router lock poisoned")
            .is_empty()
    }

    /// Number of registered plugins. For metrics.
    pub fn len(&self) -> usize {
        self.handles.read().expect("router lock poisoned").len()
    }
}

/// JSON-RPC tick request payload. The plugin subprocess receives
/// this verbatim and replies with [`TickReply`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickRequest {
    pub kind: String,
    pub job_id: String,
    pub agent_id: String,
    /// Cursor as URL-safe base64 (no padding). Subprocess decodes
    /// before passing to its handler.
    pub cursor: Option<String>,
    pub config: Value,
    /// RFC3339 timestamp.
    pub now: String,
    pub interval_hint_secs: u64,
}

/// Plugin reply to a tick request. Wire-compatible with
/// [`TickAck`] but carries cursor as base64 so the message survives
/// any text-only broker transport.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TickReply {
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub next_interval_secs: Option<u64>,
    #[serde(default)]
    pub metrics: Option<TickMetrics>,
}

impl TickReply {
    /// Decode the reply into the runner-facing [`TickAck`] shape,
    /// translating the base64 cursor back into raw bytes.
    pub fn into_tick_ack(self) -> Result<TickAck, PluginPollerForwardError> {
        let next_cursor = match self.next_cursor {
            Some(s) => Some(decode_cursor(&s)?),
            None => None,
        };
        Ok(TickAck {
            next_cursor,
            next_interval_hint: self.next_interval_secs.map(Duration::from_secs),
            metrics: self.metrics,
        })
    }
}

/// Build the JSON-RPC tick request from runner inputs. Pure fn so
/// the caller (or a future test harness) can exercise the wire
/// shape without a broker.
pub fn build_tick_request(
    kind: &str,
    job_id: &str,
    agent_id: &str,
    cursor: Option<&[u8]>,
    config: Value,
    now: DateTime<Utc>,
    interval_hint: Duration,
) -> TickRequest {
    TickRequest {
        kind: kind.to_string(),
        job_id: job_id.to_string(),
        agent_id: agent_id.to_string(),
        cursor: cursor.map(encode_cursor),
        config,
        now: now.to_rfc3339(),
        interval_hint_secs: interval_hint.as_secs(),
    }
}

/// Forward a tick request to the plugin via broker JSON-RPC. Caller
/// owns the [`TickRequest`] construction so this fn can be a thin
/// IO wrapper. Returns the parsed [`TickReply`]; conversion to
/// [`TickAck`] is the runner's concern.
pub async fn forward_tick(
    broker: &AnyBroker,
    handle: &PluginPollerHandle,
    request: TickRequest,
) -> Result<TickReply, PluginPollerForwardError> {
    let topic = handle.tick_topic();
    let payload = json!({
        "method": "poll_tick",
        "params": request,
    });
    let msg = Message::new(topic.clone(), payload);
    let reply = broker
        .request(&topic, msg, handle.tick_timeout)
        .await
        .map_err(|e| PluginPollerForwardError::Broker(e.to_string()))?;
    serde_json::from_value::<TickReply>(reply.payload).map_err(|e| {
        PluginPollerForwardError::ParseReply(format!(
            "plugin {} returned malformed poll_tick reply: {e}",
            handle.plugin_id
        ))
    })
}

/// Phase 96.E — daemon-side `Poller` impl that spawns a fresh
/// subprocess per tick, drives the tick over stdio JSON-RPC, kills
/// the child on reply. Used when the plugin manifest declares
/// `lifecycle = "ephemeral"`.
///
/// Wire shape (one JSON line each):
///
/// ```text
/// daemon → subprocess.stdin:  {"method":"poll_tick","params":{...TickRequest}}
/// subprocess → daemon.stdout: {"result":{...TickReply}} | {"error":{code,message}}
/// ```
///
/// **Limitations of V1 ephemeral**:
/// - No reverse-RPC during tick. The subprocess receives one input
///   (TickRequest) + writes one output (TickReply). For credential
///   resolution or LLM invocation, use `lifecycle = "long_lived"`.
/// - The subprocess inherits the daemon's `NEXO_BROKER_URL` env so
///   it can publish outbound directly if needed (Phase 92 pattern).
pub struct EphemeralPollerProxy {
    kind: &'static str,
    handle: Arc<PluginPollerHandle>,
}

impl EphemeralPollerProxy {
    /// Wrap a `(handle, kind)` pair for spawn-per-tick dispatch.
    /// `handle.entrypoint_command` MUST be `Some` — the proxy uses
    /// it as the binary path.
    pub fn new(kind: &'static str, handle: Arc<PluginPollerHandle>) -> Self {
        Self { kind, handle }
    }
}

#[async_trait]
impl Poller for EphemeralPollerProxy {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn description(&self) -> &'static str {
        "(plugin v2 subprocess — ephemeral, spawn-per-tick)"
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickAck, PollerError> {
        let command = self.handle.entrypoint_command.as_deref().ok_or_else(|| {
            PollerError::Config {
                job: ctx.job_id.clone(),
                reason: format!(
                    "ephemeral poller '{}' has no [plugin.entrypoint] command",
                    self.handle.plugin_id
                ),
            }
        })?;
        let request = build_tick_request(
            self.kind,
            &ctx.job_id,
            &ctx.agent_id,
            ctx.cursor.as_deref(),
            ctx.config.clone(),
            ctx.now,
            ctx.interval_hint,
        );
        spawn_ephemeral_tick(
            command,
            &self.handle.plugin_id,
            request,
            self.handle.tick_timeout,
            ctx.cancel.clone(),
        )
        .await
    }
}

/// Spawn the binary, write the tick request to stdin, read one
/// JSON line from stdout, await exit, classify the reply.
pub async fn spawn_ephemeral_tick(
    command: &str,
    plugin_id: &str,
    request: TickRequest,
    timeout: Duration,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<TickAck, PollerError> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;

    let request_json = serde_json::to_string(&json!({
        "method": "poll_tick",
        "params": request,
    }))
    .map_err(|e| PollerError::Config {
        job: request.job_id.clone(),
        reason: format!("serialize TickRequest: {e}"),
    })?;

    let mut child = Command::new(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("NEXO_POLLER_EPHEMERAL", "1")
        .env("NEXO_POLLER_PLUGIN_ID", plugin_id)
        .spawn()
        .map_err(|e| PollerError::Transient(anyhow::anyhow!(
            "spawn '{command}' failed: {e}"
        )))?;

    {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            PollerError::Transient(anyhow::anyhow!("subprocess stdin not captured"))
        })?;
        stdin
            .write_all(request_json.as_bytes())
            .await
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
        // Closing stdin signals end-of-input. Subprocess proceeds.
    }

    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| PollerError::Transient(anyhow::anyhow!("stdout not captured")))?,
    );
    let mut line = String::new();
    let read = tokio::select! {
        r = stdout.read_line(&mut line) => r,
        _ = tokio::time::sleep(timeout) => {
            let _ = child.kill().await;
            return Err(PollerError::Transient(anyhow::anyhow!(
                "ephemeral subprocess exceeded tick_timeout ({timeout:?})"
            )));
        }
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(PollerError::Transient(anyhow::anyhow!(
                "ephemeral subprocess cancelled (shutdown or hot-reload)"
            )));
        }
    };
    read.map_err(|e| PollerError::Transient(anyhow::Error::from(e)))?;
    let _ = child.wait().await;

    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(PollerError::Transient(anyhow::anyhow!(
            "ephemeral subprocess wrote no reply"
        )));
    }
    let envelope: Value = serde_json::from_str(trimmed).map_err(|e| {
        PollerError::Transient(anyhow::anyhow!(
            "ephemeral reply parse failed: {e} (line: {trimmed:.200})"
        ))
    })?;
    if let Some(err) = envelope.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603);
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("subprocess error")
            .to_string();
        return Err(match code {
            -32002 => PollerError::Permanent(anyhow::anyhow!("ephemeral: {message}")),
            -32602 => PollerError::Config {
                job: request.job_id.clone(),
                reason: message,
            },
            _ => PollerError::Transient(anyhow::anyhow!("ephemeral rpc {code}: {message}")),
        });
    }
    let result = envelope.get("result").cloned().unwrap_or(Value::Null);
    let reply: TickReply = serde_json::from_value(result).map_err(|e| {
        PollerError::Transient(anyhow::anyhow!("ephemeral TickReply parse: {e}"))
    })?;
    reply
        .into_tick_ack()
        .map_err(|e| PollerError::Transient(anyhow::anyhow!("cursor decode: {e}")))
}

/// Daemon-side `Poller` impl that proxies every tick to a subprocess
/// plugin via broker JSON-RPC. Registered with the runner alongside
/// in-tree builtins so the runner sees a single homogeneous registry.
///
/// `kind()` returns the leaked `&'static str` for one of the
/// plugin's declared kinds; multi-kind plugins register one proxy
/// per kind sharing the same `PluginPollerHandle`.
pub struct PluginPollerProxy {
    kind: &'static str,
    handle: Arc<PluginPollerHandle>,
    broker: AnyBroker,
}

impl PluginPollerProxy {
    /// Wrap a `(handle, kind)` pair into a `Poller` impl. `kind` must
    /// be one of `handle.kinds`; the runner validates this at boot.
    pub fn new(kind: &'static str, handle: Arc<PluginPollerHandle>, broker: AnyBroker) -> Self {
        Self {
            kind,
            handle,
            broker,
        }
    }
}

#[async_trait]
impl Poller for PluginPollerProxy {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn description(&self) -> &'static str {
        "(plugin v2 subprocess via [plugin.poller])"
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickAck, PollerError> {
        let request = build_tick_request(
            self.kind,
            &ctx.job_id,
            &ctx.agent_id,
            ctx.cursor.as_deref(),
            ctx.config.clone(),
            ctx.now,
            ctx.interval_hint,
        );

        let reply = forward_tick(&self.broker, &self.handle, request)
            .await
            .map_err(|e| match e {
                PluginPollerForwardError::Broker(s) => {
                    PollerError::Transient(anyhow::anyhow!("plugin poller broker: {s}"))
                }
                PluginPollerForwardError::ParseReply(s) => {
                    PollerError::Transient(anyhow::anyhow!("plugin poller reply parse: {s}"))
                }
            })?;

        reply.into_tick_ack().map_err(|e| {
            PollerError::Transient(anyhow::anyhow!("plugin poller cursor decode: {e}"))
        })
    }
}

pub fn encode_cursor(raw: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

fn decode_cursor(s: &str) -> Result<Vec<u8>, PluginPollerForwardError> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|e| PluginPollerForwardError::ParseReply(format!("cursor base64: {e}")))
}

#[derive(Debug, thiserror::Error)]
pub enum PollerRouteRegistrationError {
    #[error(
        "kind `{kind}` already owned by plugin `{existing_plugin}` — `{new_plugin}` cannot register"
    )]
    DuplicateKind {
        kind: String,
        existing_plugin: String,
        new_plugin: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PluginPollerForwardError {
    #[error("broker error: {0}")]
    Broker(String),
    #[error("plugin reply parse error: {0}")]
    ParseReply(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(plugin_id: &str, kinds: &[&str], topic: &str) -> PluginPollerHandle {
        PluginPollerHandle {
            plugin_id: plugin_id.into(),
            kinds: kinds.iter().map(|k| (*k).into()).collect(),
            broker_topic_prefix: topic.into(),
            lifecycle: PollerLifecycle::LongLived,
            max_concurrent_ticks: 1,
            tick_timeout: Duration::from_secs(60),
            entrypoint_command: None,
        }
    }

    #[test]
    fn register_and_lookup_single_kind() {
        let r = PluginPollerRouter::new();
        r.register(handle("gcal", &["google_calendar"], "plugin.poller.gcal"))
            .unwrap();
        let h = r.handle_for_kind("google_calendar").expect("found");
        assert_eq!(h.plugin_id, "gcal");
        assert!(r.handle_for_kind("unknown").is_none());
    }

    #[test]
    fn register_one_plugin_with_multiple_kinds() {
        let r = PluginPollerRouter::new();
        r.register(handle("google", &["gmail", "google_calendar"], "plugin.google"))
            .unwrap();
        assert!(r.handle_for_kind("gmail").is_some());
        assert!(r.handle_for_kind("google_calendar").is_some());
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn register_rejects_duplicate_kind_across_plugins() {
        let r = PluginPollerRouter::new();
        r.register(handle("gcal_a", &["google_calendar"], "plugin.a"))
            .unwrap();
        let err = r
            .register(handle("gcal_b", &["google_calendar"], "plugin.b"))
            .expect_err("dup kind rejected");
        match err {
            PollerRouteRegistrationError::DuplicateKind { kind, .. } => {
                assert_eq!(kind, "google_calendar");
            }
        }
    }

    #[test]
    fn register_same_plugin_id_replaces_previous() {
        let r = PluginPollerRouter::new();
        r.register(handle("gcal", &["google_calendar"], "plugin.poller.v1"))
            .unwrap();
        r.register(handle("gcal", &["google_calendar"], "plugin.poller.v2"))
            .expect("replace allowed");
        assert_eq!(r.len(), 1);
        let h = r.handle_for_kind("google_calendar").unwrap();
        assert_eq!(h.broker_topic_prefix, "plugin.poller.v2");
    }

    #[test]
    fn unregister_drops_handle() {
        let r = PluginPollerRouter::new();
        r.register(handle("rss", &["rss"], "plugin.poller.rss"))
            .unwrap();
        assert!(r.unregister("rss"));
        assert!(r.is_empty());
        assert!(!r.unregister("rss"));
    }

    #[test]
    fn build_tick_request_serializes_cursor_b64() {
        let req = build_tick_request(
            "rss",
            "job-1",
            "ana",
            Some(b"hello"),
            json!({"k": "v"}),
            DateTime::parse_from_rfc3339("2026-05-17T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            Duration::from_secs(300),
        );
        assert_eq!(req.kind, "rss");
        assert_eq!(req.cursor.as_deref(), Some("aGVsbG8"));
        assert_eq!(req.now, "2026-05-17T10:00:00+00:00");
        assert_eq!(req.interval_hint_secs, 300);
    }

    #[test]
    fn build_tick_request_omits_cursor_when_none() {
        let req = build_tick_request(
            "rss",
            "job-1",
            "ana",
            None,
            Value::Null,
            Utc::now(),
            Duration::from_secs(60),
        );
        assert!(req.cursor.is_none());
    }

    #[test]
    fn tick_reply_decodes_cursor_round_trip() {
        let reply = TickReply {
            next_cursor: Some(encode_cursor(b"world")),
            next_interval_secs: Some(120),
            metrics: Some(TickMetrics {
                items_seen: 5,
                items_dispatched: 2,
            }),
        };
        let ack = reply.into_tick_ack().unwrap();
        assert_eq!(ack.next_cursor.as_deref(), Some(b"world".as_slice()));
        assert_eq!(ack.next_interval_hint, Some(Duration::from_secs(120)));
        let m = ack.metrics.unwrap();
        assert_eq!(m.items_seen, 5);
        assert_eq!(m.items_dispatched, 2);
    }

    #[test]
    fn tick_reply_handles_empty() {
        let reply = TickReply::default();
        let ack = reply.into_tick_ack().unwrap();
        assert!(ack.next_cursor.is_none());
        assert!(ack.next_interval_hint.is_none());
        assert!(ack.metrics.is_none());
    }

    #[test]
    fn tick_reply_bad_cursor_b64_errors() {
        let reply = TickReply {
            next_cursor: Some("!!not_b64!!".into()),
            ..TickReply::default()
        };
        let err = reply.into_tick_ack().unwrap_err();
        assert!(matches!(err, PluginPollerForwardError::ParseReply(_)));
    }

    #[test]
    fn handle_tick_topic_appends_dot_tick() {
        let h = handle("rss", &["rss"], "plugin.poller.rss");
        assert_eq!(h.tick_topic(), "plugin.poller.rss.tick");
    }
}
