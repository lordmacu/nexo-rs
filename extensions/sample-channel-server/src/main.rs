//! Phase 80.9 fixture — minimal MCP server that acts as a
//! channel surface for nexo. Designed for end-to-end testing of
//! the channel pipeline without spinning up a real Slack /
//! Telegram / iMessage adapter.
//!
//! What it does:
//!
//! 1. Speaks JSON-RPC over stdio (line-delimited frames).
//! 2. Replies to `initialize` declaring
//!    `experimental['nexo/channel']` AND
//!    `experimental['nexo/channel/permission']` capabilities.
//! 3. Exposes one tool `send_message` that echoes the input —
//!    nexo's `channel_send` LLM tool will hit this when it
//!    relays an outbound reply.
//! 4. On a configurable interval (default 30 s), emits a fake
//!    `notifications/nexo/channel` notification so the operator
//!    can see inbound flowing through the bridge without
//!    needing a real human typing on Slack.
//! 5. When nexo emits `notifications/nexo/channel/permission_request`,
//!    auto-replies with `behavior: "allow"` after a 500 ms
//!    delay — letting operators test the permission-relay race
//!    against the local prompt.
//!
//! Environment variables (all optional):
//!
//! - `NEXO_SAMPLE_CHANNEL_INTERVAL_SECS` — interval between
//!   fake inbound notifications (default 30, `0` disables).
//! - `NEXO_SAMPLE_CHANNEL_AUTO_APPROVE` — `1` (default) or `0`
//!   to disable auto-approval of permission requests.
//! - `NEXO_SAMPLE_CHANNEL_PERMISSION_DELAY_MS` — milliseconds
//!   to wait before auto-approving (default 500).
//! - `NEXO_SAMPLE_CHANNEL_NAME` — what to render in
//!   `meta.user` for fake inbounds (default `"sample"`).
//! - `RUST_LOG` — standard tracing filter.
//!
//! How to wire into nexo's `mcp.yaml`:
//!
//! ```yaml
//! mcp:
//!   enabled: true
//!   servers:
//!     sample-channel:
//!       kind: stdio
//!       command: cargo
//!       args:
//!         - run
//!         - --quiet
//!         - --bin
//!         - sample-channel-server
//!       cwd: /path/to/nexo-rs
//!
//! agents:
//!   - id: kate
#![allow(rustdoc::invalid_rust_codeblocks)]
//!     channels:
//!       enabled: true
//!       approved:
//!         - server: sample-channel
//!     inbound_bindings:
//!       - plugin: telegram
//!         allowed_channel_servers: [sample-channel]
//! ```
//!
//! Then `nexo run --config config/agents.yaml` will surface
//! `sample-channel` in `nexo channel doctor` output, and the
//! agent will start receiving fake `<channel source="sample-channel">…</channel>`
//! user messages on the configured interval.
//!
//! This is a fixture — not production. It auto-approves every
//! permission request with no audit trail. Use it to validate
//! your YAML setup, then swap to a real channel server.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

const SERVER_NAME: &str = "sample-channel-server";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "2025-06-18";

const CHANNEL_NOTIFICATION_METHOD: &str = "notifications/nexo/channel";
const CHANNEL_PERMISSION_RESPONSE_METHOD: &str = "notifications/nexo/channel/permission";
const CHANNEL_PERMISSION_REQUEST_METHOD: &str =
    "notifications/nexo/channel/permission_request";

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcSuccess<T: Serialize> {
    jsonrpc: &'static str,
    id: Value,
    result: T,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    jsonrpc: &'static str,
    id: Value,
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: i32,
    message: String,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification<T: Serialize> {
    jsonrpc: &'static str,
    method: String,
    params: T,
}

/// Shared writer guarded by a mutex so the periodic ticker and
/// the request handler don't interleave bytes on stdout.
type SharedWriter = Arc<Mutex<tokio::io::Stdout>>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    // Logs to stderr — stdout is reserved for JSON-RPC frames.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let interval_secs = std::env::var("NEXO_SAMPLE_CHANNEL_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30);
    let auto_approve = std::env::var("NEXO_SAMPLE_CHANNEL_AUTO_APPROVE")
        .map(|v| v != "0")
        .unwrap_or(true);
    let permission_delay_ms = std::env::var("NEXO_SAMPLE_CHANNEL_PERMISSION_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(500);
    let user_label =
        std::env::var("NEXO_SAMPLE_CHANNEL_NAME").unwrap_or_else(|_| "sample".to_string());

    tracing::info!(
        interval_secs,
        auto_approve,
        permission_delay_ms,
        user_label = %user_label,
        "sample-channel-server booting"
    );

    let writer: SharedWriter = Arc::new(Mutex::new(tokio::io::stdout()));

    // Spawn the ticker that emits fake channel notifications.
    if interval_secs > 0 {
        let writer = writer.clone();
        let user_label_clone = user_label.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // First tick fires immediately — skip it so the
            // ticker doesn't beat the initialize handshake.
            ticker.tick().await;
            let mut counter: u64 = 0;
            loop {
                ticker.tick().await;
                counter += 1;
                let content = format!(
                    "Fake message #{counter} from {user_label_clone}. \
                     If you see this in the agent's transcript, the channel \
                     pipeline works end-to-end."
                );
                let notif = JsonRpcNotification {
                    jsonrpc: "2.0",
                    method: CHANNEL_NOTIFICATION_METHOD.to_string(),
                    params: json!({
                        "content": content,
                        "meta": {
                            "user": user_label_clone,
                            "chat_id": "C_SAMPLE",
                            "thread_ts": format!("{}.{:03}", counter, counter % 1000),
                        }
                    }),
                };
                if let Err(e) = write_frame(&writer, &notif).await {
                    tracing::warn!(error = %e, "ticker: write failed");
                }
            }
        });
    } else {
        tracing::info!("ticker disabled (interval=0)");
    }

    // Stdin reader loop — process JSON-RPC frames one line at a time.
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    while let Some(line) = reader.next_line().await.context("stdin read")? {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(req) => {
                handle_request(req, &writer, auto_approve, permission_delay_ms).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, raw = %line, "invalid jsonrpc frame");
            }
        }
    }
    tracing::info!("stdin closed; exiting");
    Ok(())
}

async fn handle_request(
    req: JsonRpcRequest,
    writer: &SharedWriter,
    auto_approve: bool,
    permission_delay_ms: u64,
) {
    let id = req.id.clone();
    match req.method.as_str() {
        "initialize" => {
            let result = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "experimental": {
                        "nexo/channel": {},
                        "nexo/channel/permission": {},
                    },
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION,
                },
            });
            reply_success(writer, id, result).await;
        }
        "notifications/initialized" => {
            // Fire-and-forget; nothing to do.
        }
        "tools/list" => {
            let result = json!({
                "tools": [
                    {
                        "name": "send_message",
                        "description": "Echo the supplied content back. The sample channel server has no real platform; this exists so nexo's channel_send tool has something to invoke.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "text": { "type": "string", "description": "Message body." },
                                "chat_id": { "type": "string", "description": "Optional chat / thread id." },
                                "thread_ts": { "type": "string", "description": "Optional thread timestamp." }
                            },
                            "required": ["text"]
                        }
                    }
                ]
            });
            reply_success(writer, id, result).await;
        }
        "tools/call" => {
            let name = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Null);
            if name != "send_message" {
                reply_error(
                    writer,
                    id,
                    -32601,
                    format!("unknown tool: {name}"),
                )
                .await;
                return;
            }
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("(no text)");
            tracing::info!(text = %text, "send_message invoked");
            let result = json!({
                "content": [{
                    "type": "text",
                    "text": format!("[sample-channel] echoed: {text}")
                }],
                "isError": false
            });
            reply_success(writer, id, result).await;
        }
        m if m == CHANNEL_PERMISSION_REQUEST_METHOD => {
            // Inbound notification from nexo asking us to surface a
            // permission prompt. Fixtures don't have a human; we
            // auto-approve after a short delay so the operator
            // sees the race-against-local-prompt path work.
            let request_id = req
                .params
                .get("request_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let tool_name = req
                .params
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            tracing::info!(
                request_id = %request_id,
                tool_name = %tool_name,
                auto_approve,
                "permission_request received"
            );
            if !auto_approve {
                tracing::info!("auto_approve disabled — letting the local prompt win");
                return;
            }
            let writer = writer.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(permission_delay_ms))
                    .await;
                let notif = JsonRpcNotification {
                    jsonrpc: "2.0",
                    method: CHANNEL_PERMISSION_RESPONSE_METHOD.to_string(),
                    params: json!({
                        "request_id": request_id,
                        "behavior": "allow",
                    }),
                };
                if let Err(e) = write_frame(&writer, &notif).await {
                    tracing::warn!(error = %e, "permission auto-reply: write failed");
                }
            });
        }
        other => {
            tracing::debug!(method = %other, "unhandled method");
            if id.is_some() {
                reply_error(
                    writer,
                    id,
                    -32601,
                    format!("method not implemented in sample fixture: {other}"),
                )
                .await;
            }
        }
    }
}

async fn reply_success(writer: &SharedWriter, id: Option<Value>, result: Value) {
    let id = id.unwrap_or(Value::Null);
    let env = JsonRpcSuccess {
        jsonrpc: "2.0",
        id,
        result,
    };
    let _ = write_frame(writer, &env).await;
}

async fn reply_error(writer: &SharedWriter, id: Option<Value>, code: i32, message: String) {
    let id = id.unwrap_or(Value::Null);
    let env = JsonRpcError {
        jsonrpc: "2.0",
        id,
        error: ErrorBody { code, message },
    };
    let _ = write_frame(writer, &env).await;
}

async fn write_frame<T: Serialize>(writer: &SharedWriter, payload: &T) -> anyhow::Result<()> {
    let line = serde_json::to_string(payload)?;
    let mut guard = writer.lock().await;
    guard.write_all(line.as_bytes()).await?;
    guard.write_all(b"\n").await?;
    guard.flush().await?;
    Ok(())
}
