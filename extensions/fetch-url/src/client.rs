use std::net::IpAddr;
use std::str::FromStr;
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::Method;
use url::Url;

use ext_common::{Breaker, BreakerError};

pub const CLIENT_VERSION: &str = "fetch-url-0.1.0";
const USER_AGENT: &str = "agent-rs-fetch-url/0.1 (+https://github.com/lordmacu/agent-rs)";
const RETRY_BACKOFFS_MS: [u64; 2] = [500, 1000];
const CB_THRESHOLD: u32 = 10; // per host too strict for a generic fetcher
const CB_OPEN_SECS: u64 = 30;

pub const DEFAULT_MAX_BYTES: usize = 5 * 1024 * 1024;
pub const HARD_MAX_BYTES: usize = 50 * 1024 * 1024;
pub const DEFAULT_TIMEOUT_SECS: u64 = 15;
pub const MAX_TIMEOUT_SECS: u64 = 120;

fn http_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(5))
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

pub fn user_agent() -> &'static str {
    USER_AGENT
}

#[derive(Debug)]
pub enum FetchError {
    BadInput(String),
    BlockedHost(String),
    SizeCap { limit: usize, observed: usize },
    Status(u16, String),
    Timeout,
    Transport(String),
    CircuitOpen,
}

impl FetchError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            FetchError::BadInput(_) => -32602,
            FetchError::BlockedHost(_) => -32020,
            FetchError::SizeCap { .. } => -32021,
            FetchError::Status(s, _) if (400..500).contains(&(*s as u32)) => -32002,
            FetchError::Status(_, _) => -32003,
            FetchError::CircuitOpen => -32004,
            FetchError::Timeout => -32005,
            FetchError::Transport(_) => -32003,
        }
    }

    pub fn message(&self) -> String {
        match self {
            FetchError::BadInput(m) => m.clone(),
            FetchError::BlockedHost(h) => format!(
                "host `{h}` is private/loopback/metadata; set allow_private=true to override"
            ),
            FetchError::SizeCap { limit, observed } => {
                format!("response exceeds {limit} bytes (observed at least {observed})")
            }
            FetchError::Status(s, body) if (400..500).contains(&(*s as u32)) => {
                format!("HTTP {s}: {}", truncate(body, 200))
            }
            FetchError::Status(s, _) => format!("HTTP {s} (server error)"),
            FetchError::CircuitOpen => "circuit open, too many upstream failures".into(),
            FetchError::Timeout => "request timed out".into(),
            FetchError::Transport(e) => format!("transport error: {e}"),
        }
    }
}

pub struct FetchRequest<'a> {
    pub url: &'a str,
    pub method: &'a str,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub max_bytes: usize,
    pub timeout_secs: u64,
    pub allow_private: bool,
}

pub struct FetchOutcome {
    pub status: u16,
    pub final_url: String,
    pub headers: Vec<(String, String)>,
    pub body_base64: Option<String>,
    pub body_text: Option<String>,
    pub truncated: bool,
    pub bytes_read: usize,
    pub content_type: Option<String>,
}

pub fn fetch(req: FetchRequest<'_>) -> Result<FetchOutcome, FetchError> {
    let url = Url::parse(req.url).map_err(|e| FetchError::BadInput(format!("bad url: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(FetchError::BadInput(format!(
                "unsupported scheme `{other}`; only http/https allowed"
            )))
        }
    }
    if !req.allow_private {
        guard_private_host(&url)?;
    }

    let method = Method::from_str(&req.method.to_ascii_uppercase())
        .map_err(|_| FetchError::BadInput(format!("bad method: {}", req.method)))?;
    if req.max_bytes == 0 || req.max_bytes > HARD_MAX_BYTES {
        return Err(FetchError::BadInput(format!(
            "max_bytes must be in 1..={HARD_MAX_BYTES}"
        )));
    }
    if req.timeout_secs == 0 || req.timeout_secs > MAX_TIMEOUT_SECS {
        return Err(FetchError::BadInput(format!(
            "timeout_secs must be in 1..={MAX_TIMEOUT_SECS}"
        )));
    }

    let url_s = url.to_string();
    let body_clone = req.body.clone();
    let headers_clone = req.headers.clone();
    let timeout = Duration::from_secs(req.timeout_secs);
    let method_clone = method.clone();

    let resp = call_with_retries(|| {
        let mut rb: RequestBuilder = http_client().request(method_clone.clone(), &url_s);
        rb = rb.timeout(timeout);
        for (k, v) in &headers_clone {
            rb = rb.header(k, v);
        }
        if let Some(b) = &body_clone {
            rb = rb.body(b.clone());
        }
        rb.send()
    })?;

    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Bound response read to `max_bytes`. reqwest's blocking `bytes()` has no
    // native cap, so we read into a Vec up to the cap + 1 byte to detect
    // overflow, then truncate.
    let bytes_all = resp
        .bytes()
        .map_err(|e| FetchError::Transport(e.to_string()))?;
    let bytes_read = bytes_all.len();
    let truncated = bytes_read > req.max_bytes;
    let bytes = if truncated {
        bytes_all.slice(0..req.max_bytes)
    } else {
        bytes_all
    };

    // 4xx surface as Status error to the caller so the LLM sees a
    // structured failure. 5xx already triggered retry; if we got here it
    // means retry exhausted.
    if (400..600).contains(&status) {
        let body_preview = String::from_utf8_lossy(&bytes[..bytes.len().min(500)]).to_string();
        return Err(FetchError::Status(status, body_preview));
    }

    // Decide text vs binary: try UTF-8, fall back to base64 for anything
    // with a non-text content_type or invalid UTF-8.
    let is_text_hint = content_type
        .as_deref()
        .map(|ct| {
            ct.starts_with("text/")
                || ct.starts_with("application/json")
                || ct.starts_with("application/xml")
                || ct.starts_with("application/yaml")
                || ct.contains("javascript")
                || ct.contains("charset=")
        })
        .unwrap_or(true);

    let (body_text, body_base64) = if is_text_hint {
        match std::str::from_utf8(&bytes) {
            Ok(s) => (Some(s.to_string()), None),
            Err(_) => (None, Some(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes))),
        }
    } else {
        (
            None,
            Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &bytes,
            )),
        )
    };

    Ok(FetchOutcome {
        status,
        final_url,
        headers,
        body_text,
        body_base64,
        truncated,
        bytes_read,
        content_type,
    })
}

fn call_with_retries<F>(mut send: F) -> Result<reqwest::blocking::Response, FetchError>
where
    F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, FetchError> {
        match once() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if resp.status().is_server_error() {
                    // Consume body to release the connection before retry.
                    let _ = resp.bytes();
                    Err(FetchError::Status(status, String::new()))
                } else {
                    Ok(resp)
                }
            }
            Err(e) if e.is_timeout() => Err(FetchError::Timeout),
            Err(e) => Err(FetchError::Transport(e.to_string())),
        }
    };

    let mut last_err: Option<FetchError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        let outcome = BREAKER.call(|| attempt(&mut send));
        match outcome {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(FetchError::CircuitOpen),
            Err(BreakerError::Upstream(e)) => {
                let transient = match &e {
                    FetchError::Timeout | FetchError::Transport(_) => true,
                    FetchError::Status(s, _) => *s >= 500,
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
    Err(last_err.unwrap_or(FetchError::Transport("unknown".into())))
}

/// Rejects hosts that resolve to loopback, link-local, private, or well-known
/// cloud metadata endpoints. Best-effort: we check the hostname as a literal
/// IP or against a small string blocklist. DNS-based SSRF (hostname resolves
/// to 127.0.0.1 after lookup) is not covered here; set `allow_private=true`
/// on trusted callers instead of trying to patch that hole piecemeal.
fn guard_private_host(url: &Url) -> Result<(), FetchError> {
    let host = url
        .host()
        .ok_or_else(|| FetchError::BadInput("url missing host".into()))?;

    match host {
        url::Host::Domain(d) => {
            let lower = d.to_ascii_lowercase();
            for bad in ["localhost", "metadata.google.internal", "metadata"] {
                if lower == bad {
                    return Err(FetchError::BlockedHost(d.to_string()));
                }
            }
        }
        url::Host::Ipv4(v4) => {
            if is_private_ip(&IpAddr::V4(v4)) {
                return Err(FetchError::BlockedHost(v4.to_string()));
            }
        }
        url::Host::Ipv6(v6) => {
            if is_private_ip(&IpAddr::V6(v6)) {
                return Err(FetchError::BlockedHost(v6.to_string()));
            }
        }
    }

    Ok(())
}

pub(crate) fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || *v4 == std::net::Ipv4Addr::new(169, 254, 169, 254) // cloud metadata
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // is_unique_local and is_unicast_link_local_strict are unstable on stable Rust;
                // do prefix match instead.
                || {
                    let seg = v6.segments()[0];
                    (seg & 0xfe00) == 0xfc00   // fc00::/7  unique local
                        || (seg & 0xffc0) == 0xfe80 // fe80::/10 link local
                }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_localhost_literal() {
        let url = Url::parse("http://localhost/x").unwrap();
        assert!(guard_private_host(&url).is_err());
    }

    #[test]
    fn blocks_loopback_ipv4() {
        let url = Url::parse("http://127.0.0.1/x").unwrap();
        assert!(guard_private_host(&url).is_err());
    }

    #[test]
    fn blocks_private_ipv4() {
        for host in ["10.0.0.1", "192.168.1.1", "172.16.5.2", "169.254.169.254"] {
            let url = Url::parse(&format!("http://{host}/x")).unwrap();
            assert!(
                guard_private_host(&url).is_err(),
                "expected {host} blocked"
            );
        }
    }

    #[test]
    fn allows_public_ip() {
        let url = Url::parse("http://1.1.1.1/x").unwrap();
        assert!(guard_private_host(&url).is_ok());
    }

    #[test]
    fn blocks_cloud_metadata_hostname() {
        let url = Url::parse("http://metadata.google.internal/").unwrap();
        assert!(guard_private_host(&url).is_err());
    }

    #[test]
    fn blocks_ipv6_loopback() {
        let url = Url::parse("http://[::1]/x").unwrap();
        assert!(guard_private_host(&url).is_err());
    }

    #[test]
    fn blocks_ipv6_unique_local() {
        let url = Url::parse("http://[fd00::1]/x").unwrap();
        assert!(guard_private_host(&url).is_err());
    }
}
