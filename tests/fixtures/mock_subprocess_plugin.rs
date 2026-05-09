//! Phase 81.18.b.e2e-mock-binary — tiny synthetic subprocess plugin
//! that speaks the JSON-RPC newline-delimited protocol the daemon's
//! `SubprocessNexoPlugin` expects, but with NO Telegram / WhatsApp
//! / wa-agent dependencies. Used by `tests/subprocess_flip_e2e.rs`
//! to validate the daemon-side spawn wire shape (env passthrough,
//! initialize handshake, shutdown reply) without dragging the real
//! plugin binaries + their network dependencies into CI.
//!
//! Wire shape:
//! - `initialize` request (id=1) → reply
//!   `{ "manifest": <minimal>, "server_version": "mock-0.0.1", "tools": [] }`
//! - `shutdown` request → `{ "ok": true }` then exit clean.
//!
//! Optional: passing the env var
//! `MOCK_SUBPROCESS_PLUGIN_ECHO_ENV=1` makes the binary write a
//! single line to stderr enumerating every `NEXO_PLUGIN_*` env var
//! it sees. Tests assert on that stderr line to verify per-spawn
//! env isolation.

use std::io::{self, BufRead, Write};

/// Manifest pre-encoded as JSON so the mock binary doesn't need a
/// TOML parser dep — the daemon only consumes the JSON-RPC reply
/// shape, never the raw TOML.
fn manifest_value() -> serde_json::Value {
    serde_json::json!({
        "plugin": {
            "id": "mock_subprocess_plugin",
            "version": "0.0.1",
            "name": "Mock Subprocess Plugin",
            "description": "JSON-RPC fixture for proyecto/tests/subprocess_flip_e2e.rs",
            "min_nexo_version": ">=0.1.0"
        }
    })
}

fn main() -> io::Result<()> {
    if std::env::var("MOCK_SUBPROCESS_PLUGIN_ECHO_ENV").as_deref() == Ok("1") {
        let mut keys: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("NEXO_PLUGIN_"))
            .collect();
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        let line: Vec<String> = keys.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
        eprintln!("MOCK_ENV: {}", line.join(" "));
    }

    let manifest_value: serde_json::Value = manifest_value();

    let stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    for raw in stdin.lines() {
        let raw = raw?;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("MOCK_ERR: bad json: {e}");
                continue;
            }
        };
        let id = parsed.get("id").cloned();
        let method = parsed.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let reply = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "manifest": manifest_value,
                    "server_version": "mock-0.0.1",
                    "tools": [],
                }
            }),
            "shutdown" => {
                let ack = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "ok": true }
                });
                writeln!(stdout, "{ack}")?;
                stdout.flush()?;
                return Ok(());
            }
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("mock subprocess plugin: method {method:?} not supported")
                }
            }),
        };
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
    }
    Ok(())
}
