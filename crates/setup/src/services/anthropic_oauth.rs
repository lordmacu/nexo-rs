//! CLI wrapper around `nexo_llm_auth::anthropic` — OAuth PKCE flow
//! for Claude.ai subscriptions.
//!
//! Phase 82.10.u.1 extracted the cryptographic + HTTP-exchange
//! primitives into `nexo-llm-auth` so the admin RPC handler
//! (`oauth_start` / `oauth_finish`) can reuse them. This file keeps
//! the interactive glue: print URL banner, best-effort browser
//! open, read pasted `<code>#<state>` from stdin, hand off to the
//! pure async exchange.

use std::io::{self, BufRead, Write};

use anyhow::{Context, Result};
use nexo_llm_auth::anthropic::{build_authorize_url, exchange_code, TOKEN_URL};
use nexo_llm_auth::pkce::{gen_pkce, parse_code_payload, StateEncoding};
use nexo_llm_auth::OAuthBundle;

/// Public CLI client_id reused so callers that want to reference the
/// constant from setup don't have to reach into the inner crate.
pub use nexo_llm_auth::anthropic::CLIENT_ID;
/// Public callback URL reused for the same reason.
pub use nexo_llm_auth::anthropic::REDIRECT_URI;

/// Result of a successful CLI flow — flat shape preserved so the
/// `writer.rs` persist branch keeps its old API.
pub struct OAuthToken {
    /// OAuth access token sent in `Authorization: Bearer ...`.
    pub access_token: String,
    /// Long-lived refresh token used by the LLM client to mint new
    /// access tokens automatically.
    pub refresh_token: String,
    /// Unix-seconds expiry of `access_token`.
    pub expires_at: i64,
    /// Operator-facing email (when the provider returns it).
    pub account_email: Option<String>,
}

impl From<OAuthBundle> for OAuthToken {
    fn from(b: OAuthBundle) -> Self {
        Self {
            access_token: b.access_token,
            refresh_token: b.refresh_token,
            expires_at: b.expires_at,
            account_email: b.account_email,
        }
    }
}

/// Blocking entry-point. The caller may already be inside a tokio
/// runtime (the `agent` binary is `#[tokio::main]`), so we cannot
/// `block_on` on the same thread. Offload to a dedicated std thread
/// with its own current-thread runtime.
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
    // Anthropic insists on hex-only state — see pkce::StateEncoding docs.
    let pkce = gen_pkce(StateEncoding::HexOnly);
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

    let (code, state) = parse_code_payload(&raw).map_err(anyhow::Error::from)?;
    let bundle = exchange_code(&pkce, &code, &state, TOKEN_URL).await?;
    Ok(bundle.into())
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
