//! Starter stdio JSON-RPC extension for agent-rs.
//!
//! Line-delimited JSON-RPC 2.0. `initialize` → `tools/list` → `tools/call` →
//! `shutdown`. Swap the sample tools in `tools.rs` for your own.

mod protocol;
mod tools;

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const SERVER_VERSION: &str = "template-rust-0.1.0";

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
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                protocol::write_error(&mut stdout, None, -32700, &format!("parse error: {e}"));
                continue;
            }
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));

        match method {
            "initialize" => protocol::write_result(
                &mut stdout,
                id,
                json!({
                    "server_version": SERVER_VERSION,
                    "tools": tools::tool_schemas(),
                    "hooks": ["before_message"],
                }),
            ),
            "tools/list" => protocol::write_result(
                &mut stdout,
                id,
                json!({ "tools": tools::tool_schemas() }),
            ),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match tools::dispatch(name, &args) {
                    Ok(result) => protocol::write_result(&mut stdout, id, result),
                    Err(msg) => protocol::write_error(&mut stdout, id, -32000, &msg),
                }
            }
            m if m.starts_with("hooks/") => {
                // Observer-only hook handler — log to stderr, allow through.
                eprintln!("[template-rust] hook {}: {}", m, params);
                protocol::write_result(&mut stdout, id, json!({"abort": false}));
            }
            "shutdown" => {
                if let Some(id_val) = id {
                    protocol::write_result(&mut stdout, Some(id_val), json!({"ok": true}));
                }
                let _ = stdout.flush();
                break;
            }
            other => {
                protocol::write_error(
                    &mut stdout,
                    id,
                    -32601,
                    &format!("method not found: {other}"),
                );
            }
        }
    }
}
