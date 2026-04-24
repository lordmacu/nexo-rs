use std::path::Path;

use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "pdf-extract-0.1.0";
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
const DEFAULT_MAX_CHARS: usize = 200_000;

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns PDF extract extension info and limits.",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "extract_text",
            "description": "Extracts plain UTF-8 text from a local PDF file. Pure Rust, no external binaries.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or relative path to a PDF (≤ 25 MB)" },
                    "max_chars": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 1000000,
                        "description": "Truncate output at this char count. Default 200 000."
                    }
                },
                "required": ["path"],
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

fn bad_input(msg: impl Into<String>) -> ToolError {
    ToolError {
        code: -32602,
        message: msg.into(),
    }
}

fn provider_error(msg: impl Into<String>) -> ToolError {
    ToolError {
        code: -32006,
        message: msg.into(),
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "extract_text" => extract_text(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "pdf-extract (rust)",
        "client_version": CLIENT_VERSION,
        "tools": ["status", "extract_text"],
        "limits": {
            "max_file_bytes": MAX_FILE_BYTES,
            "default_max_chars": DEFAULT_MAX_CHARS,
        },
        "requires": { "bins": [], "env": [] }
    })
}

fn extract_text(args: &Value) -> Result<Value, ToolError> {
    let path_raw = required_string(args, "path")?;
    let path = Path::new(&path_raw);
    let meta = std::fs::metadata(path)
        .map_err(|e| bad_input(format!("cannot stat `{path_raw}`: {e}")))?;
    if !meta.is_file() {
        return Err(bad_input(format!("`{path_raw}` is not a regular file")));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(bad_input(format!(
            "file exceeds {MAX_FILE_BYTES} bytes ({} bytes); split or compress first",
            meta.len()
        )));
    }

    let max_chars = optional_usize(args, "max_chars")?.unwrap_or(DEFAULT_MAX_CHARS);
    if max_chars == 0 {
        return Err(bad_input("`max_chars` must be > 0"));
    }

    let full = pdf_extract::extract_text(path)
        .map_err(|e| provider_error(format!("pdf extraction failed: {e}")))?;
    let full_chars = full.chars().count();
    let (text, truncated) = if full_chars > max_chars {
        let slice: String = full.chars().take(max_chars).collect();
        (slice, true)
    } else {
        (full, false)
    };

    Ok(json!({
        "path": path_raw,
        "bytes": meta.len(),
        "max_chars": max_chars,
        "truncated": truncated,
        "char_count": text.chars().count(),
        "total_char_count": full_chars,
        "text": text,
    }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing or invalid `{key}`")))?
        .trim()
        .to_string();
    if s.is_empty() {
        return Err(bad_input(format!("`{key}` cannot be empty")));
    }
    Ok(s)
}

fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| bad_input(format!("`{key}` must be a positive integer"))),
    }
}
