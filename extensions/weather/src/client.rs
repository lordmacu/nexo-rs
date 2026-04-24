use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use ext_common::{Breaker, BreakerError};
use crate::cache::{GeoCache, GeoEntry};

pub const CLIENT_VERSION: &str = "weather-0.2.0";
const USER_AGENT: &str = "agent-rs-weather/0.2 (+https://github.com/lordmacu/agent-rs)";
const RETRY_BACKOFFS_MS: [u64; 2] = [500, 1000];
const CB_THRESHOLD: u32 = 5;
const CB_OPEN_SECS: u64 = 30;
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const CACHE_CAP: usize = 1000;

fn timeout_secs() -> u64 {
    std::env::var("WEATHER_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

pub fn geocoding_endpoint() -> String {
    std::env::var("WEATHER_GEOCODING_URL")
        .unwrap_or_else(|_| "https://geocoding-api.open-meteo.com/v1/search".to_string())
}

pub fn forecast_endpoint() -> String {
    std::env::var("WEATHER_FORECAST_URL")
        .unwrap_or_else(|_| "https://api.open-meteo.com/v1/forecast".to_string())
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

static GEO_BREAKER: Lazy<Breaker> =
    Lazy::new(|| Breaker::new(CB_THRESHOLD, Duration::from_secs(CB_OPEN_SECS)));
static FORECAST_BREAKER: Lazy<Breaker> =
    Lazy::new(|| Breaker::new(CB_THRESHOLD, Duration::from_secs(CB_OPEN_SECS)));
static GEO_CACHE: Lazy<GeoCache> =
    Lazy::new(|| GeoCache::new(Duration::from_secs(CACHE_TTL_SECS), CACHE_CAP));

#[derive(Debug)]
pub enum WeatherError {
    BadInput(String),
    NotFound(String),
    Timeout,
    Status(u16),
    Transport(String),
    InvalidJson(String),
    CircuitOpen,
}

impl WeatherError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            WeatherError::BadInput(_) => -32602,
            WeatherError::NotFound(_) => -32001,
            WeatherError::Status(s) if (400..500).contains(&(*s as u32)) => -32002,
            WeatherError::Status(_) => -32003,
            WeatherError::CircuitOpen => -32004,
            WeatherError::Timeout => -32005,
            WeatherError::InvalidJson(_) => -32006,
            WeatherError::Transport(_) => -32003,
        }
    }

    pub fn message(&self) -> String {
        match self {
            WeatherError::BadInput(m) => m.clone(),
            WeatherError::NotFound(q) => format!("location not found: {q}"),
            WeatherError::Status(s) if (400..500).contains(&(*s as u32)) => {
                format!("provider rejected request: HTTP {s}")
            }
            WeatherError::Status(_) => "provider unavailable".to_string(),
            WeatherError::CircuitOpen => "provider circuit open, retry later".to_string(),
            WeatherError::Timeout => "provider timeout".to_string(),
            WeatherError::InvalidJson(e) => format!("provider returned invalid json: {e}"),
            WeatherError::Transport(e) => format!("provider transport error: {e}"),
        }
    }
}

pub enum Units {
    Metric,
    Imperial,
}

impl Units {
    pub fn temperature_param(&self) -> &'static str {
        match self {
            Units::Metric => "celsius",
            Units::Imperial => "fahrenheit",
        }
    }

    pub fn wind_param(&self) -> &'static str {
        match self {
            Units::Metric => "kmh",
            Units::Imperial => "mph",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Units::Metric => "metric",
            Units::Imperial => "imperial",
        }
    }
}

#[derive(Debug, Deserialize)]
struct GeoSearchResponse {
    results: Option<Vec<GeoSearchHit>>,
}

#[derive(Debug, Deserialize)]
struct GeoSearchHit {
    name: String,
    country: Option<String>,
    timezone: Option<String>,
    latitude: f64,
    longitude: f64,
}

pub fn geocode(query: &str) -> Result<GeoEntry, WeatherError> {
    let q = query.trim();
    if q.is_empty() {
        return Err(WeatherError::BadInput("location is required".into()));
    }
    if let Some(hit) = GEO_CACHE.get(q) {
        return Ok(hit);
    }
    let url = geocoding_endpoint();
    let resp = call_with_retries(&GEO_BREAKER, || {
        http_client()
            .get(&url)
            .query(&[("name", q), ("count", "1"), ("language", "en"), ("format", "json")])
            .send()
    })?;
    let parsed: GeoSearchResponse = resp
        .json()
        .map_err(|e| WeatherError::InvalidJson(e.to_string()))?;
    let hit = parsed
        .results
        .and_then(|v| v.into_iter().next())
        .ok_or_else(|| WeatherError::NotFound(q.to_string()))?;
    let entry = GeoEntry {
        name: hit.name,
        country: hit.country.unwrap_or_default(),
        timezone: hit.timezone.unwrap_or_else(|| "UTC".into()),
        lat: hit.latitude,
        lon: hit.longitude,
    };
    GEO_CACHE.put(q, entry.clone());
    Ok(entry)
}

#[doc(hidden)]
pub fn reset_state() {
    (&*GEO_BREAKER).reset();
    (&*FORECAST_BREAKER).reset();
    (&*GEO_CACHE).clear();
}

pub fn forecast(lat: f64, lon: f64, days: u32, units: &Units) -> Result<Value, WeatherError> {
    let url = forecast_endpoint();
    let lat_s = lat.to_string();
    let lon_s = lon.to_string();
    let days_s = days.to_string();
    let resp = call_with_retries(&FORECAST_BREAKER, || {
        http_client()
            .get(&url)
            .query(&[
                ("latitude", lat_s.as_str()),
                ("longitude", lon_s.as_str()),
                (
                    "current",
                    "temperature_2m,apparent_temperature,relative_humidity_2m,wind_speed_10m,wind_direction_10m,precipitation,weather_code,is_day",
                ),
                (
                    "daily",
                    "temperature_2m_min,temperature_2m_max,precipitation_sum,wind_speed_10m_max,weather_code",
                ),
                ("timezone", "auto"),
                ("forecast_days", days_s.as_str()),
                ("temperature_unit", units.temperature_param()),
                ("wind_speed_unit", units.wind_param()),
            ])
            .send()
    })?;
    resp.json::<Value>()
        .map_err(|e| WeatherError::InvalidJson(e.to_string()))
}

fn call_with_retries<F>(breaker: &Breaker, mut send: F) -> Result<reqwest::blocking::Response, WeatherError>
where
    F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, WeatherError> {
        match once() {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    Ok(resp)
                } else if status.is_server_error() {
                    Err(WeatherError::Status(status.as_u16()))
                } else {
                    // 4xx — surface immediately, no retry, fail circuit too.
                    Err(WeatherError::Status(status.as_u16()))
                }
            }
            Err(e) if e.is_timeout() => Err(WeatherError::Timeout),
            Err(e) => Err(WeatherError::Transport(e.to_string())),
        }
    };

    // First attempt + retries on transient errors.
    let mut last_err: Option<WeatherError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        let outcome = breaker.call(|| attempt(&mut send));
        match outcome {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(WeatherError::CircuitOpen),
            Err(BreakerError::Upstream(e)) => {
                let transient = match &e {
                    WeatherError::Timeout | WeatherError::Transport(_) => true,
                    WeatherError::Status(s) => *s >= 500,
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
    Err(last_err.unwrap_or(WeatherError::Transport("unknown".into())))
}

