use std::sync::OnceLock;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "cloudflare-0.1.0";
const BASE_URL: &str = "https://api.cloudflare.com/client/v4";

fn api_token() -> Option<String> {
    std::env::var("CLOUDFLARE_API_TOKEN").ok().filter(|s| !s.trim().is_empty())
}

fn allow_writes() -> bool {
    std::env::var("CLOUDFLARE_ALLOW_WRITES").ok().as_deref() == Some("true")
}

fn allow_purge() -> bool {
    std::env::var("CLOUDFLARE_ALLOW_PURGE").ok().as_deref() == Some("true")
}

fn http() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("agent-rs-cloudflare/0.1")
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap()
    })
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Token presence + endpoint.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"list_zones","description":"List zones visible to the token. Returns id, name, status, plan.",
          "input_schema":{"type":"object","properties":{
              "name":{"type":"string","description":"Exact domain match filter"},
              "per_page":{"type":"integer","minimum":1,"maximum":50}
          },"additionalProperties":false}},
        { "name":"list_dns_records","description":"List DNS records in a zone.",
          "input_schema":{"type":"object","properties":{
              "zone_id":{"type":"string"},
              "type":{"type":"string","description":"A, AAAA, CNAME, MX, TXT, etc."},
              "name":{"type":"string"},
              "per_page":{"type":"integer","minimum":1,"maximum":100}
          },"required":["zone_id"],"additionalProperties":false}},
        { "name":"create_dns_record","description":"Create DNS record. Requires CLOUDFLARE_ALLOW_WRITES=true.",
          "input_schema":{"type":"object","properties":{
              "zone_id":{"type":"string"},
              "type":{"type":"string"},
              "name":{"type":"string"},
              "content":{"type":"string"},
              "ttl":{"type":"integer","description":"1=auto"},
              "proxied":{"type":"boolean"}
          },"required":["zone_id","type","name","content"],"additionalProperties":false}},
        { "name":"update_dns_record","description":"Update DNS record. Requires CLOUDFLARE_ALLOW_WRITES=true.",
          "input_schema":{"type":"object","properties":{
              "zone_id":{"type":"string"},
              "record_id":{"type":"string"},
              "type":{"type":"string"},
              "name":{"type":"string"},
              "content":{"type":"string"},
              "ttl":{"type":"integer"},
              "proxied":{"type":"boolean"}
          },"required":["zone_id","record_id"],"additionalProperties":false}},
        { "name":"delete_dns_record","description":"Delete DNS record. Requires CLOUDFLARE_ALLOW_WRITES=true.",
          "input_schema":{"type":"object","properties":{
              "zone_id":{"type":"string"},
              "record_id":{"type":"string"}
          },"required":["zone_id","record_id"],"additionalProperties":false}},
        { "name":"purge_cache","description":"Purge zone cache. purge_everything OR files/hosts/tags. Requires CLOUDFLARE_ALLOW_PURGE=true.",
          "input_schema":{"type":"object","properties":{
              "zone_id":{"type":"string"},
              "purge_everything":{"type":"boolean"},
              "files":{"type":"array","items":{"type":"string"}},
              "hosts":{"type":"array","items":{"type":"string"}},
              "tags":{"type":"array","items":{"type":"string"}}
          },"required":["zone_id"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }
fn unauthorized(m: impl Into<String>) -> ToolError { ToolError{code:-32041,message:m.into()} }

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "list_zones" => list_zones(args),
        "list_dns_records" => list_dns_records(args),
        "create_dns_record" => { require_writes()?; create_dns_record(args) }
        "update_dns_record" => { require_writes()?; update_dns_record(args) }
        "delete_dns_record" => { require_writes()?; delete_dns_record(args) }
        "purge_cache" => { require_purge()?; purge_cache(args) }
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn require_writes() -> Result<(), ToolError> {
    if !allow_writes() { return Err(unauthorized("CLOUDFLARE_ALLOW_WRITES not set to `true`")); }
    Ok(())
}
fn require_purge() -> Result<(), ToolError> {
    if !allow_purge() { return Err(unauthorized("CLOUDFLARE_ALLOW_PURGE not set to `true`")); }
    Ok(())
}

fn status() -> Value {
    json!({
        "ok": api_token().is_some(),
        "provider": "cloudflare",
        "client_version": CLIENT_VERSION,
        "endpoint": BASE_URL,
        "token_present": api_token().is_some(),
        "allow_writes": allow_writes(),
        "allow_purge": allow_purge(),
        "tools": ["status","list_zones","list_dns_records","create_dns_record","update_dns_record","delete_dns_record","purge_cache"],
        "requires": {"bins":[],"env":["CLOUDFLARE_API_TOKEN"]}
    })
}

fn authed(rb: RequestBuilder, token: &str) -> RequestBuilder {
    rb.header("Authorization", format!("Bearer {token}"))
      .header("Content-Type", "application/json")
}

fn call(rb: RequestBuilder) -> Result<Value, ToolError> {
    let resp = rb.send().map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().unwrap_or(json!({}));
    if !(200..300).contains(&status) {
        let msg = body.pointer("/errors/0/message").and_then(|v| v.as_str()).unwrap_or("unknown");
        return Err(ToolError{code: if status == 401 || status == 403 { -32011 } else { -32003 }, message: format!("HTTP {status}: {msg}")});
    }
    Ok(body)
}

fn token_or_err() -> Result<String, ToolError> {
    api_token().ok_or_else(|| unauthorized("CLOUDFLARE_API_TOKEN not set"))
}

fn list_zones(args: &Value) -> Result<Value, ToolError> {
    let token = token_or_err()?;
    let url = format!("{BASE_URL}/zones");
    let mut rb = authed(http().get(&url), &token);
    if let Some(n) = args.get("name").and_then(|v| v.as_str()) { rb = rb.query(&[("name", n)]); }
    if let Some(p) = args.get("per_page").and_then(|v| v.as_u64()) { rb = rb.query(&[("per_page", p.to_string())]); }
    let body = call(rb)?;
    let zones: Vec<Value> = body.get("result").and_then(|v| v.as_array()).cloned().unwrap_or_default()
        .into_iter().map(|z| json!({
            "id": z.get("id"),
            "name": z.get("name"),
            "status": z.get("status"),
            "plan": z.pointer("/plan/name"),
            "name_servers": z.get("name_servers"),
        })).collect();
    Ok(json!({"ok": true, "count": zones.len(), "zones": zones}))
}

fn list_dns_records(args: &Value) -> Result<Value, ToolError> {
    let token = token_or_err()?;
    let zone = required_string(args, "zone_id")?;
    let url = format!("{BASE_URL}/zones/{zone}/dns_records");
    let mut rb = authed(http().get(&url), &token);
    if let Some(t) = args.get("type").and_then(|v| v.as_str()) { rb = rb.query(&[("type", t)]); }
    if let Some(n) = args.get("name").and_then(|v| v.as_str()) { rb = rb.query(&[("name", n)]); }
    if let Some(p) = args.get("per_page").and_then(|v| v.as_u64()) { rb = rb.query(&[("per_page", p.to_string())]); }
    let body = call(rb)?;
    let records: Vec<Value> = body.get("result").and_then(|v| v.as_array()).cloned().unwrap_or_default()
        .into_iter().map(|r| json!({
            "id": r.get("id"),
            "type": r.get("type"),
            "name": r.get("name"),
            "content": r.get("content"),
            "ttl": r.get("ttl"),
            "proxied": r.get("proxied"),
        })).collect();
    Ok(json!({"ok": true, "count": records.len(), "records": records}))
}

fn create_dns_record(args: &Value) -> Result<Value, ToolError> {
    let token = token_or_err()?;
    let zone = required_string(args, "zone_id")?;
    let url = format!("{BASE_URL}/zones/{zone}/dns_records");
    let mut payload = json!({
        "type": required_string(args, "type")?,
        "name": required_string(args, "name")?,
        "content": required_string(args, "content")?,
    });
    if let Some(v) = args.get("ttl") { payload["ttl"] = v.clone(); }
    if let Some(v) = args.get("proxied") { payload["proxied"] = v.clone(); }
    let body = call(authed(http().post(&url), &token).json(&payload))?;
    Ok(json!({"ok": true, "record": body.get("result")}))
}

fn update_dns_record(args: &Value) -> Result<Value, ToolError> {
    let token = token_or_err()?;
    let zone = required_string(args, "zone_id")?;
    let rec = required_string(args, "record_id")?;
    let url = format!("{BASE_URL}/zones/{zone}/dns_records/{rec}");
    let mut payload = json!({});
    for key in ["type","name","content","ttl","proxied"] {
        if let Some(v) = args.get(key) { payload[key] = v.clone(); }
    }
    let body = call(authed(http().patch(&url), &token).json(&payload))?;
    Ok(json!({"ok": true, "record": body.get("result")}))
}

fn delete_dns_record(args: &Value) -> Result<Value, ToolError> {
    let token = token_or_err()?;
    let zone = required_string(args, "zone_id")?;
    let rec = required_string(args, "record_id")?;
    let url = format!("{BASE_URL}/zones/{zone}/dns_records/{rec}");
    let body = call(authed(http().delete(&url), &token))?;
    Ok(json!({"ok": true, "result": body.get("result")}))
}

fn purge_cache(args: &Value) -> Result<Value, ToolError> {
    let token = token_or_err()?;
    let zone = required_string(args, "zone_id")?;
    let url = format!("{BASE_URL}/zones/{zone}/purge_cache");
    let mut payload = json!({});
    if args.get("purge_everything").and_then(|v| v.as_bool()) == Some(true) {
        payload["purge_everything"] = json!(true);
    } else {
        for key in ["files","hosts","tags"] {
            if let Some(v) = args.get(key) { payload[key] = v.clone(); }
        }
        if payload.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            return Err(bad_input("must provide purge_everything OR files/hosts/tags"));
        }
    }
    let body = call(authed(http().post(&url), &token).json(&payload))?;
    Ok(json!({"ok": true, "result": body.get("result")}))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim().to_string();
    if s.is_empty() { return Err(bad_input(format!("`{key}` cannot be empty"))); }
    Ok(s)
}
