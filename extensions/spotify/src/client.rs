use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use reqwest::Method;
use serde_json::Value;

use ext_common::{Breaker, BreakerError};

pub const CLIENT_VERSION: &str = "spotify-0.1.0";
const USER_AGENT: &str = "agent-rs-spotify/0.1 (+https://github.com/lordmacu/agent-rs)";
const RETRY_BACKOFFS_MS: [u64; 2] = [500, 1000];
const CB_THRESHOLD: u32 = 5;
const CB_OPEN_SECS: u64 = 30;

fn timeout_secs() -> u64 {
    std::env::var("SPOTIFY_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15)
}

pub fn base_url() -> String {
    std::env::var("SPOTIFY_API_URL").unwrap_or_else(|_| "https://api.spotify.com/v1".to_string())
}

pub fn user_agent() -> &'static str {
    USER_AGENT
}

pub fn token_present() -> bool {
    std::env::var("SPOTIFY_ACCESS_TOKEN")
        .ok()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false)
}

fn token() -> Result<String, SpotifyError> {
    let raw = std::env::var("SPOTIFY_ACCESS_TOKEN")
        .map_err(|_| SpotifyError::MissingToken)?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(SpotifyError::MissingToken);
    }
    Ok(trimmed)
}

fn http_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(timeout_secs()))
            .build()
            .expect("reqwest client build")
    })
}

static BREAKER: Lazy<Breaker> =
    Lazy::new(|| Breaker::new(CB_THRESHOLD, Duration::from_secs(CB_OPEN_SECS)));

#[doc(hidden)]
pub fn reset_state() {
    (&*BREAKER).reset();
}

#[derive(Debug)]
pub enum SpotifyError {
    MissingToken,
    Unauthorized,
    Forbidden(String),
    NotFound(String),
    NoActiveDevice,
    RateLimited { retry_after_secs: Option<u64> },
    Timeout,
    Status(u16, String),
    Transport(String),
    InvalidJson(String),
    CircuitOpen,
    BadInput(String),
}

impl SpotifyError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            SpotifyError::BadInput(_) => -32602,
            SpotifyError::MissingToken => -32041,
            SpotifyError::Unauthorized => -32011,
            SpotifyError::Forbidden(_) => -32012,
            SpotifyError::NotFound(_) => -32001,
            SpotifyError::NoActiveDevice => -32070,
            SpotifyError::RateLimited { .. } => -32013,
            SpotifyError::Status(s, _) if (400..500).contains(&(*s as u32)) => -32002,
            SpotifyError::Status(_, _) => -32003,
            SpotifyError::CircuitOpen => -32004,
            SpotifyError::Timeout => -32005,
            SpotifyError::InvalidJson(_) => -32006,
            SpotifyError::Transport(_) => -32003,
        }
    }

    pub fn message(&self) -> String {
        match self {
            SpotifyError::BadInput(m) => m.clone(),
            SpotifyError::MissingToken => "SPOTIFY_ACCESS_TOKEN not set".into(),
            SpotifyError::Unauthorized => {
                "unauthorized: access token invalid or expired (refresh via OAuth)".into()
            }
            SpotifyError::Forbidden(m) => format!("forbidden: {m}"),
            SpotifyError::NotFound(m) => format!("not found: {m}"),
            SpotifyError::NoActiveDevice => {
                "no active Spotify device; open Spotify on a device first (requires a Spotify Connect target)".into()
            }
            SpotifyError::RateLimited { retry_after_secs } => match retry_after_secs {
                Some(s) => format!("rate limited; retry after {s}s"),
                None => "rate limited".into(),
            },
            SpotifyError::Status(s, body) if (400..500).contains(&(*s as u32)) => {
                format!("HTTP {s}: {}", truncate(body, 300))
            }
            SpotifyError::Status(s, _) => format!("HTTP {s} (server error)"),
            SpotifyError::CircuitOpen => "circuit open, too many upstream failures".into(),
            SpotifyError::Timeout => "request timed out".into(),
            SpotifyError::InvalidJson(e) => format!("spotify returned invalid json: {e}"),
            SpotifyError::Transport(e) => format!("transport: {e}"),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

pub struct Request<'a> {
    pub method: Method,
    pub path: &'a str,
    pub query: &'a [(&'a str, String)],
    pub body: Option<Value>,
}

pub fn call(req: Request<'_>) -> Result<Option<Value>, SpotifyError> {
    let t = token()?;
    let url = format!("{}{}", base_url(), req.path);
    let method_clone = req.method.clone();
    let query_clone: Vec<(String, String)> = req
        .query
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    let body_clone = req.body.clone();

    let resp = call_with_retries(|| {
        let mut rb = http_client().request(method_clone.clone(), &url);
        rb = rb.header("Authorization", format!("Bearer {t}"));
        if !query_clone.is_empty() {
            rb = rb.query(&query_clone);
        }
        if let Some(b) = &body_clone {
            rb = rb.json(b);
        }
        rb.send()
    })?;
    let status = resp.status().as_u16();
    if status == 204 {
        return Ok(None);
    }
    let text = resp.text().unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|e| SpotifyError::InvalidJson(e.to_string()))?;
    Ok(Some(parsed))
}

fn call_with_retries<F>(
    mut send: F,
) -> Result<reqwest::blocking::Response, SpotifyError>
where
    F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, SpotifyError> {
        match once() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if resp.status().is_success() || status == 204 {
                    return Ok(resp);
                }
                match status {
                    401 => Err(SpotifyError::Unauthorized),
                    403 => {
                        let body = resp.text().unwrap_or_default();
                        if body.contains("NO_ACTIVE_DEVICE") {
                            return Err(SpotifyError::NoActiveDevice);
                        }
                        Err(SpotifyError::Forbidden(body))
                    }
                    404 => {
                        let body = resp.text().unwrap_or_default();
                        if body.contains("NO_ACTIVE_DEVICE") {
                            return Err(SpotifyError::NoActiveDevice);
                        }
                        Err(SpotifyError::NotFound(body))
                    }
                    429 => {
                        let retry_after = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok());
                        Err(SpotifyError::RateLimited {
                            retry_after_secs: retry_after,
                        })
                    }
                    _ => {
                        let body = resp.text().unwrap_or_default();
                        // Spotify's "no active device" error is 404 with a
                        // specific message — but some SDKs return 400 with
                        // reason NO_ACTIVE_DEVICE. Detect either.
                        if body.contains("NO_ACTIVE_DEVICE") {
                            return Err(SpotifyError::NoActiveDevice);
                        }
                        Err(SpotifyError::Status(status, body))
                    }
                }
            }
            Err(e) if e.is_timeout() => Err(SpotifyError::Timeout),
            Err(e) => Err(SpotifyError::Transport(e.to_string())),
        }
    };

    let mut last_err: Option<SpotifyError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        let outcome = BREAKER.call(|| attempt(&mut send));
        match outcome {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(SpotifyError::CircuitOpen),
            Err(BreakerError::Upstream(e)) => {
                let transient = match &e {
                    SpotifyError::Timeout | SpotifyError::Transport(_) => true,
                    SpotifyError::Status(s, _) => *s >= 500,
                    _ => false,
                };
                last_err = Some(e);
                if !transient || i == RETRY_BACKOFFS_MS.len() {
                    break;
                }
                sleep(Duration::from_millis(RETRY_BACKOFFS_MS[i]));
            }
        }
    }
    Err(last_err.unwrap_or(SpotifyError::Transport("unknown".into())))
}
