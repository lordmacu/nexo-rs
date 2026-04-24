use serde_json::{json, Value};

use crate::client::{
    self, FetchError, FetchRequest, CLIENT_VERSION, DEFAULT_MAX_BYTES, DEFAULT_TIMEOUT_SECS,
    HARD_MAX_BYTES, MAX_TIMEOUT_SECS,
};

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns fetch-url extension info, limits, and policy flags.",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "fetch_url",
            "description": "HTTP GET/POST a URL with size cap, SSRF guard, retries, and circuit breaker. Returns body as text (when decodable UTF-8 or text-like content-type) or base64 otherwise.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "url":     { "type": "string", "description": "http(s) URL" },
                    "method":  { "type": "string", "enum": ["GET","POST","PUT","DELETE","HEAD","PATCH","OPTIONS"], "description": "default GET" },
                    "headers": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Header map. Host/User-Agent already set by the extension."
                    },
                    "body": { "type": "string", "description": "Raw request body (caller controls Content-Type via headers)." },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": HARD_MAX_BYTES, "description": "Response read cap in bytes." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Per-request timeout." },
                    "allow_private": { "type": "boolean", "description": "Allow private/loopback/metadata hosts. Default false." }
                },
                "required": ["url"],
                "additionalProperties": false
            }
        }
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl From<FetchError> for ToolError {
    fn from(e: FetchError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "fetch_url" => fetch_url(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "fetch-url (rust / reqwest)",
        "client_version": CLIENT_VERSION,
        "user_agent": client::user_agent(),
        "tools": ["status", "fetch_url"],
        "limits": {
            "default_max_bytes": DEFAULT_MAX_BYTES,
            "hard_max_bytes": HARD_MAX_BYTES,
            "default_timeout_secs": DEFAULT_TIMEOUT_SECS,
            "max_timeout_secs": MAX_TIMEOUT_SECS,
        },
        "policy": {
            "blocks_private_hosts_by_default": true,
            "allowed_schemes": ["http", "https"],
        }
    })
}

fn fetch_url(args: &Value) -> Result<Value, ToolError> {
    let url = required_string(args, "url")?;
    let method = optional_string(args, "method").unwrap_or_else(|| "GET".to_string());
    let headers = parse_headers(args)?;
    let body = optional_string(args, "body");
    let max_bytes = optional_usize(args, "max_bytes")?.unwrap_or(DEFAULT_MAX_BYTES);
    let timeout_secs = optional_u64(args, "timeout_secs")?.unwrap_or(DEFAULT_TIMEOUT_SECS);
    let allow_private = args
        .get("allow_private")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let outcome = client::fetch(FetchRequest {
        url: &url,
        method: &method,
        headers,
        body,
        max_bytes,
        timeout_secs,
        allow_private,
    })?;

    let headers_json: Value = outcome
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect::<serde_json::Map<_, _>>()
        .into();

    Ok(json!({
        "status": outcome.status,
        "final_url": outcome.final_url,
        "headers": headers_json,
        "content_type": outcome.content_type,
        "truncated": outcome.truncated,
        "bytes_read": outcome.bytes_read,
        "body_text": outcome.body_text,
        "body_base64": outcome.body_base64,
    }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError {
            code: -32602,
            message: format!("missing or invalid `{key}`"),
        })?
        .trim()
        .to_string();
    if s.is_empty() {
        return Err(ToolError {
            code: -32602,
            message: format!("`{key}` cannot be empty"),
        });
    }
    Ok(s)
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(n as usize)).ok_or_else(|| ToolError {
            code: -32602,
            message: format!("`{key}` must be a positive integer"),
        }),
    }
}

fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| ToolError {
            code: -32602,
            message: format!("`{key}` must be a positive integer"),
        }),
    }
}

fn parse_headers(args: &Value) -> Result<Vec<(String, String)>, ToolError> {
    let Some(obj) = args.get("headers") else {
        return Ok(Vec::new());
    };
    if obj.is_null() {
        return Ok(Vec::new());
    }
    let map = obj.as_object().ok_or_else(|| ToolError {
        code: -32602,
        message: "`headers` must be an object {string: string}".into(),
    })?;
    let mut out = Vec::with_capacity(map.len());
    for (k, v) in map {
        let vs = v.as_str().ok_or_else(|| ToolError {
            code: -32602,
            message: format!("header `{k}` must be a string value"),
        })?;
        out.push((k.clone(), vs.to_string()));
    }
    Ok(out)
}
