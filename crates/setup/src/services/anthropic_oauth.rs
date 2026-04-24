//! Anthropic OAuth authorization-code (PKCE) browser flow.
//!
//! Same shape that Claude Code CLI and opencode use. The operator
//! opens an `https://console.anthropic.com/oauth/authorize?...` URL
//! in a browser, logs in with a Claude.ai subscription account,
//! Anthropic redirects to the callback page which displays the
//! `code#state` payload, operator pastes it back here, we exchange
//! it for `{access_token, refresh_token, expires_in}` and persist.
//!
//! Different from MiniMax's `user_code` flow — Anthropic uses the
//! standard OAuth 2.1 authorization_code + PKCE combo.

use std::io::{self, BufRead, Write};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Public Claude Code CLI client_id. Not a secret.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Anthropic's hosted callback page — displays the `code#state`
/// payload so the operator can copy it back.
pub const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
pub const SCOPES: &str = "org:create_api_key user:profile user:inference";

pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub account_email: Option<String>,
}

struct Pkce {
    verifier: String,
    challenge: String,
    state: String,
}

fn gen_pkce() -> Pkce {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    // Claude's OAuth implementation echoes `state` straight into
    // fragments and query strings without re-escaping — any `-` / `_`
    // / `=` upsets its parser and the authorize page returns a
    // generic "Invalid request format" after the user clicks allow.
    // Use hex so the state is [0-9a-f] only.
    let mut state_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut state_bytes);
    let state = state_bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Pkce {
        verifier,
        challenge,
        state,
    }
}

/// Blocking entry-point. The caller may already be inside a tokio
/// runtime (the `agent` binary is `#[tokio::main]`), so we cannot
/// `block_on` on the same thread. Offload to a dedicated std thread
/// with its own current-thread runtime — same pattern Google OAuth
/// uses in `writer.rs`.
pub fn run_flow() -> Result<OAuthToken> {
    std::thread::spawn(|| -> Result<OAuthToken> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for anthropic oauth flow")?;
        rt.block_on(run_flow_async())
    })
    .join()
    .map_err(|_| anyhow::anyhow!("anthropic oauth thread panicked"))?
}

async fn run_flow_async() -> Result<OAuthToken> {
    let pkce = gen_pkce();
    let url = build_authorize_url(&pkce);

    println!();
    println!("┌─────────────── Anthropic Claude OAuth (suscripción) ───────────────┐");
    println!("│                                                                    │");
    println!("│  1. Abre este URL en un navegador logueado en claude.ai:           │");
    println!("│                                                                    │");
    println!("│     {}", url);
    println!("│                                                                    │");
    println!("│  2. Autoriza el acceso. Anthropic te mostrará un código en la      │");
    println!("│     página (formato `<code>#<state>`).                             │");
    println!("│                                                                    │");
    println!("│  3. Pega ese valor completo aquí abajo y presiona ENTER.           │");
    println!("│                                                                    │");
    println!("└────────────────────────────────────────────────────────────────────┘");
    println!();

    // Best-effort browser open — failure is fine, user copies URL.
    let _ = try_open_browser(&url);

    print!("Pega el código (`<code>#<state>`): ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .context("read code from stdin")?;
    let raw = line.trim().to_string();
    if raw.is_empty() {
        bail!("código vacío — abortado");
    }

    let (code, state) = parse_code_payload(&raw)?;
    if state != pkce.state {
        bail!(
            "state mismatch (esperado `{}`, recibido `{}`) — posible CSRF o sesión reutilizada",
            pkce.state,
            state
        );
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": REDIRECT_URI,
            "client_id": CLIENT_ID,
            "code_verifier": pkce.verifier,
            "state": state,
        }))
        .send()
        .await
        .context("POST /v1/oauth/token failed")?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Anthropic /v1/oauth/token HTTP {status}: {text}");
    }
    let parsed: TokenResp = serde_json::from_str(&text)
        .with_context(|| format!("parse token response: {text}"))?;

    let now = chrono::Utc::now().timestamp();
    let ttl = parsed.expires_in.unwrap_or(3600).max(60);
    Ok(OAuthToken {
        access_token: parsed
            .access_token
            .ok_or_else(|| anyhow::anyhow!("response missing access_token"))?,
        refresh_token: parsed
            .refresh_token
            .ok_or_else(|| anyhow::anyhow!("response missing refresh_token"))?,
        expires_at: now + ttl,
        account_email: parsed.account.and_then(|a| a.email_address),
    })
}

fn build_authorize_url(pkce: &Pkce) -> String {
    let params = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPES),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", &pkce.state),
    ];
    let qs: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", pct_encode(k), pct_encode(v)))
        .collect();
    format!("{}?{}", AUTHORIZE_URL, qs.join("&"))
}

/// Minimal percent-encoding for query-string values: escape anything
/// that isn't unreserved per RFC 3986. Enough for our fixed params
/// (scope has spaces, redirect_uri has `:/.`) without pulling in the
/// full `url` crate.
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            // application/x-www-form-urlencoded: space → `+`. Claude's
            // OAuth server accepts this form; strict %20 makes it
            // reject the scope with a generic "Invalid request format".
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// The Anthropic callback page shows `<code>#<state>`. Some users
/// paste the whole callback URL — tolerate either.
fn parse_code_payload(raw: &str) -> Result<(String, String)> {
    let trimmed = raw.trim();
    // If user pasted full URL, pull the fragment/query after `code=`.
    let core = if trimmed.contains("code=") {
        trimmed
            .split_once("code=")
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_else(|| trimmed.to_string())
    } else {
        trimmed.to_string()
    };
    // Strip any leading `#` fragment marker if present.
    let core = core.trim_start_matches('#').to_string();
    let (code, state) = core
        .split_once('#')
        .or_else(|| core.split_once('&'))
        .ok_or_else(|| anyhow::anyhow!("formato inválido — esperado `<code>#<state>`"))?;
    // The state portion might be `state=<x>` if user pasted a URL.
    let state = state.trim_start_matches("state=").to_string();
    Ok((code.to_string(), state))
}

fn try_open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let cmd = "start";
    let status = std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match status {
        Ok(mut child) => {
            let _ = child.wait();
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct TokenResp {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    account: Option<Account>,
}

#[derive(Deserialize)]
struct Account {
    #[serde(default)]
    email_address: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_b64url_sha256_of_verifier() {
        let p = gen_pkce();
        let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(p.verifier.as_bytes()));
        assert_eq!(p.challenge, expect);
    }

    #[test]
    fn authorize_url_has_all_required_params() {
        let p = Pkce {
            verifier: "v".into(),
            challenge: "c".into(),
            state: "s".into(),
        };
        let url = build_authorize_url(&p);
        for needle in [
            "client_id=9d1c250a",
            "response_type=code",
            "redirect_uri=https",
            "code_challenge=c",
            "code_challenge_method=S256",
            "state=s",
            "scope=org",
        ] {
            assert!(url.contains(needle), "missing `{needle}` in: {url}");
        }
    }

    #[test]
    fn parse_code_payload_accepts_hash_form() {
        let (c, s) = parse_code_payload("abc123#mystate").unwrap();
        assert_eq!(c, "abc123");
        assert_eq!(s, "mystate");
    }

    #[test]
    fn parse_code_payload_accepts_full_url() {
        let url =
            "https://console.anthropic.com/oauth/code/callback?code=abc123&state=mystate";
        let (c, s) = parse_code_payload(url).unwrap();
        assert_eq!(c, "abc123");
        assert_eq!(s, "mystate");
    }

    #[test]
    fn parse_code_payload_rejects_plain() {
        assert!(parse_code_payload("justacode").is_err());
    }
}
