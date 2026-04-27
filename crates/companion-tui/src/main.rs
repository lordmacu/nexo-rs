//! Reference companion CLI for the Nexo pairing protocol.
//!
//! Usage:
//!     nexo-companion --code <BASE64URL>     # explicit
//!     echo <BASE64URL> | nexo-companion     # stdin
//!     nexo-companion --json                 # machine output
//!     nexo-companion --no-connect           # decode only (no WS handshake)

mod ws;

use std::io::Read;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use nexo_pairing::setup_code::{decode_setup_code, token_device_label, token_expires_at};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let no_connect = args.iter().any(|a| a == "--no-connect");

    let code = pick_code(&args)?;
    let payload =
        decode_setup_code(&code).map_err(|e| anyhow!("invalid setup-code payload: {e}"))?;

    let token_expiry = token_expires_at(&payload.bootstrap_token);
    let now = Utc::now();
    let payload_expired = payload.expires_at < now;
    let token_expired = token_expiry.map(|exp| exp < now).unwrap_or(true);
    let expired = payload_expired || token_expired;

    if json {
        let v = serde_json::json!({
            "url": payload.url,
            "bootstrap_token": payload.bootstrap_token,
            "payload_expires_at": payload.expires_at,
            "token_expires_at": token_expiry,
            "payload_expired": payload_expired,
            "token_expired": token_expired,
            "now": now,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        if expired {
            std::process::exit(2);
        }
        if !no_connect {
            let device_label = token_device_label(&payload.bootstrap_token)
                .unwrap_or_else(|| "default".to_string());
            let sessions_dir = nexo_home().join("pairing").join("sessions");
            match ws::perform_handshake(&payload, &device_label, &sessions_dir).await {
                Ok(outcome) => {
                    let v2 = serde_json::json!({
                        "paired": true,
                        "token_path": outcome.token_path,
                    });
                    println!("{}", serde_json::to_string_pretty(&v2).unwrap());
                }
                Err(e) => {
                    let v2 = serde_json::json!({"paired": false, "error": e.to_string()});
                    println!("{}", serde_json::to_string_pretty(&v2).unwrap());
                    std::process::exit(1);
                }
            }
        }
        return Ok(());
    }

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

    if expired {
        println!("✗ The setup-code is EXPIRED.");
        println!("  Ask the operator to regenerate one with `nexo pair start`.");
        std::process::exit(2);
    }

    if no_connect {
        println!("✓ Payload valid. (--no-connect: skipping WS handshake)");
        println!();
        println!("  To connect: {}", payload.url);
        return Ok(());
    }

    let device_label =
        token_device_label(&payload.bootstrap_token).unwrap_or_else(|| "default".to_string());
    let sessions_dir = nexo_home().join("pairing").join("sessions");

    println!("✓ Connecting to {} …", payload.url);
    println!();

    match ws::perform_handshake(&payload, &device_label, &sessions_dir).await {
        Ok(outcome) => {
            println!("✓ Paired successfully.");
            println!("  Session token saved to: {}", outcome.token_path.display());
        }
        Err(e) => {
            println!("✗ Pairing failed: {e}");
            std::process::exit(1);
        }
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
    // Fallback to stdin so QR-scan tools that pipe their output can be used.
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

fn nexo_home() -> PathBuf {
    std::env::var("NEXO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".nexo"))
                .unwrap_or_else(|_| PathBuf::from(".nexo"))
        })
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
}
