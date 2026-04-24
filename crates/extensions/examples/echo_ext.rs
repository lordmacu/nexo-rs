//! Test helper: a trivial stdio extension that speaks JSON-RPC 2.0.
//!
//! - `initialize` → returns one tool named `echo`.
//! - `tools/call name=echo` → returns `{ echoed: arguments }`.
//! - `tools/list`          → returns `{ tools: [...] }`.
//! - `shutdown`            → exits cleanly.
//! - unknown method        → returns `-32601 method not found`.

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut reader = stdin.lock();

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_error(&mut stdout, None, -32700, &format!("parse error: {e}"));
                continue;
            }
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(serde_json::json!({}));

        match method {
            "initialize" => {
                write_result(
                    &mut stdout,
                    id,
                    serde_json::json!({
                        "server_version": "echo-0.1.0",
                        "tools": [
                            {
                                "name": "echo",
                                "description": "Echoes its arguments",
                                "input_schema": {"type": "object", "additionalProperties": true}
                            }
                        ],
                        "hooks": ["before_message", "before_tool_call"]
                    }),
                );
            }
            "tools/list" => {
                write_result(
                    &mut stdout,
                    id,
                    serde_json::json!({
                        "tools": [
                            {
                                "name": "echo",
                                "description": "Echoes its arguments",
                                "input_schema": {"type": "object", "additionalProperties": true}
                            }
                        ]
                    }),
                );
            }
            m if m.starts_with("hooks/") => {
                // Phase 11.6 — test hook handler. Defaults to {abort:false}.
                // If the event payload contains `test_abort: true`, the hook
                // returns `{abort:true, reason:"test abort"}` so integration
                // tests can exercise the abort short-circuit path.
                let abort = params
                    .get("test_abort")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let result = if abort {
                    serde_json::json!({"abort": true, "reason": "test abort"})
                } else {
                    serde_json::json!({"abort": false})
                };
                write_result(&mut stdout, id, result);
            }
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if name != "echo" {
                    write_error(&mut stdout, id, -32601, &format!("unknown tool `{name}`"));
                    continue;
                }
                let args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                write_result(&mut stdout, id, serde_json::json!({ "echoed": args }));
            }
            "shutdown" => {
                // Notifications have no id; either way, exit.
                if let Some(id_val) = id {
                    write_result(&mut stdout, Some(id_val), serde_json::json!({"ok": true}));
                }
                break;
            }
            other => {
                write_error(&mut stdout, id, -32601, &format!("method not found: {other}"));
            }
        }
    }
}

fn write_result(
    out: &mut impl Write,
    id: Option<serde_json::Value>,
    result: serde_json::Value,
) {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "result": result,
    });
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

fn write_error(
    out: &mut impl Write,
    id: Option<serde_json::Value>,
    code: i32,
    message: &str,
) {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "error": { "code": code, "message": message },
    });
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}
