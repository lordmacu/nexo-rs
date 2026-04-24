//! Mock MCP server used by `tests/mock_server_test.rs`.
//!
//! Behaviour is controlled by env vars so a single binary covers every test
//! scenario:
//!
//! - `MOCK_MODE=happy` (default) — answers `initialize`, `tools/list`,
//!   `tools/call` with canned payloads.
//! - `MOCK_MODE=silent_initialize` — never answers `initialize` so the client
//!   times out.
//! - `MOCK_MODE=paginate` — `tools/list` returns two pages.
//! - `MOCK_MODE=server_error` — `tools/call` responds with a JSON-RPC error.
//! - `MOCK_MODE=tool_error` — `tools/call` responds with `isError=true`.
//! - `MOCK_MODE=notify_burst` — emits a `notifications/tools/list_changed`
//!   before responding to `tools/call`.
//! - `MOCK_MODE=notify_resources` — emits a
//!   `notifications/resources/list_changed` before responding to `tools/call`.
//! - `MOCK_MODE=slow_call` — responds to `tools/call` only after a 500 ms
//!   delay so the parent has time to call `shutdown` while it is pending.
//!   If env var `MOCK_CANCELLED_LOG` is set, every received
//!   `notifications/cancelled` is appended to that file as one JSON line.
//! - `MOCK_MODE=emit_log` — pushes a `notifications/message` at level
//!   `info` before responding to `tools/call`. Lets clients verify MCP
//!   logging capability plumbing end-to-end.
//! - `MOCK_MODE=echo_meta` — echoes `params._meta` back as the
//!   content[0].text of the tools/call response. Lets clients verify
//!   `_meta` propagation on request params.
//! - `MOCK_MODE=logging_capable` — advertises `capabilities.logging` in
//!   the initialize result and accepts `logging/setLevel` requests with
//!   an empty-object result. If env `MOCK_SETLEVEL_LOG` is set, the
//!   received level is appended to that file.

use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};

fn main() {
    let mode = std::env::var("MOCK_MODE").unwrap_or_else(|_| "happy".into());
    let cancelled_log = std::env::var("MOCK_CANCELLED_LOG").ok();
    let setlevel_log = std::env::var("MOCK_SETLEVEL_LOG").ok();
    let sampling_log = std::env::var("MOCK_SAMPLING_LOG").ok();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut handle = stdin.lock();

    loop {
        let mut line = String::new();
        match handle.read_line(&mut line) {
            Ok(0) => break, // EOF — parent closed stdin
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned();

        // Notifications (no id) are fire-and-forget from our perspective.
        let Some(id_value) = id else {
            if method == "notifications/cancelled" {
                if let Some(path) = &cancelled_log {
                    if let Ok(mut f) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                    {
                        let _ = writeln!(f, "{trimmed}");
                    }
                }
            }
            if method == "notifications/initialized" && mode == "sampling_trigger" {
                // Server-originated request: ask the client to sample.
                let line = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 9001,
                    "method": "sampling/createMessage",
                    "params": {
                        "messages": [{
                            "role": "user",
                            "content": {"type":"text","text":"ping from mock"}
                        }],
                        "maxTokens": 64,
                    }
                });
                writeln!(out, "{line}").ok();
                out.flush().ok();
            }
            continue;
        };

        // Sampling mode: a line with an id AND no method = client's
        // response to our `sampling/createMessage` request. Log and
        // continue (no reply to emit).
        if mode == "sampling_trigger" && method.is_empty() {
            if let Some(path) = &sampling_log {
                if let Ok(mut f) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(f, "{trimmed}");
                }
            }
            continue;
        }

        match (mode.as_str(), method) {
            (_, "initialize") if mode == "silent_initialize" => {
                // Drop the request.
            }
            ("logging_capable", "initialize") => {
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}, "logging": {}},
                        "serverInfo": {"name":"mock","version":"0.1.0"}
                    }),
                );
            }
            ("logging_capable", "logging/setLevel") => {
                if let Some(path) = &setlevel_log {
                    let level = req
                        .get("params")
                        .and_then(|p| p.get("level"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if let Ok(mut f) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                    {
                        let _ = writeln!(f, "{level}");
                    }
                }
                respond(&mut out, &id_value, serde_json::json!({}));
            }
            ("resources", "initialize") => {
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}, "resources": {}},
                        "serverInfo": {"name":"mock","version":"0.1.0"}
                    }),
                );
            }
            ("resources", "resources/list") => {
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "resources": [
                            {"uri":"file:///readme","name":"readme","description":"top-level","mimeType":"text/plain","annotations":{"audience":["assistant"],"priority":0.9}},
                            {"uri":"file:///config","name":"config","mimeType":"application/json"},
                            {"uri":"file:///blob","name":"blob","mimeType":"image/png"}
                        ]
                    }),
                );
            }
            ("resources", "resources/templates/list") => {
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "resourceTemplates": [
                            {"uriTemplate":"file:///{path}","name":"file","description":"local file by path","mimeType":"text/plain"},
                            {"uriTemplate":"log://{date}/{level}","name":"log","mimeType":"text/plain"}
                        ]
                    }),
                );
            }
            ("resources", "resources/read") => {
                let uri = req
                    .get("params")
                    .and_then(|p| p.get("uri"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("");
                if uri == "file:///readme" {
                    respond(
                        &mut out,
                        &id_value,
                        serde_json::json!({
                            "contents":[
                                {"uri":uri,"mimeType":"text/plain","text":"hello"}
                            ]
                        }),
                    );
                } else if uri == "file:///blob" {
                    respond(
                        &mut out,
                        &id_value,
                        serde_json::json!({
                            "contents":[
                                {"uri":uri,"mimeType":"image/png","blob":"aGVsbG8="}
                            ]
                        }),
                    );
                } else {
                    respond_error(&mut out, &id_value, -32002, "resource not found");
                }
            }
            ("echo_meta", "initialize") => {
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}, "resources": {}},
                        "serverInfo": {"name":"mock","version":"0.1.0"}
                    }),
                );
            }
            (_, "initialize") => {
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {"listChanged": true}},
                        "serverInfo": {"name":"mock","version":"0.1.0"}
                    }),
                );
            }
            ("paginate", "tools/list") => {
                let cursor = req
                    .get("params")
                    .and_then(|p| p.get("cursor"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                if cursor.is_empty() {
                    respond(
                        &mut out,
                        &id_value,
                        serde_json::json!({
                            "tools": [{"name":"alpha","inputSchema":{"type":"object"}}],
                            "nextCursor": "p2"
                        }),
                    );
                } else {
                    respond(
                        &mut out,
                        &id_value,
                        serde_json::json!({
                            "tools": [{"name":"beta","inputSchema":{"type":"object"}}],
                            "nextCursor": null
                        }),
                    );
                }
            }
            (_, "tools/list") => {
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "tools": [
                            {
                                "name":"echo",
                                "description":"echoes the input",
                                "inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}
                            },
                            {
                                "name":"ping",
                                "inputSchema":{"type":"object"}
                            }
                        ]
                    }),
                );
            }
            ("server_error", "tools/call") => {
                respond_error(&mut out, &id_value, -32001, "forbidden");
            }
            ("tool_error", "tools/call") => {
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "content":[{"type":"text","text":"file not found"}],
                        "isError": true
                    }),
                );
            }
            ("notify_burst", "tools/call") => {
                // Notification before the response.
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "jsonrpc":"2.0",
                        "method":"notifications/tools/list_changed",
                        "params":{}
                    })
                )
                .ok();
                out.flush().ok();
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "content":[{"type":"text","text":"ok"}],
                        "isError": false
                    }),
                );
            }
            ("notify_resources", "tools/call") => {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "jsonrpc":"2.0",
                        "method":"notifications/resources/list_changed",
                        "params":{}
                    })
                )
                .ok();
                out.flush().ok();
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "content":[{"type":"text","text":"ok"}],
                        "isError": false
                    }),
                );
            }
            ("echo_meta", "resources/list") => {
                let meta = req
                    .get("params")
                    .and_then(|p| p.get("_meta"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "resources": [
                            {"uri": format!("meta://{}", meta), "name":"meta-echo"}
                        ]
                    }),
                );
            }
            ("echo_meta", "resources/read") => {
                let meta = req
                    .get("params")
                    .and_then(|p| p.get("_meta"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "contents": [
                            {"uri":"meta://read","mimeType":"text/plain","text": meta.to_string()}
                        ]
                    }),
                );
            }
            ("echo_meta", "tools/call") => {
                let meta = req
                    .get("params")
                    .and_then(|p| p.get("_meta"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "content":[{"type":"text","text":meta.to_string()}],
                        "isError": false
                    }),
                );
            }
            ("emit_log", "tools/call") => {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/message",
                        "params": {
                            "level": "info",
                            "logger": "mock-core",
                            "data": "tool invoked"
                        }
                    })
                )
                .ok();
                out.flush().ok();
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "content":[{"type":"text","text":"ok"}],
                        "isError": false
                    }),
                );
            }
            ("slow_call", "tools/call") => {
                std::thread::sleep(std::time::Duration::from_millis(500));
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "content":[{"type":"text","text":"slow ok"}],
                        "isError": false
                    }),
                );
            }
            (_, "tools/call") => {
                let args = req
                    .get("params")
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                respond(
                    &mut out,
                    &id_value,
                    serde_json::json!({
                        "content":[{"type":"text","text":format!("echo: {args}")}],
                        "isError": false
                    }),
                );
            }
            _ => {
                respond_error(&mut out, &id_value, -32601, "method not found");
            }
        }
    }
}

fn respond<W: Write>(out: &mut W, id: &serde_json::Value, result: serde_json::Value) {
    let line = serde_json::json!({
        "jsonrpc":"2.0",
        "result": result,
        "id": id,
    });
    writeln!(out, "{}", line).ok();
    out.flush().ok();
}

fn respond_error<W: Write>(out: &mut W, id: &serde_json::Value, code: i32, message: &str) {
    let line = serde_json::json!({
        "jsonrpc":"2.0",
        "error": {"code": code, "message": message},
        "id": id,
    });
    writeln!(out, "{}", line).ok();
    out.flush().ok();
}
