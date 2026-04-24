use std::sync::OnceLock;
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "wikipedia-0.1.0";

fn default_lang() -> String {
    std::env::var("WIKIPEDIA_LANG").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "en".into())
}

fn http() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("agent-rs-wikipedia/0.1 (+https://github.com/lordmacu/agent-rs)")
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap()
    })
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Default language + endpoint.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"search","description":"Search Wikipedia article titles.",
          "input_schema":{"type":"object","properties":{
              "query":{"type":"string"},
              "limit":{"type":"integer","minimum":1,"maximum":20,"description":"default 5"},
              "lang":{"type":"string","description":"ISO code (en, es, fr, ...); default from WIKIPEDIA_LANG or `en`"}
          },"required":["query"],"additionalProperties":false}},
        { "name":"summary","description":"Short REST summary of an article by title. Returns title, description, extract, page URL.",
          "input_schema":{"type":"object","properties":{
              "title":{"type":"string"},
              "lang":{"type":"string"}
          },"required":["title"],"additionalProperties":false}},
        { "name":"extract","description":"Plain-text extract of an article (full intro or specified sections).",
          "input_schema":{"type":"object","properties":{
              "title":{"type":"string"},
              "lang":{"type":"string"},
              "sentences":{"type":"integer","minimum":1,"maximum":40,"description":"default 10 sentences; set 0 for full article"}
          },"required":["title"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "search" => search(args),
        "summary" => summary(args),
        "extract" => extract(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "wikipedia",
        "client_version": CLIENT_VERSION,
        "default_lang": default_lang(),
        "tools": ["status","search","summary","extract"],
        "requires": {"bins":[],"env":[]}
    })
}

fn lang_of(args: &Value) -> String {
    args.get("lang").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(default_lang)
}

fn search(args: &Value) -> Result<Value, ToolError> {
    let q = required_string(args, "query")?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5).min(20);
    let lang = lang_of(args);
    let url = format!("https://{lang}.wikipedia.org/w/api.php");
    let resp = http().get(&url)
        .query(&[
            ("action","query"), ("list","search"), ("format","json"),
            ("srsearch", q.as_str()), ("srlimit", &limit.to_string()),
        ])
        .send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
    if !resp.status().is_success() {
        return Err(ToolError{code:-32003,message:format!("HTTP {}", resp.status())});
    }
    let body: Value = resp.json().map_err(|e| ToolError{code:-32006,message:e.to_string()})?;
    let results: Vec<Value> = body.pointer("/query/search").and_then(|v| v.as_array()).cloned().unwrap_or_default()
        .into_iter().map(|r| json!({
            "title": r.get("title"),
            "snippet": r.get("snippet"),
            "pageid": r.get("pageid"),
            "size": r.get("size"),
            "wordcount": r.get("wordcount"),
        })).collect();
    Ok(json!({"ok": true, "query": q, "lang": lang, "count": results.len(), "results": results}))
}

fn summary(args: &Value) -> Result<Value, ToolError> {
    let title = required_string(args, "title")?;
    let lang = lang_of(args);
    let encoded = urlencoding::encode(&title);
    let url = format!("https://{lang}.wikipedia.org/api/rest_v1/page/summary/{encoded}");
    let resp = http().get(&url).send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(json!({"ok": false, "error": "not_found", "title": title, "lang": lang}));
    }
    if !resp.status().is_success() {
        return Err(ToolError{code:-32003,message:format!("HTTP {status}")});
    }
    let body: Value = resp.json().map_err(|e| ToolError{code:-32006,message:e.to_string()})?;
    Ok(json!({
        "ok": true,
        "title": body.get("title"),
        "description": body.get("description"),
        "extract": body.get("extract"),
        "page_url": body.pointer("/content_urls/desktop/page"),
        "thumbnail": body.pointer("/thumbnail/source"),
        "lang": lang,
    }))
}

fn extract(args: &Value) -> Result<Value, ToolError> {
    let title = required_string(args, "title")?;
    let lang = lang_of(args);
    let sentences = args.get("sentences").and_then(|v| v.as_u64()).unwrap_or(10);
    let url = format!("https://{lang}.wikipedia.org/w/api.php");
    let mut q: Vec<(&str, String)> = vec![
        ("action","query".into()), ("prop","extracts".into()), ("format","json".into()),
        ("explaintext","1".into()), ("redirects","1".into()), ("titles", title.clone()),
    ];
    if sentences > 0 { q.push(("exsentences", sentences.to_string())); q.push(("exintro","1".into())); }
    let resp = http().get(&url).query(&q).send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
    if !resp.status().is_success() {
        return Err(ToolError{code:-32003,message:format!("HTTP {}", resp.status())});
    }
    let body: Value = resp.json().map_err(|e| ToolError{code:-32006,message:e.to_string()})?;
    let pages = body.pointer("/query/pages").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let page = pages.values().next().cloned().unwrap_or(json!({}));
    if page.get("missing").is_some() {
        return Ok(json!({"ok": false, "error": "not_found", "title": title, "lang": lang}));
    }
    Ok(json!({
        "ok": true,
        "title": page.get("title"),
        "pageid": page.get("pageid"),
        "extract": page.get("extract"),
        "lang": lang,
    }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim().to_string();
    if s.is_empty() { return Err(bad_input(format!("`{key}` cannot be empty"))); }
    Ok(s)
}
