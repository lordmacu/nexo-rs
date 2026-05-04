//! CLI wrapper around `nexo_llm_auth::minimax` — Token-Plan
//! device-code OAuth flow.
//!
//! Phase 82.10.u.1 extracted the cryptographic + HTTP primitives
//! into `nexo-llm-auth` so the admin RPC handler (`oauth_start` /
//! `oauth_finish`) can reuse them. This file keeps the interactive
//! glue: print the verification URL + user code banner, then poll
//! `/oauth/token` blocking until the user approves or the code
//! expires.

use anyhow::Result;
use nexo_llm_auth::minimax::{poll_token, request_user_code};
use nexo_llm_auth::pkce::{gen_pkce, StateEncoding};
use nexo_llm_auth::OAuthBundle;

/// Region selector — re-exported so `writer.rs` callers can pick
/// without depending on `nexo-llm-auth` directly.
pub use nexo_llm_auth::minimax::Region;

/// Result surfaced to `writer.rs`. Flat shape preserved for
/// back-compat with the previous CLI entry-point.
pub struct OAuthToken {
    /// MiniMax access token used for portal-proxied API calls.
    pub access_token: String,
    /// Refresh token for offline rotation by the LLM client.
    pub refresh_token: String,
    /// Unix-seconds expiry of `access_token`.
    pub expires_at: i64,
    /// Optional human message MiniMax surfaces post-approval.
    pub notification_message: Option<String>,
}

impl From<OAuthBundle> for OAuthToken {
    fn from(b: OAuthBundle) -> Self {
        Self {
            access_token: b.access_token,
            refresh_token: b.refresh_token,
            expires_at: b.expires_at,
            // Bundle currently doesn't capture notification_message
            // (it lives on the polling response only). The upstream
            // CLI displays it once and discards; preserve None here.
            notification_message: None,
        }
    }
}

/// Run the blocking OAuth user-code flow. Prints the verification
/// URL + user code to stdout, then polls until the server approves
/// or expiry fires. Safe to call from an async context — owns a
/// short-lived tokio runtime internally.
pub fn run_flow(region: Region) -> Result<OAuthToken> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run_flow_async(region))
}

async fn run_flow_async(region: Region) -> Result<OAuthToken> {
    let pkce = gen_pkce(StateEncoding::Base64Url);
    let device = request_user_code(region, &pkce, None).await?;

    println!();
    println!("┌─────────────────── MiniMax Token Plan OAuth ───────────────────┐");
    println!("│                                                                │");
    println!("│  1. Abre esta URL en el navegador donde tengas MiniMax logged  │");
    println!("│     in:                                                        │");
    println!("│                                                                │");
    println!("│     {:<58} │", device.verification_uri);
    println!("│                                                                │");
    println!("│  2. Cuando te pida código, ingresa:                            │");
    println!("│                                                                │");
    println!("│     {:<58} │", device.user_code);
    println!("│                                                                │");
    println!("│  3. Aprueba el acceso. El wizard espera aquí hasta que lo      │");
    println!("│     confirmes (o hasta que el código expire).                  │");
    println!("│                                                                │");
    println!("└────────────────────────────────────────────────────────────────┘");
    println!();
    println!("Esperando aprobación…");

    let bundle = poll_token(region, &pkce, &device, None).await?;
    Ok(bundle.into())
}
