//! Phase 81.15.a — Child-side helper for out-of-tree subprocess
//! plugins. Pairs with `nexo-core`'s `SubprocessNexoPlugin`
//! host-side adapter (Phase 81.14 + 81.14.b).
//!
//! Plugin authors avoid hand-rolling the JSON-RPC parser, manifest
//! handshake, and broker-publish framing by using
//! [`PluginAdapter`]:
//!
//! ```no_run
//! # #[cfg(feature = "plugin")] {
//! use nexo_microapp_sdk::plugin::{PluginAdapter, BrokerSender};
//! use nexo_broker::Event;
//!
//! const MANIFEST: &str = include_str!("../nexo-plugin.toml");
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     PluginAdapter::new(MANIFEST)?
//!         .on_broker_event(|topic, event, broker: BrokerSender| async move {
//!             // Plugin's outbound logic — e.g. send to Slack API,
//!             // then publish a confirmation event back.
//!             let ack = Event::new(
//!                 format!("plugin.inbound.slack"),
//!                 "slack",
//!                 serde_json::json!({"echo": event.payload}),
//!             );
//!             let _ = broker.publish("plugin.inbound.slack", ack).await;
//!         })
//!         .on_shutdown(|| async { Ok(()) })
//!         .run_stdio()
//!         .await
//! }
//! # }
//! ```
//!
//! # Wire format
//!
//! Same JSON-RPC 2.0 newline-delimited shape as the host adapter:
//! - `initialize` request → `{ manifest, server_version }` reply
//! - `broker.event { topic, event }` notification → user handler
//! - User handler may call `BrokerSender::publish` → emits
//!   `broker.publish` notification on stdout
//! - `shutdown` request → `{ ok: true }` reply, then loop exits
//!
//! # IRROMPIBLE refs
//!
//! - Internal: `crates/microapp-sdk/src/runtime.rs:87-264` —
//!   existing JSON-RPC dispatch loop pattern reused structurally
//!   (different methods, but same line/parse/dispatch shape).
//! - Internal: `crates/core/src/agent/nexo_plugin_registry/subprocess.rs`
//!   — host-side wire spec the SDK must match.
//! - claude-code-leak `src/utils/computerUse/mcpServer.ts` — MCP
//!   child-side server pattern (initialize, notifications without
//!   `id`).
//! - OpenClaw absence: their channel plugins ran in-process Node,
//!   no separate child-side SDK.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use nexo_broker::Event;
use nexo_plugin_manifest::PluginManifest;
use serde_json::{json, Value};
use tokio::io::{
    self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::Mutex;

use crate::errors::{Error as SdkError, Result as SdkResult};

/// Boxed future returned by user-supplied handlers. Avoids forcing
/// downstream authors to import `futures` crate just to type a
/// closure.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Handler invoked when the daemon delivers a `broker.event`
/// notification. Receives the topic, the parsed `Event`, and a
/// [`BrokerSender`] handle so the handler can publish back to the
/// daemon without holding any global state.
///
/// Errors from the handler are intentionally swallowed — same
/// best-effort contract the host adapter uses for `broker.publish`
/// forwarding. A handler that wants to surface errors should log
/// + drop on its own.
pub trait BrokerEventHandler: Send + Sync + 'static {
    /// Process one event delivered from the daemon.
    fn handle(&self, topic: String, event: Event, broker: BrokerSender) -> BoxFuture<'static, ()>;
}

impl<F, Fut> BrokerEventHandler for F
where
    F: Fn(String, Event, BrokerSender) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn handle(&self, topic: String, event: Event, broker: BrokerSender) -> BoxFuture<'static, ()> {
        Box::pin((self)(topic, event, broker))
    }
}

/// Handler invoked when the daemon sends `shutdown`. Called BEFORE
/// the SDK writes the `{ok:true}` reply, so the handler can flush
/// state. Errors propagate as `PluginInitError::Other` on the host
/// side via the `shutdown` reply error path.
pub trait ShutdownHandler: Send + Sync + 'static {
    /// Hook called once at shutdown; return `Ok(())` for clean
    /// exit, `Err(_)` to surface a structured error.
    fn handle(&self) -> BoxFuture<'static, Result<(), String>>;
}

impl<F, Fut> ShutdownHandler for F
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    fn handle(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin((self)())
    }
}

/// Child-side handle for emitting `broker.publish` notifications.
/// Cheap to clone — wraps an `Arc<Mutex<W>>` shared with the
/// dispatch loop's writer. Plugin authors call `.publish()` from
/// inside a `BrokerEventHandler` to push events back to the daemon.
#[derive(Clone)]
pub struct BrokerSender {
    writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
}

impl BrokerSender {
    /// Emit `broker.publish { topic, event }` on stdout. The host
    /// validates the topic against its allowlist before forwarding
    /// to the broker — bad publishes get dropped (with a warn-level
    /// log on the host side).
    pub async fn publish(&self, topic: &str, event: Event) -> SdkResult<()> {
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "broker.publish",
            "params": { "topic": topic, "event": event },
        });
        let line = serde_json::to_string(&frame).map_err(|e| {
            SdkError::Io(io::Error::new(io::ErrorKind::Other, e.to_string()))
        })?;
        let mut writer = self.writer.lock().await;
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }
}

/// Builder for the child-side plugin runtime. Authors call
/// [`PluginAdapter::new`] with their manifest TOML, register
/// `on_broker_event` + `on_shutdown` handlers, then drive the
/// dispatch loop with `run_stdio`.
pub struct PluginAdapter {
    cached_manifest: PluginManifest,
    server_version: String,
    on_broker_event: Option<Arc<dyn BrokerEventHandler>>,
    on_shutdown: Option<Arc<dyn ShutdownHandler>>,
}

impl PluginAdapter {
    /// Parse the bundled manifest. Plugin authors typically pass
    /// the result of `include_str!("../nexo-plugin.toml")`. The
    /// manifest's `plugin.id` becomes the identity the daemon
    /// validates after `initialize`.
    pub fn new(manifest_toml: &str) -> SdkResult<Self> {
        let cached_manifest: PluginManifest = toml::from_str(manifest_toml).map_err(|e| {
            SdkError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("PluginAdapter: parse manifest TOML failed: {e}"),
            ))
        })?;
        let server_version = format!(
            "{}-{}",
            cached_manifest.plugin.id, cached_manifest.plugin.version
        );
        Ok(Self {
            cached_manifest,
            server_version,
            on_broker_event: None,
            on_shutdown: None,
        })
    }

    /// Override the default `server_version` (which defaults to
    /// `<plugin.id>-<plugin.version>` from the manifest). Useful
    /// when the binary's runtime version differs from the manifest
    /// version (e.g. a hot-patched build).
    pub fn with_server_version(mut self, version: impl Into<String>) -> Self {
        self.server_version = version.into();
        self
    }

    /// Register the handler invoked for each `broker.event`
    /// notification the daemon delivers. Without one, events are
    /// silently dropped.
    pub fn on_broker_event<H: BrokerEventHandler>(mut self, handler: H) -> Self {
        self.on_broker_event = Some(Arc::new(handler));
        self
    }

    /// Register the handler invoked when the daemon sends
    /// `shutdown`. Called BEFORE the reply, so the handler can
    /// flush state; an `Err` propagates as JSON-RPC error so the
    /// host surfaces `PluginShutdownError::Other`.
    pub fn on_shutdown<H: ShutdownHandler>(mut self, handler: H) -> Self {
        self.on_shutdown = Some(Arc::new(handler));
        self
    }

    /// Drive the dispatch loop on stdin/stdout until the daemon
    /// sends `shutdown` or stdin reaches EOF.
    pub async fn run_stdio(self) -> SdkResult<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        self.run(BufReader::new(stdin), stdout).await
    }

    /// Drive the dispatch loop on caller-supplied IO. Used by unit
    /// tests via `tokio::io::duplex` and by integration tests that
    /// want to inject mocks.
    pub async fn run<R, W>(self, reader: R, writer: W) -> SdkResult<()>
    where
        R: AsyncBufRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> =
            Arc::new(Mutex::new(Box::new(writer)));
        dispatch_loop(reader, writer, self).await
    }
}

/// Inner dispatch loop. Reads JSON-RPC lines, demuxes by method,
/// invokes user handlers, writes replies. Returns on EOF or
/// `shutdown`.
async fn dispatch_loop<R>(
    reader: R,
    writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    adapter: PluginAdapter,
) -> SdkResult<()>
where
    R: AsyncBufRead + Unpin + Send + 'static,
{
    let mut lines = reader.lines();
    let manifest_value = serde_json::to_value(&adapter.cached_manifest).map_err(|e| {
        SdkError::Io(io::Error::new(io::ErrorKind::Other, e.to_string()))
    })?;
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_error(&writer, None, -32700, &format!("parse error: {e}")).await?;
                continue;
            }
        };
        let id = frame.get("id").cloned();
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let params = frame.get("params").cloned().unwrap_or(Value::Null);

        // Notifications carry no `id`. Today the only one we
        // accept is `broker.event`; everything else is dropped
        // with a debug log.
        if id.is_none() {
            if method == "broker.event" {
                let topic = params
                    .get("topic")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let event_val = params.get("event").cloned().unwrap_or(Value::Null);
                let event: Event = match serde_json::from_value(event_val) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, topic, "broker.event: deserialize Event failed — drop");
                        continue;
                    }
                };
                if let Some(handler) = &adapter.on_broker_event {
                    let sender = BrokerSender {
                        writer: writer.clone(),
                    };
                    handler.handle(topic, event, sender).await;
                }
            } else {
                tracing::debug!(method, "unhandled notification — drop");
            }
            continue;
        }

        match method {
            "initialize" => {
                let result = json!({
                    "manifest": manifest_value,
                    "server_version": adapter.server_version,
                });
                write_result(&writer, id, result).await?;
            }
            "shutdown" => {
                if let Some(handler) = &adapter.on_shutdown {
                    match handler.handle().await {
                        Ok(()) => {
                            write_result(&writer, id, json!({"ok": true})).await?;
                        }
                        Err(e) => {
                            write_error(&writer, id, -32000, &e).await?;
                        }
                    }
                } else {
                    write_result(&writer, id, json!({"ok": true})).await?;
                }
                break;
            }
            other => {
                write_error(
                    &writer,
                    id,
                    -32601,
                    &format!("method not found: {other}"),
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn write_result(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    id: Option<Value>,
    result: Value,
) -> SdkResult<()> {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result,
    });
    write_line(writer, &frame).await
}

async fn write_error(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    id: Option<Value>,
    code: i32,
    message: &str,
) -> SdkResult<()> {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": message },
    });
    write_line(writer, &frame).await
}

async fn write_line(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    frame: &Value,
) -> SdkResult<()> {
    let line = serde_json::to_string(frame)
        .map_err(|e| SdkError::Io(io::Error::new(io::ErrorKind::Other, e.to_string())))?;
    let mut w = writer.lock().await;
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use tokio::io::{duplex, BufReader as TokioBufReader};

    const TEST_MANIFEST: &str = r#"
[plugin]
id = "test_plugin"
version = "0.1.0"
name = "test"
description = "fixture"
min_nexo_version = ">=0.1.0"
"#;

    /// Spawn the adapter on a duplex pipe + return helpers to
    /// drive it from the test's side: write requests, read
    /// replies. The adapter task is moved off so the test can
    /// proceed with assertions.
    async fn run_adapter_on_duplex(
        adapter: PluginAdapter,
    ) -> (
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
        TokioBufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        tokio::task::JoinHandle<SdkResult<()>>,
    ) {
        // Two duplex pipes: host_to_plugin (test writes, adapter
        // reads) + plugin_to_host (adapter writes, test reads).
        let (host_writer_end, plugin_reader_end) = duplex(8192);
        let (plugin_writer_end, host_reader_end) = duplex(8192);
        let plugin_reader = TokioBufReader::new(plugin_reader_end);
        let plugin_writer = plugin_writer_end;
        let join = tokio::spawn(adapter.run(plugin_reader, plugin_writer));
        let (_unused_read, host_write) = tokio::io::split(host_writer_end);
        let (host_read, _unused_write) = tokio::io::split(host_reader_end);
        (host_write, TokioBufReader::new(host_read), join)
    }

    async fn read_reply_line(
        reader: &mut TokioBufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    ) -> Value {
        let mut buf = String::new();
        reader.read_line(&mut buf).await.expect("read reply line");
        serde_json::from_str(buf.trim()).expect("reply parses as JSON")
    }

    #[tokio::test]
    async fn initialize_replies_with_cached_manifest() {
        let adapter = PluginAdapter::new(TEST_MANIFEST).expect("manifest parses");
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["manifest"]["plugin"]["id"], "test_plugin");
        assert_eq!(reply["result"]["server_version"], "test_plugin-0.1.0");
    }

    #[tokio::test]
    async fn broker_event_dispatches_to_user_handler() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(
                move |topic: String, event: Event, _broker: BrokerSender| {
                    let called = called_clone.clone();
                    async move {
                        assert_eq!(topic, "plugin.outbound.test");
                        assert_eq!(event.source, "host");
                        called.store(true, Ordering::SeqCst);
                    }
                },
            );
        let (mut host_write, _host_read, _join) = run_adapter_on_duplex(adapter).await;
        // Fabricate a broker.event notification.
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "broker.event",
            "params": {
                "topic": "plugin.outbound.test",
                "event": {
                    "id": "00000000-0000-0000-0000-000000000010",
                    "timestamp": "2026-05-01T00:00:00Z",
                    "topic": "plugin.outbound.test",
                    "source": "host",
                    "session_id": null,
                    "payload": {"hello": "world"},
                }
            }
        });
        let line = format!("{}\n", serde_json::to_string(&frame).unwrap());
        host_write.write_all(line.as_bytes()).await.unwrap();
        // Give the adapter a tick to process; tighter than 100ms
        // makes flaky.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(called.load(Ordering::SeqCst), "handler must be invoked");
    }

    #[tokio::test]
    async fn broker_sender_writes_publish_notification() {
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_broker_event(
                |_topic: String, event: Event, broker: BrokerSender| async move {
                    let echo = Event::new(
                        "plugin.inbound.test",
                        "plugin",
                        serde_json::json!({"echo": event.payload}),
                    );
                    broker
                        .publish("plugin.inbound.test", echo)
                        .await
                        .expect("publish ok");
                },
            );
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "broker.event",
            "params": {
                "topic": "plugin.outbound.test",
                "event": {
                    "id": "00000000-0000-0000-0000-000000000010",
                    "timestamp": "2026-05-01T00:00:00Z",
                    "topic": "plugin.outbound.test",
                    "source": "host",
                    "session_id": null,
                    "payload": {"foo": "bar"},
                }
            }
        });
        let line = format!("{}\n", serde_json::to_string(&frame).unwrap());
        host_write.write_all(line.as_bytes()).await.unwrap();
        // Read the next outbound line — must be a broker.publish
        // notification carrying the echo payload.
        let reply = read_reply_line(&mut host_read).await;
        assert!(reply.get("id").is_none(), "publish must have NO id");
        assert_eq!(reply["method"], "broker.publish");
        assert_eq!(reply["params"]["topic"], "plugin.inbound.test");
        assert_eq!(reply["params"]["event"]["payload"]["echo"]["foo"], "bar");
    }

    #[tokio::test]
    async fn shutdown_invokes_handler_and_breaks_loop() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_clone = calls.clone();
        let adapter = PluginAdapter::new(TEST_MANIFEST)
            .expect("manifest parses")
            .on_shutdown(move || {
                let calls = calls_clone.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            });
        let (mut host_write, mut host_read, join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"shutdown\",\"params\":{}}\n")
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["ok"], true);
        // Loop must exit after shutdown.
        let res = tokio::time::timeout(std::time::Duration::from_millis(500), join).await;
        assert!(
            res.is_ok(),
            "dispatch loop must exit promptly after shutdown"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_method_returns_neg_32601() {
        let adapter = PluginAdapter::new(TEST_MANIFEST).expect("manifest parses");
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"bogus\",\"params\":{}}\n")
            .await
            .unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["id"], 5);
        assert_eq!(reply["error"]["code"], -32601);
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bogus"));
    }

    #[tokio::test]
    async fn parse_error_returns_neg_32700() {
        let adapter = PluginAdapter::new(TEST_MANIFEST).expect("manifest parses");
        let (mut host_write, mut host_read, _join) = run_adapter_on_duplex(adapter).await;
        host_write.write_all(b"not-json\n").await.unwrap();
        let reply = read_reply_line(&mut host_read).await;
        assert_eq!(reply["error"]["code"], -32700);
    }
}
