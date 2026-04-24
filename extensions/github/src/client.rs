use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::Value;

use ext_common::{Breaker, BreakerError};

pub const CLIENT_VERSION: &str = "github-0.2.0";
const USER_AGENT: &str = "agent-rs-github/0.2 (+https://github.com/lordmacu/agent-rs)";
const RETRY_BACKOFFS_MS: [u64; 2] = [500, 1000];
const CB_THRESHOLD: u32 = 5;
const CB_OPEN_SECS: u64 = 30;
const ACCEPT_HEADER: &str = "application/vnd.github+json";
const API_VERSION: &str = "2022-11-28";

fn timeout_secs() -> u64 {
    std::env::var("GITHUB_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15)
}

pub fn base_url() -> String {
    std::env::var("GITHUB_API_URL").unwrap_or_else(|_| "https://api.github.com".to_string())
}

pub fn user_agent() -> &'static str {
    USER_AGENT
}

pub fn token_present() -> bool {
    std::env::var("GITHUB_TOKEN")
        .ok()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false)
}

fn token() -> Result<String, GithubError> {
    let raw = std::env::var("GITHUB_TOKEN").map_err(|_| {
        GithubError::BadInput("GITHUB_TOKEN env var not set".into())
    })?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(GithubError::BadInput("GITHUB_TOKEN is empty".into()));
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
pub enum GithubError {
    BadInput(String),
    NotFound(String),
    Unauthorized,
    Forbidden(String),
    RateLimited { reset_at: Option<u64> },
    Timeout,
    Status(u16),
    Transport(String),
    InvalidJson(String),
    CircuitOpen,
}

impl GithubError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            GithubError::BadInput(_) => -32602,
            GithubError::NotFound(_) => -32001,
            GithubError::Unauthorized => -32011,
            GithubError::Forbidden(_) => -32012,
            GithubError::RateLimited { .. } => -32013,
            GithubError::Status(s) if (400..500).contains(&(*s as u32)) => -32002,
            GithubError::Status(_) => -32003,
            GithubError::CircuitOpen => -32004,
            GithubError::Timeout => -32005,
            GithubError::InvalidJson(_) => -32006,
            GithubError::Transport(_) => -32003,
        }
    }

    pub fn message(&self) -> String {
        match self {
            GithubError::BadInput(m) => m.clone(),
            GithubError::NotFound(q) => format!("not found: {q}"),
            GithubError::Unauthorized => "unauthorized: invalid or missing GITHUB_TOKEN".into(),
            GithubError::Forbidden(m) => format!("forbidden: {m}"),
            GithubError::RateLimited { reset_at } => match reset_at {
                Some(epoch) => format!("rate limited; resets at unix epoch {epoch}"),
                None => "rate limited by GitHub API".into(),
            },
            GithubError::Status(s) if (400..500).contains(&(*s as u32)) => {
                format!("provider rejected request: HTTP {s}")
            }
            GithubError::Status(_) => "provider unavailable".to_string(),
            GithubError::CircuitOpen => "provider circuit open, retry later".to_string(),
            GithubError::Timeout => "provider timeout".to_string(),
            GithubError::InvalidJson(e) => format!("provider returned invalid json: {e}"),
            GithubError::Transport(e) => format!("provider transport error: {e}"),
        }
    }
}

#[doc(hidden)]
pub fn reset_state() {
    (&*BREAKER).reset();
}

fn build(req: RequestBuilder, t: &str) -> RequestBuilder {
    req.header("Authorization", format!("Bearer {t}"))
        .header("Accept", ACCEPT_HEADER)
        .header("X-GitHub-Api-Version", API_VERSION)
}

#[derive(Debug, Deserialize)]
pub struct AuthUser {
    pub login: String,
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
}

pub fn authenticated_user() -> Result<AuthUser, GithubError> {
    let t = token()?;
    let url = format!("{}/user", base_url());
    let resp = call_with_retries(|| build(http_client().get(&url), &t).send())?;
    resp.json().map_err(|e| GithubError::InvalidJson(e.to_string()))
}

pub fn list_pulls(
    owner: &str,
    repo: &str,
    state: &str,
    limit: u32,
) -> Result<Vec<Value>, GithubError> {
    let t = token()?;
    let url = format!("{}/repos/{}/{}/pulls", base_url(), owner, repo);
    let limit_s = limit.min(100).to_string();
    let resp = call_with_retries(|| {
        build(
            http_client()
                .get(&url)
                .query(&[("state", state), ("per_page", limit_s.as_str())]),
            &t,
        )
        .send()
    })?;
    resp.json().map_err(|e| GithubError::InvalidJson(e.to_string()))
}

pub fn get_pull(owner: &str, repo: &str, number: u64) -> Result<Value, GithubError> {
    let t = token()?;
    let url = format!("{}/repos/{}/{}/pulls/{}", base_url(), owner, repo, number);
    let resp = call_with_retries(|| build(http_client().get(&url), &t).send())?;
    resp.json().map_err(|e| GithubError::InvalidJson(e.to_string()))
}

pub fn list_check_runs(owner: &str, repo: &str, sha: &str) -> Result<Value, GithubError> {
    let t = token()?;
    let url = format!(
        "{}/repos/{}/{}/commits/{}/check-runs",
        base_url(),
        owner,
        repo,
        sha
    );
    let resp = call_with_retries(|| build(http_client().get(&url), &t).send())?;
    resp.json().map_err(|e| GithubError::InvalidJson(e.to_string()))
}

pub fn list_issues(
    owner: &str,
    repo: &str,
    state: &str,
    limit: u32,
) -> Result<Vec<Value>, GithubError> {
    let t = token()?;
    let url = format!("{}/repos/{}/{}/issues", base_url(), owner, repo);
    let limit_s = limit.min(100).to_string();
    let resp = call_with_retries(|| {
        build(
            http_client()
                .get(&url)
                .query(&[("state", state), ("per_page", limit_s.as_str())]),
            &t,
        )
        .send()
    })?;
    resp.json().map_err(|e| GithubError::InvalidJson(e.to_string()))
}

fn call_with_retries<F>(mut send: F) -> Result<reqwest::blocking::Response, GithubError>
where
    F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, GithubError> {
        match once() {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp);
                }
                if status.as_u16() == 401 {
                    return Err(GithubError::Unauthorized);
                }
                if status.as_u16() == 403 {
                    let remaining = resp
                        .headers()
                        .get("x-ratelimit-remaining")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u32>().ok());
                    if remaining == Some(0) {
                        let reset = resp
                            .headers()
                            .get("x-ratelimit-reset")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok());
                        return Err(GithubError::RateLimited { reset_at: reset });
                    }
                    let body = resp.text().unwrap_or_default();
                    return Err(GithubError::Forbidden(truncate(&body, 200)));
                }
                if status.as_u16() == 404 {
                    return Err(GithubError::NotFound("resource".into()));
                }
                Err(GithubError::Status(status.as_u16()))
            }
            Err(e) if e.is_timeout() => Err(GithubError::Timeout),
            Err(e) => Err(GithubError::Transport(e.to_string())),
        }
    };

    let mut last_err: Option<GithubError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        let outcome = BREAKER.call(|| attempt(&mut send));
        match outcome {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(GithubError::CircuitOpen),
            Err(BreakerError::Upstream(e)) => {
                let transient = match &e {
                    GithubError::Timeout | GithubError::Transport(_) => true,
                    GithubError::Status(s) => *s >= 500,
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
    Err(last_err.unwrap_or(GithubError::Transport("unknown".into())))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
