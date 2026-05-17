//! Phase 96 — synthetic poller plugin fixture for the ephemeral
//! lifecycle e2e test. Reads one JSON-RPC envelope from stdin,
//! echoes the tick metadata back as a `TickReply` on stdout, exits.
//!
//! Wire shape (one JSON line each):
//!
//! ```text
//! daemon → stdin:  {"method":"poll_tick","params":{"kind","job_id","agent_id","cursor","config","now","interval_hint_secs"}}
//! stdout → daemon: {"result":{"next_cursor","next_interval_secs","metrics":{"items_seen","items_dispatched"}}}
//! ```
//!
//! Behaviour knobs via env so a single fixture binary covers
//! multiple test scenarios:
//!
//! - `POLLER_ECHO_MODE=ok` (default) — happy-path reply mirroring
//!   the tick request's `items_seen` from `config.echo_items`
//!   (default 1).
//! - `POLLER_ECHO_MODE=permanent_error` — `{"error":{"code":-32002,"message":"echo: simulated permanent error"}}`.
//! - `POLLER_ECHO_MODE=transient_error` — `{"error":{"code":-32001,"message":"echo: simulated transient error"}}`.
//! - `POLLER_ECHO_MODE=malformed` — writes garbage instead of valid
//!   JSON; the daemon side classifies as Transient.
//! - `POLLER_ECHO_SLEEP_MS=<n>` — sleeps `<n>` ms before writing
//!   the reply (test_tick_timeout exercises this against a 1s
//!   `tick_timeout`).
//! - `POLLER_ECHO_CRASH=1` — exits with code 1 BEFORE writing
//!   the reply (tests the spawn-but-no-reply path).

use std::io::{self, BufRead, Write};
use std::time::Duration;

fn main() -> io::Result<()> {
    // Optional crash-before-read — exercises the "child died
    // before consuming stdin" branch.
    if std::env::var("POLLER_ECHO_CRASH_EARLY").as_deref() == Ok("1") {
        std::process::exit(1);
    }

    let stdin = io::stdin().lock();
    let mut line = String::new();
    let mut reader = stdin;
    reader.read_line(&mut line)?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        // Empty stdin → exit clean. Daemon-side classifies as
        // "no reply" → Transient.
        std::process::exit(0);
    }
    let envelope: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => {
            // Daemon sent malformed envelope — return parse-error
            // RPC envelope.
            write_reply(&serde_json::json!({
                "error": { "code": -32700, "message": "echo: parse error on input" }
            }))?;
            return Ok(());
        }
    };

    let params = envelope
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let kind = params
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let job_id = params
        .get("job_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cursor_in = params
        .get("cursor")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(sleep_ms) = std::env::var("POLLER_ECHO_SLEEP_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }

    if std::env::var("POLLER_ECHO_CRASH").as_deref() == Ok("1") {
        std::process::exit(1);
    }

    let mode = std::env::var("POLLER_ECHO_MODE").unwrap_or_else(|_| "ok".to_string());
    let reply = match mode.as_str() {
        "permanent_error" => serde_json::json!({
            "error": { "code": -32002, "message": "echo: simulated permanent error" }
        }),
        "transient_error" => serde_json::json!({
            "error": { "code": -32001, "message": "echo: simulated transient error" }
        }),
        "config_error" => serde_json::json!({
            "error": { "code": -32602, "message": "echo: simulated config error" }
        }),
        "malformed" => {
            io::stdout().write_all(b"not-json\n")?;
            io::stdout().flush()?;
            return Ok(());
        }
        _ => {
            // Default OK reply. echo_items config knob lets the
            // test assert that config flows through.
            let items_seen = params
                .get("config")
                .and_then(|c| c.get("echo_items"))
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u32;
            // echo the kind + job_id into the next_cursor so the
            // round-trip identity is testable end-to-end without a
            // mock host call.
            let cursor_out = format!("{kind}|{job_id}|{}", cursor_in.unwrap_or_default());
            let cursor_b64 = base64_encode(cursor_out.as_bytes());
            serde_json::json!({
                "result": {
                    "next_cursor": cursor_b64,
                    "next_interval_secs": null,
                    "metrics": { "items_seen": items_seen, "items_dispatched": 0 }
                }
            })
        }
    };
    write_reply(&reply)
}

fn write_reply(value: &serde_json::Value) -> io::Result<()> {
    let line = serde_json::to_string(value).expect("reply serialises");
    let mut out = io::stdout().lock();
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

/// URL-safe base64 (no padding). Hand-rolled because the fixture
/// crate intentionally has zero non-stdlib deps beyond `serde_json`
/// (and tokio/serial_test in dev-only).
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((input.len() * 4).div_ceil(3));
    let mut chunks = input.chunks_exact(3);
    for chunk in chunks.by_ref() {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHABET[(n & 0x3F) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        0 => {}
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        }
        _ => unreachable!(),
    }
    out
}
