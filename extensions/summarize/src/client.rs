use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use serde_json::{json, Value};

use ext_common::{Breaker, BreakerError};

pub const CLIENT_VERSION: &str = "summarize-0.2.0";
const USER_AGENT: &str = "agent-rs-summarize/0.2 (+https://github.com/lordmacu/agent-rs)";
const RETRY_BACKOFFS_MS: [u64; 2] = [500, 1000];
const CB_THRESHOLD: u32 = 5;
const CB_OPEN_SECS: u64 = 30;
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4o-mini";

fn timeout_secs() -> u64 {
    std::env::var("SUMMARIZE_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
}

pub fn base_url() -> String {
    std::env::var("SUMMARIZE_OPENAI_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

pub fn model() -> String {
    std::env::var("SUMMARIZE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

pub fn user_agent() -> &'static str {
    USER_AGENT
}

pub fn token_present() -> bool {
    std::env::var("SUMMARIZE_OPENAI_API_KEY")
        .ok()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false)
}

fn token() -> Result<String, SumError> {
    let raw = std::env::var("SUMMARIZE_OPENAI_API_KEY")
        .map_err(|_| SumError::BadInput("SUMMARIZE_OPENAI_API_KEY env var not set".into()))?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(SumError::BadInput("SUMMARIZE_OPENAI_API_KEY is empty".into()));
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

#[derive(Debug)]
pub enum SumError {
    BadInput(String),
    Unauthorized,
    Forbidden(String),
    Timeout,
    Status(u16),
    Transport(String),
    InvalidJson(String),
    EmptyCompletion,
    CircuitOpen,
}

impl SumError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            SumError::BadInput(_) => -32602,
            SumError::Unauthorized => -32011,
            SumError::Forbidden(_) => -32012,
            SumError::Status(s) if (400..500).contains(&(*s as u32)) => -32002,
            SumError::Status(_) => -32003,
            SumError::CircuitOpen => -32004,
            SumError::Timeout => -32005,
            SumError::InvalidJson(_) => -32006,
            SumError::EmptyCompletion => -32007,
            SumError::Transport(_) => -32003,
        }
    }

    pub fn message(&self) -> String {
        match self {
            SumError::BadInput(m) => m.clone(),
            SumError::Unauthorized => "unauthorized: invalid or missing SUMMARIZE_OPENAI_API_KEY".into(),
            SumError::Forbidden(m) => format!("forbidden: {m}"),
            SumError::Status(s) if (400..500).contains(&(*s as u32)) => {
                format!("provider rejected request: HTTP {s}")
            }
            SumError::Status(_) => "provider unavailable".to_string(),
            SumError::CircuitOpen => "provider circuit open, retry later".to_string(),
            SumError::Timeout => "provider timeout".to_string(),
            SumError::InvalidJson(e) => format!("provider returned invalid json: {e}"),
            SumError::EmptyCompletion => "provider returned no content".to_string(),
            SumError::Transport(e) => format!("provider transport error: {e}"),
        }
    }
}

#[doc(hidden)]
pub fn reset_state() {
    (&*BREAKER).reset();
}

#[derive(Debug, Clone, Copy)]
pub enum Length {
    Short,
    Medium,
    Long,
}

impl Length {
    pub fn label(&self) -> &'static str {
        match self {
            Length::Short => "short",
            Length::Medium => "medium",
            Length::Long => "long",
        }
    }

    fn instruction(&self) -> &'static str {
        match self {
            Length::Short => "Write a concise summary in 1–2 sentences.",
            Length::Medium => "Write a balanced summary in one short paragraph (3–5 sentences).",
            Length::Long => "Write a thorough summary in 6–10 sentences covering main points and supporting detail.",
        }
    }
}

pub struct SummarizeParams<'a> {
    pub text: &'a str,
    pub length: Length,
    pub language: Option<&'a str>,
}

pub fn summarize(params: SummarizeParams<'_>) -> Result<String, SumError> {
    let t = token()?;
    let url = format!("{}/chat/completions", base_url());
    let mut system = format!(
        "You are a precise summarizer. {}",
        params.length.instruction()
    );
    if let Some(lang) = params.language {
        system.push_str(&format!(" Write the summary in {}.", lang));
    }
    let body = json!({
        "model": model(),
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": params.text }
        ],
        "temperature": 0.2,
    });
    let resp = call_with_retries(|| {
        http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {t}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
    })?;
    let parsed: Value = resp.json().map_err(|e| SumError::InvalidJson(e.to_string()))?;
    let content = parsed
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .ok_or(SumError::EmptyCompletion)?;
    if content.is_empty() {
        return Err(SumError::EmptyCompletion);
    }
    Ok(content)
}

fn call_with_retries<F>(mut send: F) -> Result<reqwest::blocking::Response, SumError>
where
    F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, SumError> {
        match once() {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp);
                }
                if status.as_u16() == 401 {
                    return Err(SumError::Unauthorized);
                }
                if status.as_u16() == 403 {
                    let body = resp.text().unwrap_or_default();
                    return Err(SumError::Forbidden(truncate(&body, 200)));
                }
                Err(SumError::Status(status.as_u16()))
            }
            Err(e) if e.is_timeout() => Err(SumError::Timeout),
            Err(e) => Err(SumError::Transport(e.to_string())),
        }
    };

    let mut last_err: Option<SumError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        let outcome = BREAKER.call(|| attempt(&mut send));
        match outcome {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(SumError::CircuitOpen),
            Err(BreakerError::Upstream(e)) => {
                let transient = match &e {
                    SumError::Timeout | SumError::Transport(_) => true,
                    SumError::Status(s) => *s >= 500,
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
    Err(last_err.unwrap_or(SumError::Transport("unknown".into())))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
