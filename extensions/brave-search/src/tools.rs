use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use serde_json::{json, Value};

use ext_common::{Breaker, BreakerError};

pub const CLIENT_VERSION: &str = "brave-search-0.1.0";
const USER_AGENT: &str = "agent-rs-brave-search/0.1";
const RETRY_BACKOFFS_MS: [u64; 2] = [500, 1000];

fn base_url() -> String {
    std::env::var("BRAVE_SEARCH_URL")
        .unwrap_or_else(|_| "https://api.search.brave.com/res/v1".to_string())
}

fn api_key() -> Option<String> {
    std::env::var("BRAVE_SEARCH_API_KEY").ok().filter(|s| !s.trim().is_empty())
}

fn http() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap()
    })
}

static BREAKER: Lazy<Breaker> =
    Lazy::new(|| Breaker::new(5, Duration::from_secs(30)));

#[doc(hidden)]
pub fn reset_state() { (&*BREAKER).reset(); }

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Key presence + endpoint info.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"brave_search",
          "description":"Web search via Brave. Returns top results with title, url, description, and page_age.",
          "input_schema":{"type":"object","properties":{
              "query":{"type":"string"},
              "count":{"type":"integer","minimum":1,"maximum":20,"description":"default 10"},
              "freshness":{"type":"string","enum":["pd","pw","pm","py"],"description":"past day/week/month/year"},
              "country":{"type":"string","description":"2-letter, e.g. us, co, es"},
              "safesearch":{"type":"string","enum":["off","moderate","strict"],"description":"default moderate"}
          },"required":["query"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "brave_search" => brave_search(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    json!({
        "ok": api_key().is_some(),
        "provider": "brave-search",
        "client_version": CLIENT_VERSION,
        "endpoint": base_url(),
        "api_key_present": api_key().is_some(),
        "tools": ["status","brave_search"],
        "requires": {"bins":[],"env":["BRAVE_SEARCH_API_KEY"]}
    })
}

fn brave_search(args: &Value) -> Result<Value, ToolError> {
    let query = required_string(args, "query")?;
    let count = args.get("count").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(10);
    if !(1..=20).contains(&count) {
        return Err(bad_input("count must be 1..=20"));
    }
    let freshness = optional_string(args, "freshness");
    let country = optional_string(args, "country");
    let safesearch = optional_string(args, "safesearch");

    let Some(key) = api_key() else {
        return Err(ToolError { code: -32041, message: "BRAVE_SEARCH_API_KEY not set".into() });
    };

    let url = format!("{}/web/search", base_url());
    let count_s = count.to_string();
    let mut query_params: Vec<(&str, &str)> = vec![("q", &query), ("count", &count_s)];
    if let Some(f) = freshness.as_deref() { query_params.push(("freshness", f)); }
    if let Some(c) = country.as_deref() { query_params.push(("country", c)); }
    if let Some(s) = safesearch.as_deref() { query_params.push(("safesearch", s)); }

    let resp = call_with_retries(|| {
        http()
            .get(&url)
            .header("X-Subscription-Token", &key)
            .header("Accept", "application/json")
            .query(&query_params)
            .send()
    })?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(match status {
            401 | 403 => ToolError{code:-32011, message: format!("unauthorized: {}", truncate(&body, 200))},
            429 => ToolError{code:-32013, message: "rate limited".into()},
            _ => ToolError{code:-32003, message: format!("HTTP {status}: {}", truncate(&body, 200))}
        });
    }
    let parsed: Value = resp.json().map_err(|e| ToolError{code:-32006, message: e.to_string()})?;
    let web_results = parsed
        .pointer("/web/results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let summary: Vec<Value> = web_results
        .into_iter()
        .take(count as usize)
        .map(|r| json!({
            "title": r.get("title"),
            "url": r.get("url"),
            "description": r.get("description"),
            "page_age": r.get("page_age"),
            "language": r.get("language"),
        }))
        .collect();
    Ok(json!({
        "ok": true,
        "query": query,
        "count": summary.len(),
        "results": summary,
    }))
}

fn call_with_retries<F>(mut send: F) -> Result<reqwest::blocking::Response, ToolError>
where F: FnMut() -> reqwest::Result<reqwest::blocking::Response>
{
    let attempt = |once: &mut F| -> Result<reqwest::blocking::Response, ToolError> {
        match once() {
            Ok(r) => {
                let s = r.status().as_u16();
                if r.status().is_success() { Ok(r) }
                else if s >= 500 { Err(ToolError{code:-32003, message: format!("HTTP {s}")}) }
                else if s == 429 { Err(ToolError{code:-32013, message: "rate limited".into()}) }
                else {
                    // 4xx non-retryable — return response for caller to extract body
                    Ok(r)
                }
            }
            Err(e) if e.is_timeout() => Err(ToolError{code:-32005, message:"timeout".into()}),
            Err(e) => Err(ToolError{code:-32003, message: e.to_string()}),
        }
    };
    let mut last: Option<ToolError> = None;
    for i in 0..=RETRY_BACKOFFS_MS.len() {
        match BREAKER.call(|| attempt(&mut send)) {
            Ok(r) => return Ok(r),
            Err(BreakerError::Rejected) => return Err(ToolError{code:-32004,message:"circuit open".into()}),
            Err(BreakerError::Upstream(e)) => {
                let transient = e.code == -32003 || e.code == -32005 || e.code == -32013;
                last = Some(e);
                if !transient || i == RETRY_BACKOFFS_MS.len() { break; }
                sleep(Duration::from_millis(RETRY_BACKOFFS_MS[i]));
            }
        }
    }
    Err(last.unwrap_or(ToolError{code:-32003,message:"unknown".into()}))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim().to_string();
    if s.is_empty() { return Err(bad_input(format!("`{key}` cannot be empty"))); }
    Ok(s)
}
fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}
