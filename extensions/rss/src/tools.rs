use std::sync::OnceLock;
use std::time::Duration;

use feed_rs::parser;
use reqwest::blocking::Client;
use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "rss-0.1.0";

fn http() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("agent-rs-rss/0.1")
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap()
    })
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Extension info.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"fetch_feed",
          "description":"Fetch and parse an RSS/Atom/JSON feed. Returns normalized entries sorted newest first.",
          "input_schema":{"type":"object","properties":{
              "url":{"type":"string"},
              "limit":{"type":"integer","minimum":1,"maximum":100,"description":"default 20"},
              "include_content":{"type":"boolean","description":"default false; when true, include entry HTML/text body"}
          },"required":["url"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "fetch_feed" => fetch_feed(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "rss",
        "client_version": CLIENT_VERSION,
        "tools": ["status","fetch_feed"],
        "requires": {"bins":[],"env":[]}
    })
}

fn fetch_feed(args: &Value) -> Result<Value, ToolError> {
    let url = required_string(args, "url")?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(bad_input("url must be http(s)"));
    }
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(100) as usize;
    let include_content = args.get("include_content").and_then(|v| v.as_bool()).unwrap_or(false);

    let resp = http().get(&url)
        .header("Accept", "application/rss+xml, application/atom+xml, application/json, application/xml, text/xml, */*")
        .send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(ToolError{code:-32003,message:format!("HTTP {status}")});
    }
    let bytes = resp.bytes().map_err(|e| ToolError{code:-32006,message:e.to_string()})?;
    let feed = parser::parse(bytes.as_ref()).map_err(|e| ToolError{code:-32006,message:format!("parse failed: {e}")})?;

    let title = feed.title.as_ref().map(|t| t.content.clone());
    let desc = feed.description.as_ref().map(|t| t.content.clone());
    let link = feed.links.first().map(|l| l.href.clone());

    let mut entries: Vec<Value> = feed.entries.into_iter().map(|e| {
        let published = e.published.or(e.updated).map(|d| d.to_rfc3339());
        let ent_title = e.title.map(|t| t.content);
        let ent_link = e.links.first().map(|l| l.href.clone());
        let summary = e.summary.map(|t| t.content);
        let content_body = if include_content { e.content.and_then(|c| c.body) } else { None };
        json!({
            "id": e.id,
            "title": ent_title,
            "link": ent_link,
            "published": published,
            "summary": summary,
            "content": content_body,
            "authors": e.authors.into_iter().map(|a| a.name).collect::<Vec<_>>(),
        })
    }).collect();
    entries.sort_by(|a,b| b.get("published").and_then(|v| v.as_str()).unwrap_or("")
        .cmp(a.get("published").and_then(|v| v.as_str()).unwrap_or("")));
    entries.truncate(limit);

    Ok(json!({
        "ok": true,
        "feed": {
            "title": title,
            "description": desc,
            "link": link,
            "feed_url": url,
        },
        "count": entries.len(),
        "entries": entries,
    }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim().to_string();
    if s.is_empty() { return Err(bad_input(format!("`{key}` cannot be empty"))); }
    Ok(s)
}
