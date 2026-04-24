use std::path::Path;

use serde_json::{json, Value};

use crate::client::{
    self, ResponseFormat, TranscribeParams, Transcript, WhisperError, CLIENT_VERSION,
};

const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024; // OpenAI hard limit

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns whisper extension status, endpoint, model, and key presence",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "transcribe_file",
            "description": "Transcribe a local audio file via OpenAI-compatible /audio/transcriptions",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute or relative path to an audio file (≤ 25 MB)" },
                    "model": { "type": "string", "description": "Override default model (e.g. whisper-1, whisper-large-v3)" },
                    "language": { "type": "string", "description": "ISO 639-1 source language hint (e.g. 'en', 'es')" },
                    "prompt": { "type": "string", "description": "Optional prompt to bias decoding (vocabulary, style)" },
                    "response_format": { "type": "string", "enum": ["text", "json", "verbose_json", "srt", "vtt"], "description": "default text" },
                    "temperature": { "type": "number", "minimum": 0, "maximum": 1 }
                },
                "required": ["file_path"],
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

impl From<WhisperError> for ToolError {
    fn from(e: WhisperError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "transcribe_file" => transcribe_file(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "openai-whisper-compatible",
        "endpoint": client::base_url(),
        "default_model": client::default_model(),
        "client_version": CLIENT_VERSION,
        "user_agent": client::user_agent(),
        "tools": ["status", "transcribe_file"],
        "token_present": client::token_present(),
        "limits": {
            "max_file_bytes": MAX_FILE_BYTES
        }
    })
}

fn transcribe_file(args: &Value) -> Result<Value, ToolError> {
    let file_path_raw = required_string(args, "file_path")?;
    let path = Path::new(&file_path_raw);
    let meta = std::fs::metadata(path).map_err(|e| ToolError {
        code: -32602,
        message: format!("cannot stat `{file_path_raw}`: {e}"),
    })?;
    if !meta.is_file() {
        return Err(ToolError {
            code: -32602,
            message: format!("`{file_path_raw}` is not a regular file"),
        });
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ToolError {
            code: -32602,
            message: format!(
                "file exceeds {MAX_FILE_BYTES} bytes ({} bytes); split or compress first",
                meta.len()
            ),
        });
    }
    let bytes = std::fs::read(path).map_err(|e| ToolError {
        code: -32602,
        message: format!("cannot read `{file_path_raw}`: {e}"),
    })?;

    let model = optional_string(args, "model")?.unwrap_or_else(client::default_model);
    let language = optional_string(args, "language")?;
    let prompt = optional_string(args, "prompt")?;
    let format = parse_format(args)?;
    let temperature = optional_f64(args, "temperature")?;
    if let Some(t) = temperature {
        if !(0.0..=1.0).contains(&t) {
            return Err(ToolError {
                code: -32602,
                message: "`temperature` must be in [0, 1]".into(),
            });
        }
    }

    let transcript = client::transcribe(TranscribeParams {
        file_path: path,
        file_bytes: bytes,
        model: model.clone(),
        language: language.as_deref(),
        prompt: prompt.as_deref(),
        format,
        temperature,
    })?;

    let payload = match transcript {
        Transcript::Plain(s) => json!({ "text": s }),
        Transcript::Json(v) => v,
    };

    Ok(json!({
        "file_path": file_path_raw,
        "bytes": meta.len(),
        "model": model,
        "language": language,
        "response_format": format.label(),
        "transcript": payload,
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

fn optional_f64(args: &Value, key: &str) -> Result<Option<f64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| ToolError {
            code: -32602,
            message: format!("`{key}` must be a number"),
        }),
    }
}

fn parse_format(args: &Value) -> Result<ResponseFormat, ToolError> {
    match args.get("response_format").and_then(|v| v.as_str()) {
        None => Ok(ResponseFormat::Text),
        Some("text") => Ok(ResponseFormat::Text),
        Some("json") => Ok(ResponseFormat::Json),
        Some("verbose_json") => Ok(ResponseFormat::VerboseJson),
        Some("srt") => Ok(ResponseFormat::Srt),
        Some("vtt") => Ok(ResponseFormat::Vtt),
        Some(other) => Err(ToolError {
            code: -32602,
            message: format!(
                "`response_format` must be text|json|verbose_json|srt|vtt, got `{other}`"
            ),
        }),
    }
}
