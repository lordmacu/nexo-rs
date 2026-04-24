use std::path::Path;
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::{multipart, Client};
use serde_json::Value;

use ext_common::{Breaker, BreakerError};

pub const CLIENT_VERSION: &str = "openai-whisper-0.2.0";
const USER_AGENT: &str = "agent-rs-whisper/0.2 (+https://github.com/lordmacu/agent-rs)";
const RETRY_BACKOFFS_MS: [u64; 1] = [1500];
const CB_THRESHOLD: u32 = 5;
const CB_OPEN_SECS: u64 = 60;
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "whisper-1";

fn timeout_secs() -> u64 {
    std::env::var("WHISPER_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
}

pub fn base_url() -> String {
    std::env::var("WHISPER_OPENAI_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

pub fn default_model() -> String {
    std::env::var("WHISPER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

pub fn user_agent() -> &'static str {
    USER_AGENT
}

pub fn token_present() -> bool {
    std::env::var("WHISPER_OPENAI_API_KEY")
        .ok()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false)
}

fn token() -> Result<String, WhisperError> {
    let raw = std::env::var("WHISPER_OPENAI_API_KEY")
        .map_err(|_| WhisperError::BadInput("WHISPER_OPENAI_API_KEY env var not set".into()))?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(WhisperError::BadInput("WHISPER_OPENAI_API_KEY is empty".into()));
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
pub enum WhisperError {
    BadInput(String),
    Unauthorized,
    Forbidden(String),
    PayloadTooLarge,
    UnsupportedMedia(String),
    Timeout,
    Status(u16),
    Transport(String),
    InvalidJson(String),
    EmptyTranscript,
    CircuitOpen,
}

impl WhisperError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            WhisperError::BadInput(_) => -32602,
            WhisperError::Unauthorized => -32011,
            WhisperError::Forbidden(_) => -32012,
            WhisperError::PayloadTooLarge => -32014,
            WhisperError::UnsupportedMedia(_) => -32015,
            WhisperError::Status(s) if (400..500).contains(&(*s as u32)) => -32002,
            WhisperError::Status(_) => -32003,
            WhisperError::CircuitOpen => -32004,
            WhisperError::Timeout => -32005,
            WhisperError::InvalidJson(_) => -32006,
            WhisperError::EmptyTranscript => -32007,
            WhisperError::Transport(_) => -32003,
        }
    }

    pub fn message(&self) -> String {
        match self {
            WhisperError::BadInput(m) => m.clone(),
            WhisperError::Unauthorized => "unauthorized: invalid or missing WHISPER_OPENAI_API_KEY".into(),
            WhisperError::Forbidden(m) => format!("forbidden: {m}"),
            WhisperError::PayloadTooLarge => "audio file exceeds provider limit (typ. 25 MB)".into(),
            WhisperError::UnsupportedMedia(m) => format!("unsupported media: {m}"),
            WhisperError::Status(s) if (400..500).contains(&(*s as u32)) => {
                format!("provider rejected request: HTTP {s}")
            }
            WhisperError::Status(_) => "provider unavailable".to_string(),
            WhisperError::CircuitOpen => "provider circuit open, retry later".to_string(),
            WhisperError::Timeout => "provider timeout".to_string(),
            WhisperError::InvalidJson(e) => format!("provider returned invalid json: {e}"),
            WhisperError::EmptyTranscript => "provider returned empty transcript".to_string(),
            WhisperError::Transport(e) => format!("provider transport error: {e}"),
        }
    }
}

#[doc(hidden)]
pub fn reset_state() {
    (&*BREAKER).reset();
}

#[derive(Debug, Clone, Copy)]
pub enum ResponseFormat {
    Text,
    Json,
    VerboseJson,
    Srt,
    Vtt,
}

impl ResponseFormat {
    pub fn label(&self) -> &'static str {
        match self {
            ResponseFormat::Text => "text",
            ResponseFormat::Json => "json",
            ResponseFormat::VerboseJson => "verbose_json",
            ResponseFormat::Srt => "srt",
            ResponseFormat::Vtt => "vtt",
        }
    }
}

pub struct TranscribeParams<'a> {
    pub file_path: &'a Path,
    pub file_bytes: Vec<u8>,
    pub model: String,
    pub language: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub format: ResponseFormat,
    pub temperature: Option<f64>,
}

pub enum Transcript {
    Plain(String),
    Json(Value),
}

pub fn transcribe(params: TranscribeParams<'_>) -> Result<Transcript, WhisperError> {
    let t = token()?;
    let url = format!("{}/audio/transcriptions", base_url());

    let filename = params
        .file_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "audio.bin".to_string());

    // Build the form once. reqwest::blocking::multipart::Form is not Clone, so
    // for retries we re-build it inside the closure from owned bytes/strings.
    let bytes = params.file_bytes.clone();
    let model = params.model.clone();
    let format = params.format;
    let language = params.language.map(|s| s.to_string());
    let prompt = params.prompt.map(|s| s.to_string());
    let temperature = params.temperature;

    let resp = call_with_retries(|| {
        let part = multipart::Part::bytes(bytes.clone())
            .file_name(filename.clone())
            .mime_str("application/octet-stream")
            .expect("static mime");
        let mut form = multipart::Form::new()
            .part("file", part)
            .text("model", model.clone())
            .text("response_format", format.label().to_string());
        if let Some(l) = language.as_ref() {
            form = form.text("language", l.clone());
        }
        if let Some(p) = prompt.as_ref() {
            form = form.text("prompt", p.clone());
        }
        if let Some(temp) = temperature {
            form = form.text("temperature", format!("{temp}"));
        }
        http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {t}"))
            .multipart(form)
            .send()
    })?;

    match params.format {
        ResponseFormat::Text | ResponseFormat::Srt | ResponseFormat::Vtt => {
            let body = resp
                .text()
                .map_err(|e| WhisperError::Transport(e.to_string()))?;
            let trimmed = body.trim();
            if trimmed.is_empty() {
                return Err(WhisperError::EmptyTranscript);
            }
            Ok(Transcript::Plain(trimmed.to_string()))
        }
        ResponseFormat::Json | ResponseFormat::VerboseJson => {
            let parsed: Value = resp
                .json()
                .map_err(|e| WhisperError::InvalidJson(e.to_string()))?;
            // Validate at least a `text` field is present.
            let has_text = parsed
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if !has_text {
                return Err(WhisperError::EmptyTranscript);
            }
            Ok(Transcript::Json(parsed))
        }
    }
}

fn call_with_retries<F>(mut send: F) -> Result<reqwest::blocking::Response, WhisperError>
where
    F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, WhisperError> {
        match once() {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp);
                }
                match status.as_u16() {
                    401 => Err(WhisperError::Unauthorized),
                    403 => {
                        let body = resp.text().unwrap_or_default();
                        Err(WhisperError::Forbidden(truncate(&body, 200)))
                    }
                    413 => Err(WhisperError::PayloadTooLarge),
                    415 => {
                        let body = resp.text().unwrap_or_default();
                        Err(WhisperError::UnsupportedMedia(truncate(&body, 200)))
                    }
                    s => Err(WhisperError::Status(s)),
                }
            }
            Err(e) if e.is_timeout() => Err(WhisperError::Timeout),
            Err(e) => Err(WhisperError::Transport(e.to_string())),
        }
    };

    let mut last_err: Option<WhisperError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        let outcome = BREAKER.call(|| attempt(&mut send));
        match outcome {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(WhisperError::CircuitOpen),
            Err(BreakerError::Upstream(e)) => {
                let transient = match &e {
                    WhisperError::Timeout | WhisperError::Transport(_) => true,
                    WhisperError::Status(s) => *s >= 500,
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
    Err(last_err.unwrap_or(WhisperError::Transport("unknown".into())))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
