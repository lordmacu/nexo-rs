use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use ext_common::{Breaker, BreakerError};
use crate::rate_limit::RateLimiter;

pub const CLIENT_VERSION: &str = "openstreetmap-0.2.0";
const USER_AGENT: &str = "agent-rs-openstreetmap/0.2 (+https://github.com/lordmacu/agent-rs)";
const RETRY_BACKOFFS_MS: [u64; 2] = [500, 1000];
const CB_THRESHOLD: u32 = 5;
const CB_OPEN_SECS: u64 = 30;
const NOMINATIM_MIN_INTERVAL_MS: u64 = 1100; // ~1 req/sec with margin

fn timeout_secs() -> u64 {
    std::env::var("OSM_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

pub fn base_url() -> String {
    std::env::var("OSM_NOMINATIM_URL")
        .unwrap_or_else(|_| "https://nominatim.openstreetmap.org".to_string())
}

pub fn user_agent() -> &'static str {
    USER_AGENT
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
static RATE: Lazy<RateLimiter> =
    Lazy::new(|| RateLimiter::new(Duration::from_millis(NOMINATIM_MIN_INTERVAL_MS)));

#[derive(Debug)]
pub enum OsmError {
    BadInput(String),
    NotFound(String),
    Timeout,
    Status(u16),
    Transport(String),
    InvalidJson(String),
    CircuitOpen,
}

impl OsmError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            OsmError::BadInput(_) => -32602,
            OsmError::NotFound(_) => -32001,
            OsmError::Status(s) if (400..500).contains(&(*s as u32)) => -32002,
            OsmError::Status(_) => -32003,
            OsmError::CircuitOpen => -32004,
            OsmError::Timeout => -32005,
            OsmError::InvalidJson(_) => -32006,
            OsmError::Transport(_) => -32003,
        }
    }

    pub fn message(&self) -> String {
        match self {
            OsmError::BadInput(m) => m.clone(),
            OsmError::NotFound(q) => format!("not found: {q}"),
            OsmError::Status(s) if (400..500).contains(&(*s as u32)) => {
                format!("provider rejected request: HTTP {s}")
            }
            OsmError::Status(_) => "provider unavailable".to_string(),
            OsmError::CircuitOpen => "provider circuit open, retry later".to_string(),
            OsmError::Timeout => "provider timeout".to_string(),
            OsmError::InvalidJson(e) => format!("provider returned invalid json: {e}"),
            OsmError::Transport(e) => format!("provider transport error: {e}"),
        }
    }
}

#[doc(hidden)]
pub fn reset_state() {
    (&*BREAKER).reset();
    (&*RATE).reset();
}

#[derive(Debug, Deserialize, Clone)]
pub struct SearchHit {
    pub display_name: String,
    pub lat: String,
    pub lon: String,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub importance: Option<f64>,
    #[serde(default)]
    pub boundingbox: Option<Vec<String>>,
}

pub struct SearchParams<'a> {
    pub query: &'a str,
    pub limit: u32,
    pub country_codes: Option<&'a str>,
}

pub fn search(params: SearchParams<'_>) -> Result<Vec<SearchHit>, OsmError> {
    let q = params.query.trim();
    if q.is_empty() {
        return Err(OsmError::BadInput("query is required".into()));
    }
    let url = format!("{}/search", base_url());
    let limit_s = params.limit.to_string();
    let mut query: Vec<(&str, String)> = vec![
        ("q", q.to_string()),
        ("format", "jsonv2".into()),
        ("limit", limit_s),
        ("addressdetails", "0".into()),
    ];
    if let Some(cc) = params.country_codes {
        query.push(("countrycodes", cc.to_string()));
    }

    let resp = call_with_retries(|| {
        RATE.acquire();
        http_client().get(&url).query(&query).send()
    })?;
    let hits: Vec<SearchHit> = resp.json().map_err(|e| OsmError::InvalidJson(e.to_string()))?;
    if hits.is_empty() {
        return Err(OsmError::NotFound(q.to_string()));
    }
    Ok(hits)
}

#[derive(Debug, Deserialize)]
pub struct ReverseResult {
    pub display_name: String,
    pub lat: String,
    pub lon: String,
    #[serde(default)]
    pub address: Option<Value>,
}

pub fn reverse(lat: f64, lon: f64, zoom: u32) -> Result<ReverseResult, OsmError> {
    if !(-90.0..=90.0).contains(&lat) {
        return Err(OsmError::BadInput("lat must be in [-90, 90]".into()));
    }
    if !(-180.0..=180.0).contains(&lon) {
        return Err(OsmError::BadInput("lon must be in [-180, 180]".into()));
    }
    if !(0..=18).contains(&zoom) {
        return Err(OsmError::BadInput("zoom must be in [0, 18]".into()));
    }
    let url = format!("{}/reverse", base_url());
    let lat_s = lat.to_string();
    let lon_s = lon.to_string();
    let zoom_s = zoom.to_string();
    let resp = call_with_retries(|| {
        RATE.acquire();
        http_client()
            .get(&url)
            .query(&[
                ("lat", lat_s.as_str()),
                ("lon", lon_s.as_str()),
                ("zoom", zoom_s.as_str()),
                ("format", "jsonv2"),
                ("addressdetails", "1"),
            ])
            .send()
    })?;
    let parsed: Value = resp.json().map_err(|e| OsmError::InvalidJson(e.to_string()))?;
    if parsed.get("error").is_some() {
        return Err(OsmError::NotFound(format!("({lat}, {lon})")));
    }
    serde_json::from_value(parsed).map_err(|e| OsmError::InvalidJson(e.to_string()))
}

fn call_with_retries<F>(mut send: F) -> Result<reqwest::blocking::Response, OsmError>
where
    F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, OsmError> {
        match once() {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    Ok(resp)
                } else if status.is_server_error() {
                    Err(OsmError::Status(status.as_u16()))
                } else {
                    Err(OsmError::Status(status.as_u16()))
                }
            }
            Err(e) if e.is_timeout() => Err(OsmError::Timeout),
            Err(e) => Err(OsmError::Transport(e.to_string())),
        }
    };

    let mut last_err: Option<OsmError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        let outcome = BREAKER.call(|| attempt(&mut send));
        match outcome {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(OsmError::CircuitOpen),
            Err(BreakerError::Upstream(e)) => {
                let transient = match &e {
                    OsmError::Timeout | OsmError::Transport(_) => true,
                    OsmError::Status(s) => *s >= 500,
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
    Err(last_err.unwrap_or(OsmError::Transport("unknown".into())))
}
