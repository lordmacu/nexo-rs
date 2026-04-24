//! Integration tests for Phase 11.4 — `NatsRuntime` speaks JSON-RPC 2.0
//! over `BrokerHandle` request/reply.

use std::sync::Arc;
use std::time::Duration;

use agent_broker::{BrokerHandle, Event, LocalBroker, Message};
use agent_extensions::runtime::{NatsRuntime, NatsRuntimeOptions, RuntimeState};

/// Spawn a task that pretends to be the extension process: it subscribes to
/// `ext.{id}.rpc`, decodes each JSON-RPC request, and replies on
/// `reply_to`. `responder` produces the result value for each method call.
///
/// Subscribes synchronously before returning so the caller can safely issue
/// requests without racing with the subscription install.
async fn spawn_mock_extension<F>(
    broker: Arc<dyn BrokerHandle>,
    subject: String,
    mut responder: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnMut(&str, serde_json::Value) -> serde_json::Value + Send + 'static,
{
    let mut sub = broker.subscribe(&subject).await.expect("subscribe");
    tokio::spawn(async move {
        while let Some(ev) = sub.next().await {
            let msg: Message = match serde_json::from_value(ev.payload) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let reply_to = match msg.reply_to.clone() {
                Some(r) => r,
                None => continue,
            };
            // Inner JSON-RPC line is a JSON string.
            let line = match &msg.payload {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let req: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let id = req.get("id").cloned().unwrap_or(serde_json::json!(null));
            let params = req.get("params").cloned().unwrap_or(serde_json::json!({}));

            let result = responder(method, params);
            let response_line = serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "result": result,
                "id": id,
            }))
            .unwrap();
            let reply_payload = Message::new(
                reply_to.clone(),
                serde_json::Value::String(response_line),
            );
            let reply_event = Event::new(
                reply_to.clone(),
                "mock-extension",
                serde_json::to_value(&reply_payload).unwrap(),
            );
            let _ = broker.publish(&reply_to, reply_event).await;
        }
    })
}

fn fast_opts() -> NatsRuntimeOptions {
    NatsRuntimeOptions {
        call_timeout: Duration::from_millis(500),
        handshake_timeout: Duration::from_millis(500),
        heartbeat_interval: Duration::from_millis(100),
        heartbeat_grace_factor: 3,
        shutdown_grace: Duration::from_millis(100),
    }
}

#[tokio::test]
async fn connect_handshake_exposes_tool_descriptors() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    let _mock = spawn_mock_extension(broker.clone(), "ext.weather.rpc".into(), |method, _| {
        match method {
            "initialize" => serde_json::json!({
                "server_version": "0.1.0",
                "tools": [{
                    "name": "get_weather",
                    "description": "returns a fake forecast",
                    "input_schema": {"type": "object"}
                }],
                "hooks": []
            }),
            "tools/list" => serde_json::json!({
                "tools": [{
                    "name": "get_weather",
                    "description": "returns a fake forecast",
                    "input_schema": {"type": "object"}
                }]
            }),
            "tools/call" => serde_json::json!({"content": "22C"}),
            _ => serde_json::json!(null),
        }
    })
    .await;

    let runtime = NatsRuntime::connect(broker.clone(), "weather", "ext", fast_opts())
        .await
        .expect("connect");

    assert_eq!(runtime.extension_id(), "weather");
    assert!(matches!(runtime.state(), RuntimeState::Ready));
    assert_eq!(runtime.handshake().tools.len(), 1);
    assert_eq!(runtime.handshake().tools[0].name, "get_weather");

    let tools = runtime.tools_list().await.expect("tools_list");
    assert_eq!(tools.len(), 1);

    let out = runtime
        .tools_call("get_weather", serde_json::json!({"city": "Bogota"}))
        .await
        .expect("tools_call");
    assert_eq!(out, serde_json::json!({"content": "22C"}));
}

#[tokio::test]
async fn call_times_out_when_extension_drops_request() {
    let broker: Arc<dyn BrokerHandle> = Arc::new(LocalBroker::new());
    // One responder: handles initialize, drops everything after.
    let broker_clone = broker.clone();
    let mut sub = broker_clone.subscribe("ext.silent.rpc").await.unwrap();
    tokio::spawn(async move {
        while let Some(ev) = sub.next().await {
            let msg: Message = match serde_json::from_value(ev.payload) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let Some(reply_to) = msg.reply_to.clone() else { continue };
            let line = match &msg.payload {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let req: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
            if method != "initialize" {
                // Silent drop; runtime must time out.
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
                "silent-mock",
                serde_json::to_value(&reply_payload).unwrap(),
            );
            let _ = broker_clone.publish(&reply_to, reply_event).await;
        }
    });

    let opts = NatsRuntimeOptions {
        call_timeout: Duration::from_millis(150),
        ..fast_opts()
    };
    let runtime = NatsRuntime::connect(broker.clone(), "silent", "ext", opts)
        .await
        .expect("handshake must succeed");

    let err = runtime
        .tools_call("noop", serde_json::json!({}))
        .await
        .expect_err("must time out");
    let msg = format!("{err}");
    assert!(
        msg.contains("timed out") || msg.contains("broker error"),
        "unexpected error: {msg}"
    );
}
