use std::path::Path;

use serde_json::{json, Value};

use crate::client::{self, Length, SumError, SummarizeParams, CLIENT_VERSION};

const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_TEXT_CHARS: usize = 60_000;

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns summarize extension status, endpoint, model, and key presence",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "summarize_text",
            "description": "Summarize plain text via OpenAI-compatible chat completions",
            "input_schema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Text content to summarize" },
                    "length": { "type": "string", "enum": ["short", "medium", "long"], "description": "default medium" },
                    "language": { "type": "string", "description": "Output language hint (e.g. 'Spanish')" }
                },
                "required": ["text"],
                "additionalProperties": false
            }
        },
        {
            "name": "summarize_file",
            "description": "Read a local UTF-8 file and summarize its contents (max 1 MB)",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or relative path to a UTF-8 text file" },
                    "length": { "type": "string", "enum": ["short", "medium", "long"] },
                    "language": { "type": "string" }
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

impl From<SumError> for ToolError {
    fn from(e: SumError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "summarize_text" => summarize_text(args),
        "summarize_file" => summarize_file(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "openai-compatible",
        "endpoint": client::base_url(),
        "model": client::model(),
        "client_version": CLIENT_VERSION,
        "user_agent": client::user_agent(),
        "tools": ["status", "summarize_text", "summarize_file"],
        "token_present": client::token_present(),
        "limits": {
            "max_file_bytes": MAX_FILE_BYTES,
            "max_text_chars": MAX_TEXT_CHARS,
        }
    })
}

fn summarize_text(args: &Value) -> Result<Value, ToolError> {
    let text = required_string(args, "text")?;
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(ToolError {
            code: -32602,
            message: format!("`text` exceeds {MAX_TEXT_CHARS} chars; chunk it before summarizing"),
        });
    }
    let length = parse_length(args)?;
    let language = optional_string(args, "language")?;

    let summary = client::summarize(SummarizeParams {
        text: &text,
        length,
        language: language.as_deref(),
    })?;

    Ok(json!({
        "length": length.label(),
        "language": language,
        "input_chars": text.chars().count(),
        "summary": summary,
    }))
}

fn summarize_file(args: &Value) -> Result<Value, ToolError> {
    let path_raw = required_string(args, "path")?;
    let path = Path::new(&path_raw);
    let meta = std::fs::metadata(path).map_err(|e| ToolError {
        code: -32602,
        message: format!("cannot stat `{path_raw}`: {e}"),
    })?;
    if !meta.is_file() {
        return Err(ToolError {
            code: -32602,
            message: format!("`{path_raw}` is not a regular file"),
        });
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ToolError {
            code: -32602,
            message: format!("file exceeds {MAX_FILE_BYTES} bytes ({} bytes)", meta.len()),
        });
    }
    let text = std::fs::read_to_string(path).map_err(|e| ToolError {
        code: -32602,
        message: format!("cannot read `{path_raw}` as UTF-8: {e}"),
    })?;
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(ToolError {
            code: -32602,
            message: format!("file content exceeds {MAX_TEXT_CHARS} chars"),
        });
    }

    let length = parse_length(args)?;
    let language = optional_string(args, "language")?;
    let summary = client::summarize(SummarizeParams {
        text: &text,
        length,
        language: language.as_deref(),
    })?;

    Ok(json!({
        "path": path_raw,
        "bytes": meta.len(),
        "length": length.label(),
        "language": language,
        "input_chars": text.chars().count(),
        "summary": summary,
    }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let value = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError {
            code: -32602,
            message: format!("missing or invalid `{key}`"),
        })?
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(ToolError {
            code: -32602,
            message: format!("`{key}` cannot be empty"),
        });
    }
    Ok(value)
}

fn optional_string(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.trim().to_string()))
            .ok_or_else(|| ToolError {
                code: -32602,
                message: format!("`{key}` must be a string"),
            }),
    }
}

fn parse_length(args: &Value) -> Result<Length, ToolError> {
    match args.get("length").and_then(|v| v.as_str()) {
        None => Ok(Length::Medium),
        Some("short") => Ok(Length::Short),
        Some("medium") => Ok(Length::Medium),
        Some("long") => Ok(Length::Long),
        Some(other) => Err(ToolError {
            code: -32602,
            message: format!("`length` must be short|medium|long, got `{other}`"),
        }),
    }
}
