//! MiniMax Token-Plan device-code OAuth flow.
//!
//! Mirrors `research/extensions/minimax/oauth.ts`. Two-step flow:
//!
//! 1. [`request_user_code`] — asks the server for a `user_code` +
//!    `verification_uri`. The operator opens that URL and types the
//!    code into MiniMax's portal.
//! 2. [`poll_token`] — polls `/oauth/token` until the user approves
//!    (status `success`), the server reports an error, or the
//!    user-code expires.
//!
//! Both functions are pure async — they don't print, sleep beyond
//! the polling interval, or touch disk. The CLI wizard wraps them
//! with stdout messages; the admin RPC handler invokes them across
//! `oauth_start` (returns `user_code` + `verification_uri` to the
//! SPA) and `oauth_finish` (server-side polls until the user
//! confirms).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::bundle::OAuthBundle;
use crate::pkce::Pkce;

/// Public client id MiniMax shares with OpenClaw and the wizard.
/// Not a secret.
pub const SHARED_CLIENT_ID: &str = "78257093-7e40-4613-99e0-527b14b39113";
/// OAuth scope set the wizard requests (group membership +
/// completions).
pub const SCOPE: &str = "group_id profile model.completion";
/// IETF user-code grant URI.
pub const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:user_code";
/// Source tag stamped on bundles minted via this flow.
pub const SOURCE_TAG: &str = "oauth_device_code";

/// Region selector — drives the base URL the OAuth requests hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// `api.minimax.io` — non-China endpoint.
    Global,
    /// `api.minimaxi.com` — China-mainland endpoint.
    Cn,
}

impl Region {
    /// Base URL (no trailing slash).
    pub fn base_url(self) -> &'static str {
        match self {
            Self::Global => "https://api.minimax.io",
            Self::Cn => "https://api.minimaxi.com",
        }
    }
}

/// Response from [`request_user_code`] surfaced to the operator
/// (via TTY or SPA).
#[derive(Debug, Clone)]
pub struct DeviceCodeResponse {
    /// Code the operator types into the portal.
    pub user_code: String,
    /// URL the operator opens in their browser.
    pub verification_uri: String,
    /// Unix-seconds deadline for `poll_token`. Defensive: the
    /// upstream `expired_in` field is ambiguous (TTL vs absolute
    /// timestamp), normalised here to an absolute deadline.
    pub deadline_unix: i64,
    /// Server-recommended polling interval, ≥ 2 s.
    pub interval: Duration,
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

/// Request a fresh device-code from MiniMax's `/oauth/code`
/// endpoint. The returned `state` is checked against `pkce.state`
/// for CSRF protection.
///
/// `base_url_override` lets tests point at a stub server. Pass
/// `None` to use [`Region::base_url`].
pub async fn request_user_code(
    region: Region,
    pkce: &Pkce,
    base_url_override: Option<&str>,
) -> Result<DeviceCodeResponse> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let base = base_url_override.unwrap_or(region.base_url());
    let code_resp: CodeResp = client
        .post(format!("{base}/oauth/code"))
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

    // `expired_in` is either a unix-epoch deadline or a TTL in
    // seconds — defensive: anything below `now` is treated as TTL.
    let now = unix_now();
    let deadline = if code_resp.expired_in > now {
        code_resp.expired_in
    } else {
        now + code_resp.expired_in.max(60)
    };
    let interval = Duration::from_millis(code_resp.interval.unwrap_or(2000).max(2000));

    Ok(DeviceCodeResponse {
        user_code: code_resp.user_code,
        verification_uri: code_resp.verification_uri,
        deadline_unix: deadline,
        interval,
    })
}

/// Poll `/oauth/token` until the user approves, the server reports
/// an error, or the user-code expires.
///
/// Returns a validated [`OAuthBundle`] on success.
///
/// `pkce.verifier` is the verifier from the same PKCE bundle used
/// by [`request_user_code`]. `device.user_code` was returned by
/// that call. The function honours `device.interval` between
/// polls and bails at `device.deadline_unix`.
///
/// Transient network errors retry silently; HTTP non-2xx with no
/// JSON body is fatal.
pub async fn poll_token(
    region: Region,
    pkce: &Pkce,
    device: &DeviceCodeResponse,
    base_url_override: Option<&str>,
) -> Result<OAuthBundle> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let base = base_url_override.unwrap_or(region.base_url());
    let url = format!("{base}/oauth/token");

    loop {
        let now = unix_now();
        if now >= device.deadline_unix {
            bail!("MiniMax OAuth user-code expired before approval");
        }

        let resp = client
            .post(&url)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", GRANT_TYPE),
                ("client_id", SHARED_CLIENT_ID),
                ("user_code", device.user_code.as_str()),
                ("code_verifier", pkce.verifier.as_str()),
            ])
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "MiniMax /oauth/token network error, retrying");
                tokio::time::sleep(device.interval).await;
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
            tokio::time::sleep(device.interval).await;
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
                let _ = tr.notification_message; // surfaced by CLI wrapper if needed
                let bundle =
                    OAuthBundle::new(access, refresh, expires_at, None, SOURCE_TAG, "minimax");
                bundle
                    .validate()
                    .map_err(|e| anyhow::anyhow!("bundle validation: {e}"))?;
                return Ok(bundle);
            }
            "error" => {
                let msg = tr
                    .base_resp
                    .and_then(|b| b.status_msg)
                    .unwrap_or_else(|| "MiniMax OAuth reported error".into());
                bail!("MiniMax OAuth error: {msg}");
            }
            _ => {
                tokio::time::sleep(device.interval).await;
            }
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_base_urls_match_minimax_endpoints() {
        assert_eq!(Region::Global.base_url(), "https://api.minimax.io");
        assert_eq!(Region::Cn.base_url(), "https://api.minimaxi.com");
    }
}
