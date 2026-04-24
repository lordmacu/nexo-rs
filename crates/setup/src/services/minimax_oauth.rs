//! MiniMax Token Plan OAuth (PKCE user-code flow).
//!
//! Mirrors the reference implementation in
//! `research/extensions/minimax/oauth.ts`. Runs against the **same**
//! shared `client_id` that OpenClaw uses — the ID is not a secret, it
//! just identifies the app to MiniMax's portal for user consent.
//!
//! High-level flow:
//!
//! 1. Generate PKCE verifier + challenge + `state`.
//! 2. `POST /oauth/code` → returns `{user_code, verification_uri, expired_in}`.
//! 3. Show the URL + code to the operator (they approve in-browser).
//! 4. Poll `POST /oauth/token` every ~2s until `status=success` or expiry.
//! 5. Return `{access_token, refresh_token, expires_at_unix}` to the caller.
//!
//! The wizard persists these three fields as three `0600` files under
//! `secrets/`. A follow-up wires automatic refresh into
//! `crates/llm/src/minimax.rs`; until then, the operator can paste
//! `access_token` into `MINIMAX_API_KEY` for manual use (lasts a few
//! hours).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const SHARED_CLIENT_ID: &str = "78257093-7e40-4613-99e0-527b14b39113";
pub const SCOPE: &str = "group_id profile model.completion";
pub const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:user_code";

#[derive(Debug, Clone, Copy)]
pub enum Region {
    Global,
    Cn,
}

impl Region {
    pub fn base_url(self) -> &'static str {
        match self {
            Self::Global => "https://api.minimax.io",
            Self::Cn => "https://api.minimaxi.com",
        }
    }
}

pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: String,
    /// Seconds-since-epoch for when the access token stops working.
    pub expires_at: i64,
    pub notification_message: Option<String>,
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
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        Sha256::digest(verifier.as_bytes()),
    );
    let mut state_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut state_bytes);
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes);
    Pkce {
        verifier,
        challenge,
        state,
    }
}

#[derive(Deserialize)]
struct CodeResp {
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    expired_in: i64,
    #[serde(default)]
    interval: Option<u64>,
    state: String,
}

#[derive(Deserialize)]
struct TokenResp {
    #[serde(default)]
    status: String,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expired_in: Option<i64>,
    #[serde(default)]
    notification_message: Option<String>,
    #[serde(default)]
    base_resp: Option<BaseResp>,
}

#[derive(Deserialize)]
struct BaseResp {
    #[serde(default)]
    status_msg: Option<String>,
}

/// Run the blocking OAuth user-code flow. Prints the verification URL +
/// user code to stdout, then polls until the server approves or expiry
/// fires. Safe to call from an async context — it owns a short-lived
/// tokio runtime internally so callers can stay synchronous.
pub fn run_flow(region: Region) -> Result<OAuthToken> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run_flow_async(region))
}

async fn run_flow_async(region: Region) -> Result<OAuthToken> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let pkce = gen_pkce();

    let code_resp: CodeResp = client
        .post(format!("{}/oauth/code", region.base_url()))
        .header("Accept", "application/json")
        .header("x-request-id", uuid::Uuid::new_v4().to_string())
        .form(&[
            ("response_type", "code"),
            ("client_id", SHARED_CLIENT_ID),
            ("scope", SCOPE),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", &pkce.state),
        ])
        .send()
        .await
        .context("POST /oauth/code failed")?
        .error_for_status()
        .context("MiniMax /oauth/code returned a non-2xx")?
        .json()
        .await
        .context("parsing /oauth/code JSON")?;

    if code_resp.state != pkce.state {
        bail!("MiniMax OAuth state mismatch (possible CSRF / session reuse)");
    }

    println!();
    println!("┌─────────────────── MiniMax Token Plan OAuth ───────────────────┐");
    println!("│                                                                │");
    println!("│  1. Abre esta URL en el navegador donde tengas MiniMax logged  │");
    println!("│     in:                                                        │");
    println!("│                                                                │");
    println!("│     {:<58} │", code_resp.verification_uri);
    println!("│                                                                │");
    println!("│  2. Cuando te pida código, ingresa:                            │");
    println!("│                                                                │");
    println!("│     {:<58} │", code_resp.user_code);
    println!("│                                                                │");
    println!("│  3. Aprueba el acceso. El wizard espera aquí hasta que lo      │");
    println!("│     confirmes (o hasta que el código expire).                  │");
    println!("│                                                                │");
    println!("└────────────────────────────────────────────────────────────────┘");
    println!();
    println!("Esperando aprobación…");

    let expire_at = code_resp.expired_in;
    let interval = Duration::from_millis(code_resp.interval.unwrap_or(2000).max(2000));
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // `expired_in` from MiniMax is either a unix-epoch deadline or a
    // TTL in seconds; treat anything below `start` as TTL for safety.
    let deadline = if expire_at > start {
        expire_at
    } else {
        start + expire_at.max(60)
    };

    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if now >= deadline {
            bail!("MiniMax OAuth user-code expired before approval");
        }

        let resp = client
            .post(format!("{}/oauth/token", region.base_url()))
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", GRANT_TYPE),
                ("client_id", SHARED_CLIENT_ID),
                ("user_code", code_resp.user_code.as_str()),
                ("code_verifier", pkce.verifier.as_str()),
            ])
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "poll /oauth/token network error, retrying");
                tokio::time::sleep(interval).await;
                continue;
            }
        };
        let status_code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let parsed: Option<TokenResp> = serde_json::from_str(&text).ok();
        let Some(tr) = parsed else {
            if !status_code.is_success() {
                bail!("MiniMax /oauth/token HTTP {status_code}: {text}");
            }
            tokio::time::sleep(interval).await;
            continue;
        };

        match tr.status.as_str() {
            "success" => {
                let access = tr
                    .access_token
                    .ok_or_else(|| anyhow::anyhow!("access_token missing in success response"))?;
                let refresh = tr
                    .refresh_token
                    .ok_or_else(|| anyhow::anyhow!("refresh_token missing"))?;
                let ttl = tr.expired_in.unwrap_or(3600).max(60);
                let expires_at = now + ttl;
                return Ok(OAuthToken {
                    access_token: access,
                    refresh_token: refresh,
                    expires_at,
                    notification_message: tr.notification_message,
                });
            }
            "error" => {
                let msg = tr
                    .base_resp
                    .and_then(|b| b.status_msg)
                    .unwrap_or_else(|| "MiniMax OAuth reported error".into());
                bail!("MiniMax OAuth error: {msg}");
            }
            _ => {
                // pending — keep polling
                tokio::time::sleep(interval).await;
            }
        }
    }
}
