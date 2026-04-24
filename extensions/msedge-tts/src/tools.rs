//! Tool definitions and dispatch for the Edge TTS extension.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use msedge_tts::{
    tts::{client::connect, SpeechConfig},
    voice::get_voices_list,
};
use serde_json::{json, Value};

const DEFAULT_VOICE: &str = "en-US-EmmaMultilingualNeural";
const DEFAULT_AUDIO_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";
const DEFAULT_RATE: i32 = 0;
const DEFAULT_PITCH: i32 = 0;
const DEFAULT_VOLUME: i32 = 0;
const DEFAULT_LIST_LIMIT: usize = 25;
const MAX_LIST_LIMIT: usize = 200;

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns Edge TTS extension defaults and output directory information",
            "input_schema": {
                "type": "object",
                "additionalProperties": false
            }
        },
        {
            "name": "list_voices",
            "description": "Lists available Microsoft Edge voices, optionally filtered by a text query",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional case-insensitive substring filter applied to each serialized voice entry"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "description": "Maximum number of voice entries to return"
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "synthesize",
            "description": "Synthesizes speech with Microsoft Edge Read Aloud and writes it to a local audio file",
            "input_schema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to synthesize"
                    },
                    "voice": {
                        "type": "string",
                        "description": "Edge voice name such as en-US-EmmaMultilingualNeural"
                    },
                    "audio_format": {
                        "type": "string",
                        "description": "Provider-native output format such as audio-24khz-48kbitrate-mono-mp3"
                    },
                    "rate": {
                        "type": "integer",
                        "description": "Provider-native speaking rate adjustment"
                    },
                    "pitch": {
                        "type": "integer",
                        "description": "Provider-native pitch adjustment"
                    },
                    "volume": {
                        "type": "integer",
                        "description": "Provider-native volume adjustment"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "Optional file path for the generated audio"
                    }
                },
                "required": ["text"],
                "additionalProperties": false
            }
        }
    ])
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "status" => tool_status(),
        "list_voices" => tool_list_voices(args),
        "synthesize" => tool_synthesize(args),
        other => Err(format!("unknown tool: {other}")),
    }
}

fn tool_status() -> Result<Value, String> {
    Ok(json!({
        "provider": "msedge",
        "crate": "msedge_tts",
        "default_voice": DEFAULT_VOICE,
        "default_audio_format": DEFAULT_AUDIO_FORMAT,
        "default_output_dir": default_output_dir()?.display().to_string(),
    }))
}

fn tool_list_voices(args: &Value) -> Result<Value, String> {
    let query = optional_string(args, "query")?;
    let limit = optional_u64(args, "limit")?
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);

    let voices = get_voices_list().map_err(|e| format!("list voices failed: {e}"))?;
    let mut filtered = Vec::new();

    for voice in voices {
        let value =
            serde_json::to_value(&voice).map_err(|e| format!("serialize voice failed: {e}"))?;
        if matches_query(&value, query.as_deref()) {
            filtered.push(value);
        }
    }

    let total = filtered.len();
    let voices = filtered.into_iter().take(limit).collect::<Vec<_>>();

    Ok(json!({
        "voices": voices,
        "returned": voices.len(),
        "matched": total,
        "query": query,
    }))
}

fn tool_synthesize(args: &Value) -> Result<Value, String> {
    let text = required_string(args, "text")?;
    if text.trim().is_empty() {
        return Err("`text` must not be empty".to_string());
    }

    let voice = optional_string(args, "voice")?.unwrap_or_else(|| DEFAULT_VOICE.to_string());
    let audio_format =
        optional_string(args, "audio_format")?.unwrap_or_else(|| DEFAULT_AUDIO_FORMAT.to_string());
    let rate = optional_i64(args, "rate")?.unwrap_or(DEFAULT_RATE as i64) as i32;
    let pitch = optional_i64(args, "pitch")?.unwrap_or(DEFAULT_PITCH as i64) as i32;
    let volume = optional_i64(args, "volume")?.unwrap_or(DEFAULT_VOLUME as i64) as i32;

    let output_path = match optional_string(args, "output_path")? {
        Some(path) => PathBuf::from(path),
        None => default_output_path(&audio_format)?,
    };
    ensure_parent_dir(&output_path)?;

    let config = SpeechConfig {
        voice_name: voice.clone(),
        audio_format: audio_format.clone(),
        pitch,
        rate,
        volume,
    };

    let mut client = connect().map_err(|e| format!("connect failed: {e}"))?;
    let synthesized = client
        .synthesize(text.trim(), &config)
        .map_err(|e| format!("synthesize failed: {e}"))?;

    fs::write(&output_path, &synthesized.audio_bytes)
        .map_err(|e| format!("failed to write audio file: {e}"))?;

    Ok(json!({
        "output_path": output_path.display().to_string(),
        "voice": voice,
        "audio_format": synthesized.audio_format,
        "bytes": synthesized.audio_bytes.len(),
        "metadata_count": synthesized.audio_metadata.len(),
        "rate": rate,
        "pitch": pitch,
        "volume": volume,
    }))
}

fn default_output_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("agent-msedge-tts");
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create output dir: {e}"))?;
    Ok(dir)
}

fn default_output_path(audio_format: &str) -> Result<PathBuf, String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock error: {e}"))?
        .as_millis();
    Ok(default_output_dir()?.join(format!("speech-{ts}{}", infer_extension(audio_format))))
}

fn ensure_parent_dir(path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create parent dir: {e}"))?;
    }
    Ok(())
}

fn infer_extension(audio_format: &str) -> &'static str {
    let normalized = audio_format.to_ascii_lowercase();
    if normalized.contains("webm") {
        ".webm"
    } else if normalized.contains("ogg") {
        ".ogg"
    } else if normalized.contains("opus") {
        ".opus"
    } else if normalized.contains("wav")
        || normalized.contains("riff")
        || normalized.contains("pcm")
    {
        ".wav"
    } else if normalized.contains("amr") {
        ".amr"
    } else {
        ".mp3"
    }
}

fn matches_query(value: &Value, query: Option<&str>) -> bool {
    let Some(query) = query else {
        return true;
    };
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return true;
    }
    serde_json::to_string(value)
        .map(|text| text.to_ascii_lowercase().contains(&needle))
        .unwrap_or(false)
}

fn required_string(args: &Value, key: &str) -> Result<String, String> {
    optional_string(args, key)?.ok_or_else(|| format!("missing or non-string `{key}`"))
}

fn optional_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn optional_i64(args: &Value, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an integer")),
    }
}

fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a positive integer")),
    }
}
