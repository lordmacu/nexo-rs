use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::Method;
use serde_json::Value;

use ext_common::{Breaker, BreakerError};

pub const CLIENT_VERSION: &str = "google-0.1.0";
const USER_AGENT: &str = "agent-rs-google/0.1 (+https://github.com/lordmacu/agent-rs)";
const RETRY_BACKOFFS_MS: [u64; 2] = [500, 1000];
const CB_THRESHOLD: u32 = 5;
const CB_OPEN_SECS: u64 = 30;

fn timeout_secs() -> u64 {
    std::env::var("GOOGLE_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15)
}

pub fn user_agent() -> &'static str {
    USER_AGENT
}

pub fn http_client() -> &'static Client {
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
    crate::oauth::reset_cache();
}

pub fn gmail_base_url() -> String {
    std::env::var("GOOGLE_GMAIL_URL")
        .unwrap_or_else(|_| "https://gmail.googleapis.com/gmail/v1".to_string())
}

pub fn calendar_base_url() -> String {
    std::env::var("GOOGLE_CALENDAR_URL")
        .unwrap_or_else(|_| "https://www.googleapis.com/calendar/v3".to_string())
}

pub fn tasks_base_url() -> String {
    std::env::var("GOOGLE_TASKS_URL")
        .unwrap_or_else(|_| "https://tasks.googleapis.com/tasks/v1".to_string())
}

pub fn drive_base_url() -> String {
    std::env::var("GOOGLE_DRIVE_URL")
        .unwrap_or_else(|_| "https://www.googleapis.com/drive/v3".to_string())
}

pub fn drive_upload_url() -> String {
    std::env::var("GOOGLE_DRIVE_UPLOAD_URL")
        .unwrap_or_else(|_| "https://www.googleapis.com/upload/drive/v3".to_string())
}

pub fn people_base_url() -> String {
    std::env::var("GOOGLE_PEOPLE_URL")
        .unwrap_or_else(|_| "https://people.googleapis.com/v1".to_string())
}

pub fn photos_base_url() -> String {
    std::env::var("GOOGLE_PHOTOS_URL")
        .unwrap_or_else(|_| "https://photoslibrary.googleapis.com/v1".to_string())
}

pub fn drive_sandbox_root() -> Result<std::path::PathBuf, GoogleError> {
    let root = match std::env::var("GOOGLE_DRIVE_SANDBOX_ROOT") {
        Ok(r) if !r.trim().is_empty() => std::path::PathBuf::from(r),
        _ => std::env::temp_dir(),
    };
    std::fs::create_dir_all(&root).map_err(|e| GoogleError::Transport(e.to_string()))?;
    root.canonicalize().map_err(|e| GoogleError::Transport(e.to_string()))
}

pub fn sandbox_drive_path(requested: &std::path::Path) -> Result<std::path::PathBuf, GoogleError> {
    let parent = requested.parent().ok_or_else(|| bad_input("path has no parent"))?;
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| GoogleError::Transport(e.to_string()))?;
    }
    let parent_abs = parent
        .canonicalize()
        .map_err(|e| GoogleError::Transport(e.to_string()))?;
    let root = drive_sandbox_root()?;
    if !parent_abs.starts_with(&root) {
        return Err(bad_input(format!(
            "path `{}` is outside sandbox `{}`",
            requested.display(),
            root.display()
        )));
    }
    let filename = requested
        .file_name()
        .ok_or_else(|| bad_input("path missing filename"))?;
    Ok(parent_abs.join(filename))
}

#[derive(Debug)]
pub enum GoogleError {
    MissingCredentials,
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    RateLimited { retry_after_secs: Option<u64> },
    WriteDenied(&'static str),
    BadInput(String),
    Status { code: u16, body: String },
    Transport(String),
    InvalidJson(String),
    Timeout,
    CircuitOpen,
}

impl GoogleError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            GoogleError::MissingCredentials => -32041,
            GoogleError::Unauthorized(_) => -32011,
            GoogleError::Forbidden(_) => -32012,
            GoogleError::NotFound(_) => -32001,
            GoogleError::RateLimited { .. } => -32013,
            GoogleError::WriteDenied(_) => -32043,
            GoogleError::BadInput(_) => -32602,
            GoogleError::Status { code, .. } if (400..500).contains(&(*code as u32)) => -32002,
            GoogleError::Status { .. } => -32003,
            GoogleError::Transport(_) => -32003,
            GoogleError::InvalidJson(_) => -32006,
            GoogleError::Timeout => -32005,
            GoogleError::CircuitOpen => -32004,
        }
    }

    pub fn message(&self) -> String {
        match self {
            GoogleError::MissingCredentials => {
                "missing google oauth credentials (GOOGLE_CLIENT_ID/CLIENT_SECRET/REFRESH_TOKEN)".into()
            }
            GoogleError::Unauthorized(m) => format!("unauthorized: {m}"),
            GoogleError::Forbidden(m) => format!("forbidden: {m}"),
            GoogleError::NotFound(m) => format!("not found: {m}"),
            GoogleError::RateLimited { retry_after_secs } => match retry_after_secs {
                Some(s) => format!("rate limited; retry after {s}s"),
                None => "rate limited".into(),
            },
            GoogleError::WriteDenied(flag) => format!(
                "write denied: set {flag}=true on the agent process to allow this action"
            ),
            GoogleError::BadInput(m) => m.clone(),
            GoogleError::Status { code, body } if (400..500).contains(&(*code as u32)) => {
                format!("HTTP {code}: {}", truncate(body, 300))
            }
            GoogleError::Status { code, .. } => format!("HTTP {code} (server error)"),
            GoogleError::Transport(e) => format!("transport: {e}"),
            GoogleError::InvalidJson(e) => format!("google returned invalid json: {e}"),
            GoogleError::Timeout => "request timed out".into(),
            GoogleError::CircuitOpen => "circuit open, too many upstream failures".into(),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

pub fn bad_auth(msg: impl Into<String>) -> GoogleError {
    GoogleError::Unauthorized(msg.into())
}

pub fn transport(msg: impl Into<String>) -> GoogleError {
    GoogleError::Transport(msg.into())
}

pub fn bad_input(msg: impl Into<String>) -> GoogleError {
    GoogleError::BadInput(msg.into())
}

pub struct AuthorizedRequest<'a> {
    pub method: Method,
    pub url: &'a str,
    pub query: &'a [(&'a str, String)],
    pub body: Option<Value>,
}

/// Low-level raw request + bytes result. Used by drive_download; also
/// returns the response content-type so callers can decide how to handle
/// the payload.
pub fn call_bytes(
    method: Method,
    url: &str,
    query: &[(&str, String)],
) -> Result<(Vec<u8>, Option<String>), GoogleError> {
    let client = http_client();
    let token = crate::oauth::access_token(client)?;
    let mut rb = client.request(method, url).bearer_auth(&token);
    if !query.is_empty() {
        rb = rb.query(query);
    }
    let resp = rb.send().map_err(|e| GoogleError::Transport(e.to_string()))?;
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(match status {
            401 => GoogleError::Unauthorized(body),
            403 => GoogleError::Forbidden(body),
            404 => GoogleError::NotFound(body),
            429 => GoogleError::RateLimited { retry_after_secs: None },
            _ => GoogleError::Status { code: status, body },
        });
    }
    let bytes = resp
        .bytes()
        .map_err(|e| GoogleError::Transport(e.to_string()))?
        .to_vec();
    Ok((bytes, ct))
}

/// Low-level multipart upload to /upload/drive/v3/files?uploadType=multipart.
pub fn multipart_upload(
    url: &str,
    metadata: &Value,
    content_bytes: Vec<u8>,
    content_type: &str,
) -> Result<Option<Value>, GoogleError> {
    let client = http_client();
    let token = crate::oauth::access_token(client)?;
    // Google accepts a simple multipart/related body with two parts:
    // 1. JSON metadata
    // 2. raw file bytes
    let boundary = format!("agent-rs-drive-{}", std::process::id());
    let meta_str = serde_json::to_string(metadata)
        .map_err(|e| GoogleError::InvalidJson(e.to_string()))?;
    let mut body: Vec<u8> = Vec::with_capacity(content_bytes.len() + 256);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
    body.extend_from_slice(meta_str.as_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(&content_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let resp = client
        .post(url)
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/related; boundary={boundary}"),
        )
        .body(body)
        .send()
        .map_err(|e| GoogleError::Transport(e.to_string()))?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(match status {
            401 => GoogleError::Unauthorized(body),
            403 => GoogleError::Forbidden(body),
            404 => GoogleError::NotFound(body),
            429 => GoogleError::RateLimited { retry_after_secs: None },
            _ => GoogleError::Status { code: status, body },
        });
    }
    let text = resp.text().unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(None);
    }
    let parsed: Value =
        serde_json::from_str(&text).map_err(|e| GoogleError::InvalidJson(e.to_string()))?;
    Ok(Some(parsed))
}

pub fn call(req: AuthorizedRequest<'_>) -> Result<Option<Value>, GoogleError> {
    let client = http_client();
    let token = crate::oauth::access_token(client)?;

    let method_clone = req.method.clone();
    let url_s = req.url.to_string();
    let query_clone: Vec<(String, String)> = req
        .query
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    let body_clone = req.body.clone();
    let token_clone = token.clone();

    let resp = call_with_retries(|| {
        let mut rb: RequestBuilder = http_client().request(method_clone.clone(), &url_s);
        rb = rb.bearer_auth(&token_clone);
        if !query_clone.is_empty() {
            rb = rb.query(&query_clone);
        }
        if let Some(b) = &body_clone {
            rb = rb.json(b);
        }
        rb.send()
    })?;

    if resp.status().as_u16() == 204 {
        return Ok(None);
    }
    let text = resp.text().unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|e| GoogleError::InvalidJson(e.to_string()))?;
    Ok(Some(parsed))
}

fn call_with_retries<F>(mut send: F) -> Result<reqwest::blocking::Response, GoogleError>
where
    F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, GoogleError> {
        match once() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if resp.status().is_success() || status == 204 {
                    return Ok(resp);
                }
                match status {
                    401 => Err(GoogleError::Unauthorized(
                        resp.text().unwrap_or_default(),
                    )),
                    403 => Err(GoogleError::Forbidden(resp.text().unwrap_or_default())),
                    404 => Err(GoogleError::NotFound(resp.text().unwrap_or_default())),
                    429 => {
                        let retry_after = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok());
                        Err(GoogleError::RateLimited {
                            retry_after_secs: retry_after,
                        })
                    }
                    _ => Err(GoogleError::Status {
                        code: status,
                        body: resp.text().unwrap_or_default(),
                    }),
                }
            }
            Err(e) if e.is_timeout() => Err(GoogleError::Timeout),
            Err(e) => Err(GoogleError::Transport(e.to_string())),
        }
    };

    let mut last_err: Option<GoogleError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        let outcome = BREAKER.call(|| attempt(&mut send));
        match outcome {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(GoogleError::CircuitOpen),
            Err(BreakerError::Upstream(e)) => {
                let transient = match &e {
                    GoogleError::Timeout | GoogleError::Transport(_) => true,
                    GoogleError::Status { code, .. } => *code >= 500,
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
    Err(last_err.unwrap_or(GoogleError::Transport("unknown".into())))
}
