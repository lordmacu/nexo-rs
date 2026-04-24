use std::io::{BufRead, Write};

use serde_json::{json, Value};

use weather::{protocol, tools};

const SERVER_VERSION: &str = "weather-0.2.0";

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut reader = stdin.lock();

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
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
                    "hooks": [],
                }),
            ),
            "tools/list" => {
                protocol::write_result(&mut stdout, id, json!({ "tools": tools::tool_schemas() }))
            }
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match tools::dispatch(name, &args) {
                    Ok(result) => protocol::write_result(&mut stdout, id, result),
                    Err(err) => {
                        protocol::write_error(&mut stdout, id, err.code, &err.message)
                    }
                }
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
