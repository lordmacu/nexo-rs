//! OAuth 2.0 user flow: exchange `refresh_token` for `access_token` on demand,
//! cache the access token with a safety margin, and expose it to callers.
//!
//! Why user flow (not service account): personal Gmail accounts can't use
//! service accounts without a Workspace domain + delegation. User flow works
//! for both personal Gmail and Workspace accounts.

use std::time::{Duration, Instant};

use parking_lot::Mutex;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::client::{bad_auth, http_client, transport, GoogleError};

const TOKEN_ENDPOINT_DEFAULT: &str = "https://oauth2.googleapis.com/token";
/// Refresh access tokens this many seconds before they technically expire so
/// we don't ever hand out a token that becomes invalid mid-request.
const EXPIRY_MARGIN_SECS: u64 = 60;

pub fn token_endpoint() -> String {
    std::env::var("GOOGLE_OAUTH_TOKEN_URL").unwrap_or_else(|_| TOKEN_ENDPOINT_DEFAULT.into())
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

#[derive(Clone)]
struct Cached {
    token: String,
    fetched_at: Instant,
    ttl: Duration,
}

static CACHE: once_cell::sync::Lazy<Mutex<Option<Cached>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

/// Return a valid access token. Hits the cache when fresh; otherwise asks
/// Google for a new one using the configured refresh token.
pub fn access_token(client: &Client) -> Result<String, GoogleError> {
    {
        let guard = CACHE.lock();
        if let Some(c) = guard.as_ref() {
            if c.fetched_at.elapsed() + Duration::from_secs(EXPIRY_MARGIN_SECS) < c.ttl {
                return Ok(c.token.clone());
            }
        }
    }

    let client_id = env_required("GOOGLE_CLIENT_ID")?;
    let client_secret = env_required("GOOGLE_CLIENT_SECRET")?;
    let refresh_token = env_required("GOOGLE_REFRESH_TOKEN")?;

    let params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];

    let url = token_endpoint();
    let resp = client
        .post(&url)
        .form(&params)
        .send()
        .map_err(|e| transport(e.to_string()))?;

    let status = resp.status().as_u16();
    if status == 400 || status == 401 {
        let body = resp.text().unwrap_or_default();
        return Err(bad_auth(format!("refresh failed ({status}): {}", truncate(&body, 300))));
    }
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(GoogleError::Status {
            code: status,
            body: truncate(&body, 300),
        });
    }

    let parsed: TokenResponse = resp
        .json()
        .map_err(|e| GoogleError::InvalidJson(e.to_string()))?;

    let ttl = Duration::from_secs(parsed.expires_in);
    let mut guard = CACHE.lock();
    *guard = Some(Cached {
        token: parsed.access_token.clone(),
        fetched_at: Instant::now(),
        ttl,
    });
    // Silent refresh — no tracing dep. Operator can watch oauth failures via
    // the tool's -32011 Unauthorized error path.
    let _ = (&parsed.scope, &parsed.token_type);
    Ok(parsed.access_token)
}

pub fn has_credentials() -> bool {
    std::env::var("GOOGLE_CLIENT_ID").ok().map(|s| !s.trim().is_empty()).unwrap_or(false)
        && std::env::var("GOOGLE_CLIENT_SECRET").ok().map(|s| !s.trim().is_empty()).unwrap_or(false)
        && std::env::var("GOOGLE_REFRESH_TOKEN").ok().map(|s| !s.trim().is_empty()).unwrap_or(false)
}

#[doc(hidden)]
pub fn reset_cache() {
    *CACHE.lock() = None;
}

fn env_required(key: &str) -> Result<String, GoogleError> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad_auth(format!("env var {key} is not set")))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

// Re-export http_client for callers that need it to drive `access_token`.
pub fn client() -> &'static Client {
    http_client()
}
