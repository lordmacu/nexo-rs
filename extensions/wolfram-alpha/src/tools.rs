use std::sync::OnceLock;
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "wolfram-alpha-0.1.0";

fn base_url() -> String {
    std::env::var("WOLFRAM_BASE_URL").unwrap_or_else(|_| "https://api.wolframalpha.com".into())
}

fn app_id() -> Option<String> {
    std::env::var("WOLFRAM_APP_ID").ok().filter(|s| !s.trim().is_empty())
}

fn http() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("agent-rs-wolfram/0.1")
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
    })
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"App id presence + endpoint.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"wolfram_short",
          "description":"Short answer. Ideal for single-fact queries ('distance earth to moon', '3rd king of Spain', 'sqrt(2)'). Returns a plain-text string or `-32001` if no result.",
          "input_schema":{"type":"object","properties":{
              "input":{"type":"string"},
              "units":{"type":"string","enum":["metric","imperial"],"description":"default metric"}
          },"required":["input"],"additionalProperties":false}},
        { "name":"wolfram_query",
          "description":"Full results JSON: multiple 'pods' of info (primary result, plot URLs, alternative forms, related terms). Better for complex queries needing more than one value.",
          "input_schema":{"type":"object","properties":{
              "input":{"type":"string"},
              "format":{"type":"string","enum":["plaintext","image","html"],"description":"default plaintext"},
              "units":{"type":"string","enum":["metric","imperial"]}
          },"required":["input"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "wolfram_short" => wolfram_short(args),
        "wolfram_query" => wolfram_query(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    json!({
        "ok": app_id().is_some(),
        "provider": "wolfram-alpha",
        "client_version": CLIENT_VERSION,
        "endpoint": base_url(),
        "app_id_present": app_id().is_some(),
        "tools": ["status","wolfram_short","wolfram_query"],
        "requires": {"bins":[],"env":["WOLFRAM_APP_ID"]}
    })
}

fn wolfram_short(args: &Value) -> Result<Value, ToolError> {
    let input = required_string(args, "input")?;
    let units = optional_string(args, "units").unwrap_or_else(|| "metric".into());
    let Some(key) = app_id() else {
        return Err(ToolError{code:-32041, message:"WOLFRAM_APP_ID not set".into()});
    };
    let url = format!("{}/v1/result", base_url());
    let resp = http().get(&url)
        .query(&[("appid", key.as_str()), ("i", input.as_str()), ("units", units.as_str())])
        .send()
        .map_err(|e| ToolError{code:-32003, message: e.to_string()})?;
    let status = resp.status().as_u16();
    if status == 501 {
        // Wolfram uses 501 for "Wolfram|Alpha did not understand your input"
        return Ok(json!({"ok": false, "error":"no_result", "input": input}));
    }
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(ToolError{code:-32003, message: format!("HTTP {status}: {}", truncate(&body, 200))});
    }
    let answer = resp.text().map_err(|e| ToolError{code:-32006, message: e.to_string()})?;
    Ok(json!({
        "ok": true,
        "input": input,
        "units": units,
        "answer": answer,
    }))
}

fn wolfram_query(args: &Value) -> Result<Value, ToolError> {
    let input = required_string(args, "input")?;
    let format = optional_string(args, "format").unwrap_or_else(|| "plaintext".into());
    let units = optional_string(args, "units");
    let Some(key) = app_id() else {
        return Err(ToolError{code:-32041, message:"WOLFRAM_APP_ID not set".into()});
    };
    let url = format!("{}/v2/query", base_url());
    let mut q: Vec<(&str, &str)> = vec![
        ("appid", key.as_str()),
        ("input", input.as_str()),
        ("format", format.as_str()),
        ("output", "JSON"),
    ];
    let units_s;
    if let Some(u) = units.as_deref() { units_s = u.to_string(); q.push(("units", &units_s)); }

    let resp = http().get(&url).query(&q).send()
        .map_err(|e| ToolError{code:-32003, message: e.to_string()})?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(ToolError{code:-32003, message: format!("HTTP {status}: {}", truncate(&body, 200))});
    }
    let parsed: Value = resp.json().map_err(|e| ToolError{code:-32006, message: e.to_string()})?;
    // Pull out just the pods the LLM is likely to want: primary, result, solution, interpretation.
    let pods = parsed.pointer("/queryresult/pods")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let pod_summary: Vec<Value> = pods.iter().map(|p| json!({
        "id": p.get("id"),
        "title": p.get("title"),
        "primary": p.get("primary"),
        "subpods": p.get("subpods"),
    })).collect();
    let success = parsed.pointer("/queryresult/success").and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(json!({
        "ok": success,
        "input": input,
        "success": success,
        "pods_count": pod_summary.len(),
        "pods": pod_summary,
    }))
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
