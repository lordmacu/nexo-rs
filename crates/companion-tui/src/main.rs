//! Reference companion CLI for the Nexo pairing protocol.
//!
//! FOLLOWUPS PR-4 — minimal scaffold. This binary is the
//! consumer side of `nexo pair start`'s output: takes the
//! base64url setup-code payload (from a QR scan, paste, or
//! `--code <payload>` arg), decodes it, validates the embedded
//! token expiry, and prints a connect plan describing what a
//! full companion would do next.
//!
//! What it does NOT do yet (deferred to PR-4 follow-ups):
//! - Open the WebSocket against `payload.url`.
//! - Present the bootstrap token.
//! - Receive + persist a session token.
//! - Re-render the QR for hands-off scanning workflows.
//!
//! What it does today:
//! - Parses + validates the wire format end-to-end so the
//!   protocol contract is exercised by a non-`nexo-pairing`
//!   consumer (proves third parties can integrate).
//! - Surfaces the URL + token-expiry + device-label / profile
//!   metadata so the operator can sanity-check before the full
//!   companion lands.
//!
//! Usage:
//!     nexo-companion --code <BASE64URL>     # explicit
//!     echo <BASE64URL> | nexo-companion     # stdin
//!     nexo-companion --json                 # machine output

use std::io::Read;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use nexo_pairing::setup_code::{decode_setup_code, token_expires_at};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");

    let code = pick_code(&args)?;
    let payload =
        decode_setup_code(&code).map_err(|e| anyhow!("invalid setup-code payload: {e}"))?;

    let token_expiry = token_expires_at(&payload.bootstrap_token);
    let now = Utc::now();
    let payload_expired = payload.expires_at < now;
    let token_expired = token_expiry.map(|exp| exp < now).unwrap_or(true);

    if json {
        let v = serde_json::json!({
            "url": payload.url,
            "bootstrap_token": payload.bootstrap_token,
            "payload_expires_at": payload.expires_at,
            "token_expires_at": token_expiry,
            "payload_expired": payload_expired,
            "token_expired": token_expired,
            "now": now,
            "next_step": next_step(&payload.url, payload_expired || token_expired),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else {
        println!("Decoded setup-code payload:");
        println!();
        println!("  URL                : {}", payload.url);
        println!(
            "  Bootstrap token    : {}",
            redacted(&payload.bootstrap_token)
        );
        println!("  Payload expires at : {}", payload.expires_at);
        if let Some(exp) = token_expiry {
            println!("  Token expires at   : {}", exp);
        }
        println!("  Now (UTC)          : {}", now);
        println!();
        if payload_expired || token_expired {
            println!("✗ The setup-code is EXPIRED.");
            println!("  Ask the operator to regenerate one with `nexo pair start`.");
            std::process::exit(2);
        }
        println!("✓ Payload is valid. Next step:");
        println!();
        println!("    {}", next_step(&payload.url, false));
        println!();
        println!("(This binary is a reference scaffold — the actual WebSocket");
        println!(" handshake lands in a follow-up PR-4.x. Until then, the URL");
        println!(" + token above are the values your real companion would use.)");
    }

    Ok(())
}

fn pick_code(args: &[String]) -> Result<String> {
    if let Some(i) = args.iter().position(|a| a == "--code") {
        if let Some(v) = args.get(i + 1) {
            return Ok(v.trim().to_string());
        }
        return Err(anyhow!("--code expects a value"));
    }
    // Fallback to stdin so QR-scan tools that pipe their output
    // can use this binary without quoting.
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read stdin")?;
    let trimmed = buf.trim().to_string();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "no setup-code provided (use --code <BASE64URL> or pipe via stdin)"
        ));
    }
    Ok(trimmed)
}

fn redacted(token: &str) -> String {
    // Show only the first 8 + last 4 chars; bootstrap tokens can
    // hit a phone screen log so a user might post one in a
    // bug report. Keep enough to triage a "wrong token" complaint
    // without leaking the full bearer.
    if token.len() <= 16 {
        return "<short>".to_string();
    }
    let head: String = token.chars().take(8).collect();
    let tail: String = token
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn next_step(url: &str, expired: bool) -> String {
    if expired {
        return "Ask the operator to regenerate the setup-code (`nexo pair start`).".to_string();
    }
    format!(
        "Open a WebSocket to {url} and present the bootstrap token in the \
         first frame. Server returns a session token on success."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_short_returns_placeholder() {
        assert_eq!(redacted("abc"), "<short>");
        assert_eq!(redacted("0123456789ABCDEF"), "<short>");
    }

    #[test]
    fn redacted_long_keeps_head_and_tail() {
        let r = redacted("0123456789abcdef0123456789abcdef");
        assert!(r.starts_with("01234567"));
        assert!(r.ends_with("cdef"));
        assert!(r.contains('…'));
    }

    #[test]
    fn next_step_expired_path() {
        let s = next_step("wss://x", true);
        assert!(s.contains("regenerate"));
    }

    #[test]
    fn next_step_active_path() {
        let s = next_step("wss://x.example/pair", false);
        assert!(s.contains("WebSocket"));
        assert!(s.contains("wss://x.example/pair"));
    }
}
