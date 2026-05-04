//! Anthropic Claude.ai authorization-code OAuth flow (PKCE).
//!
//! Pure functions — no terminal I/O, no browser opening, no file
//! persistence. The CLI wizard wraps these with stdin glue; the
//! admin RPC handler invokes them across two HTTP requests
//! (`oauth_start` + `oauth_finish`).

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::bundle::OAuthBundle;
use crate::pkce::{pct_encode, Pkce};

/// Public Claude Code CLI client_id. Not a secret — identifies the
/// app to Anthropic for user consent.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Anthropic's hosted callback page — displays `<code>#<state>` to
/// the operator for copy-paste.
pub const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";

/// User-facing authorize endpoint. The operator opens this URL in a
/// browser logged into claude.ai.
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";

/// Server-to-server token exchange endpoint. Called by [`exchange_code`].
pub const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// OAuth scopes requested by the CLI client.
pub const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Source tag stamped on bundles minted via this flow.
pub const SOURCE_TAG: &str = "oauth_auth_code";

/// HTTP timeout for the token exchange. 30 s tolerates slow links;
/// the exchange itself is one round-trip and usually < 1 s.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Build the authorize URL the operator opens in a browser.
///
/// The `Pkce` must have been generated with [`crate::pkce::StateEncoding::HexOnly`]
/// — Anthropic's authorize endpoint rejects `-_=` in `state`.
pub fn build_authorize_url(pkce: &Pkce) -> String {
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

/// Exchange a freshly-pasted authorization `code` (and the `state`
/// the user pasted with it) for an [`OAuthBundle`].
///
/// Verifies `state` matches the stashed `pkce.state` to prevent
/// CSRF / session-reuse before sending the code upstream.
///
/// Set `token_url` to [`TOKEN_URL`] in production; the parameter
/// exists so tests can point at a stub server.
pub async fn exchange_code(
    pkce: &Pkce,
    code: &str,
    state: &str,
    token_url: &str,
) -> Result<OAuthBundle> {
    if state != pkce.state {
        anyhow::bail!(
            "state mismatch (esperado `{}`, recibido `{}`) — posible CSRF o sesión reutilizada",
            pkce.state,
            state
        );
    }
    let client = reqwest::Client::builder()
        .timeout(EXCHANGE_TIMEOUT)
        .build()?;
    let resp = client
        .post(token_url)
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
        anyhow::bail!("Anthropic /v1/oauth/token HTTP {status}: {text}");
    }
    let parsed: TokenResp =
        serde_json::from_str(&text).with_context(|| format!("parse token response: {text}"))?;

    let now = chrono::Utc::now().timestamp();
    let ttl = parsed.expires_in.unwrap_or(3600).max(60);
    let access = parsed
        .access_token
        .ok_or_else(|| anyhow::anyhow!("response missing access_token"))?;
    let refresh = parsed
        .refresh_token
        .ok_or_else(|| anyhow::anyhow!("response missing refresh_token"))?;
    let bundle = OAuthBundle::new(
        access,
        refresh,
        now + ttl,
        parsed.account.and_then(|a| a.email_address),
        SOURCE_TAG,
        "anthropic",
    );
    bundle
        .validate()
        .map_err(|e| anyhow::anyhow!("bundle validation: {e}"))?;
    Ok(bundle)
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
    use crate::pkce::{gen_pkce, StateEncoding};

    #[test]
    fn authorize_url_carries_all_required_params() {
        let p = Pkce {
            verifier: "v".into(),
            challenge: "c".into(),
            state: "abcd1234".into(),
        };
        let url = build_authorize_url(&p);
        for needle in [
            "client_id=9d1c250a",
            "response_type=code",
            "redirect_uri=https",
            "code_challenge=c",
            "code_challenge_method=S256",
            "state=abcd1234",
            "scope=org",
        ] {
            assert!(url.contains(needle), "missing `{needle}` in: {url}");
        }
    }

    #[test]
    fn authorize_url_uses_hex_state() {
        let p = gen_pkce(StateEncoding::HexOnly);
        let url = build_authorize_url(&p);
        // The state appears verbatim — confirm we haven't injected
        // characters Anthropic rejects.
        assert!(url.contains(&format!("state={}", p.state)));
        assert!(p.state.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn exchange_code_rejects_state_mismatch() {
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "c".into(),
            state: "expected".into(),
        };
        let result = exchange_code(&pkce, "code", "different-state", TOKEN_URL).await;
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("state mismatch"),
            "expected CSRF rejection, got: {err}"
        );
    }
}
