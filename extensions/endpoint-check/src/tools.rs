use std::time::{Duration, Instant};

use serde_json::{json, Value};
use url::Url;

use crate::ssl::{self, SslError};

pub const CLIENT_VERSION: &str = "endpoint-check-0.1.0";
const DEFAULT_TIMEOUT_SECS: u64 = 10;
const MAX_TIMEOUT_SECS: u64 = 60;

pub fn tool_schemas() -> Value {
    json!([
        { "name": "status", "description": "Returns endpoint-check info and limits.",
          "input_schema": {"type":"object","additionalProperties":false}},
        { "name": "http_probe",
          "description": "GET a URL and return status, latency_ms, content_type, final_url, and a short body snippet. Timeouts map to -32005; transport errors to -32003.",
          "input_schema": {
              "type":"object",
              "properties": {
                  "url":           {"type":"string"},
                  "method":        {"type":"string","enum":["GET","HEAD"],"description":"default GET"},
                  "timeout_secs":  {"type":"integer","minimum":1,"maximum":60,"description":"default 10"},
                  "follow_redirects": {"type":"boolean","description":"default true"},
                  "expected_status": {"type":"integer","description":"Compare response status against this — returns `matches_expected` boolean."}
              },
              "required":["url"], "additionalProperties":false
          }
        },
        { "name": "ssl_cert",
          "description": "Inspect the TLS certificate chain of `host:port`. Returns issuer, subject, SAN list, expiry timestamps, seconds-until-expiry, serial, signature alg. Does NOT validate trust (so expired/self-signed certs still return data).",
          "input_schema": {
              "type":"object",
              "properties": {
                  "host":         {"type":"string"},
                  "port":         {"type":"integer","minimum":1,"maximum":65535,"description":"default 443"},
                  "timeout_secs": {"type":"integer","minimum":1,"maximum":60,"description":"default 10"},
                  "warn_days":    {"type":"integer","minimum":0,"maximum":3650,"description":"Flag the cert as expiring-soon when seconds_until_expiry < warn_days*86400. Default 30."}
              },
              "required":["host"], "additionalProperties":false
          }
        }
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl From<SslError> for ToolError {
    fn from(e: SslError) -> Self {
        Self { code: e.rpc_code(), message: e.message() }
    }
}

fn bad_input(msg: impl Into<String>) -> ToolError {
    ToolError { code: -32602, message: msg.into() }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "http_probe" => http_probe(args),
        "ssl_cert" => ssl_cert(args),
        other => Err(ToolError { code: -32601, message: format!("unknown tool `{other}`") }),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "endpoint-check (reqwest + rustls)",
        "client_version": CLIENT_VERSION,
        "tools": ["status","http_probe","ssl_cert"],
        "limits": {"default_timeout_secs": DEFAULT_TIMEOUT_SECS, "max_timeout_secs": MAX_TIMEOUT_SECS},
        "requires": {"bins": [], "env": []}
    })
}

fn http_probe(args: &Value) -> Result<Value, ToolError> {
    let url_s = required_string(args, "url")?;
    let url = Url::parse(&url_s).map_err(|e| bad_input(format!("bad url: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(bad_input(format!("unsupported scheme `{other}`"))),
    }
    let method = optional_string(args, "method").unwrap_or_else(|| "GET".to_string());
    let timeout = optional_u64(args, "timeout_secs")?.unwrap_or(DEFAULT_TIMEOUT_SECS);
    if timeout == 0 || timeout > MAX_TIMEOUT_SECS {
        return Err(bad_input(format!("timeout_secs must be 1..={MAX_TIMEOUT_SECS}")));
    }
    let follow = args.get("follow_redirects").and_then(|v| v.as_bool()).unwrap_or(true);
    let expected_status = args.get("expected_status").and_then(|v| v.as_u64()).map(|n| n as u16);

    let redirect = if follow {
        reqwest::redirect::Policy::limited(5)
    } else {
        reqwest::redirect::Policy::none()
    };
    let client = reqwest::blocking::Client::builder()
        .user_agent("agent-rs-endpoint-check/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(timeout))
        .redirect(redirect)
        .build()
        .map_err(|e| ToolError { code: -32003, message: format!("client build: {e}") })?;

    let start = Instant::now();
    let req = match method.as_str() {
        "GET" => client.get(url),
        "HEAD" => client.head(url),
        _ => return Err(bad_input("method must be GET or HEAD")),
    };
    let resp = match req.send() {
        Ok(r) => r,
        Err(e) if e.is_timeout() => return Err(ToolError { code: -32005, message: "timeout".into() }),
        Err(e) => return Err(ToolError { code: -32003, message: format!("transport: {e}") }),
    };
    let latency_ms = start.elapsed().as_millis() as u64;
    let status_code = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = resp.text().unwrap_or_default();
    let body_preview = if body.chars().count() > 500 {
        format!("{}…", body.chars().take(500).collect::<String>())
    } else {
        body
    };

    let mut out = json!({
        "ok": true,
        "status": status_code,
        "latency_ms": latency_ms,
        "final_url": final_url,
        "content_type": content_type,
        "body_preview": body_preview,
    });
    if let Some(exp) = expected_status {
        out["expected_status"] = json!(exp);
        out["matches_expected"] = json!(status_code == exp);
    }
    Ok(out)
}

fn ssl_cert(args: &Value) -> Result<Value, ToolError> {
    let host = required_string(args, "host")?;
    let port = optional_u64(args, "port")?.unwrap_or(443) as u16;
    let timeout = optional_u64(args, "timeout_secs")?.unwrap_or(DEFAULT_TIMEOUT_SECS);
    if timeout == 0 || timeout > MAX_TIMEOUT_SECS {
        return Err(bad_input(format!("timeout_secs must be 1..={MAX_TIMEOUT_SECS}")));
    }
    let warn_days = optional_u64(args, "warn_days")?.unwrap_or(30) as i64;

    let info = ssl::inspect(&host, port, timeout)?;
    let expiring_soon = info.seconds_until_expiry < warn_days * 86_400;

    Ok(json!({
        "ok": true,
        "host": host,
        "port": port,
        "subject": info.subject,
        "issuer": info.issuer,
        "sans": info.sans,
        "serial_hex": info.serial_hex,
        "signature_algorithm": info.signature_algorithm,
        "chain_length": info.chain_length,
        "not_before_unix": info.not_before_unix,
        "not_after_unix": info.not_after_unix,
        "seconds_until_expiry": info.seconds_until_expiry,
        "days_until_expiry": info.seconds_until_expiry / 86_400,
        "warn_days": warn_days,
        "expiring_soon": expiring_soon,
        "expired": info.seconds_until_expiry < 0,
    }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing or invalid `{key}`")))?
        .trim().to_string();
    if s.is_empty() {
        return Err(bad_input(format!("`{key}` cannot be empty")));
    }
    Ok(s)
}
fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| bad_input(format!("`{key}` must be integer"))),
    }
}
