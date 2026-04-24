use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::ffmpeg::{
    self, timeout_from_env, FfmpegError, DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS,
};

pub const CLIENT_VERSION: &str = "video-frames-0.1.0";
const MAX_INPUT_BYTES: u64 = 500 * 1024 * 1024; // 500 MB
const MAX_FRAMES: u32 = 1000;
const DEFAULT_FRAME_COUNT: u32 = 10;
const DEFAULT_AUDIO_CODEC: &str = "mp3";

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns ffmpeg/ffprobe versions, sandbox root, and limits.",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "probe",
            "description": "Runs `ffprobe` on a local video file and returns duration, streams, and format metadata as JSON.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to a local video file (≤ 500 MB)" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "extract_frames",
            "description": "Sample N evenly-spaced frames from a video as JPG files written to `output_dir`. Output path must lie under the sandbox root (VIDEO_FRAMES_OUTPUT_ROOT, default system temp). Returns the list of written file paths.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path":        { "type": "string" },
                    "output_dir":  { "type": "string" },
                    "count":       { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Number of frames to extract (default 10)" },
                    "fps":         { "type": "number",  "minimum": 0.01, "maximum": 60, "description": "Override evenly-spaced sampling with a fixed fps. When set, `count` is ignored." },
                    "width":       { "type": "integer", "minimum": 16, "maximum": 4096, "description": "Resize frames to this width (keeps aspect ratio)" }
                },
                "required": ["path", "output_dir"],
                "additionalProperties": false
            }
        },
        {
            "name": "extract_audio",
            "description": "Extract the audio track to a single file under the sandbox. Default codec `mp3`; also supports `wav` for direct feeding into openai-whisper transcription.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path":        { "type": "string" },
                    "output_path": { "type": "string", "description": "Full file path for the output audio file (under sandbox root)" },
                    "codec":       { "type": "string", "enum": ["mp3", "wav"], "description": "Default mp3" },
                    "mono":        { "type": "boolean", "description": "Downmix to mono (smaller, better for whisper). Default true." },
                    "sample_rate": { "type": "integer", "minimum": 8000, "maximum": 48000, "description": "Target sample rate (default 16000 — matches whisper)" }
                },
                "required": ["path", "output_path"],
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

impl From<FfmpegError> for ToolError {
    fn from(e: FfmpegError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

fn bad_input(msg: impl Into<String>) -> ToolError {
    ToolError {
        code: -32602,
        message: msg.into(),
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => status(),
        "probe" => probe(args),
        "extract_frames" => extract_frames(args),
        "extract_audio" => extract_audio(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Result<Value, ToolError> {
    let ffmpeg_version = ffmpeg::run_with_timeout("ffmpeg", &["-version"], 10)
        .map(|o| first_line(&o.stdout))
        .unwrap_or_else(|_| "unavailable".to_string());
    let ffprobe_version = ffmpeg::run_with_timeout("ffprobe", &["-version"], 10)
        .map(|o| first_line(&o.stdout))
        .unwrap_or_else(|_| "unavailable".to_string());
    let sandbox = ffmpeg::sandbox_root().map(|p| p.display().to_string());
    Ok(json!({
        "ok": ffmpeg_version != "unavailable" && ffprobe_version != "unavailable",
        "provider": "ffmpeg (subprocess)",
        "client_version": CLIENT_VERSION,
        "tools": ["status", "probe", "extract_frames", "extract_audio"],
        "ffmpeg_version": ffmpeg_version,
        "ffprobe_version": ffprobe_version,
        "sandbox_root": sandbox.unwrap_or_else(|e| format!("error: {e:?}")),
        "limits": {
            "max_input_bytes": MAX_INPUT_BYTES,
            "max_frames": MAX_FRAMES,
            "default_timeout_secs": DEFAULT_TIMEOUT_SECS,
            "max_timeout_secs": MAX_TIMEOUT_SECS,
        },
        "requires": { "bins": ["ffmpeg", "ffprobe"], "env": [] }
    }))
}

fn probe(args: &Value) -> Result<Value, ToolError> {
    let path = check_input_file(args)?;
    let path_s = path.to_string_lossy().into_owned();
    let out = ffmpeg::run_with_timeout(
        "ffprobe",
        &[
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            &path_s,
        ],
        timeout_from_env(),
    )?;
    let parsed: Value = serde_json::from_str(&out.stdout)
        .map_err(|e| bad_input(format!("ffprobe output was not valid JSON: {e}")))?;
    let duration = parsed
        .pointer("/format/duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok());
    Ok(json!({
        "path": path_s,
        "duration_secs": duration,
        "format": parsed.get("format"),
        "streams": parsed.get("streams"),
    }))
}

fn extract_frames(args: &Value) -> Result<Value, ToolError> {
    let path = check_input_file(args)?;
    let output_dir = resolve_output_dir(args)?;
    let count = optional_u32(args, "count")?.unwrap_or(DEFAULT_FRAME_COUNT);
    if count == 0 || count > MAX_FRAMES {
        return Err(bad_input(format!(
            "`count` must be in 1..={MAX_FRAMES}"
        )));
    }
    let fps = optional_f64(args, "fps")?;
    let width = optional_u32(args, "width")?;

    // Determine the filter chain. With explicit `fps`, use `fps=N`. Otherwise
    // evenly sample `count` frames by first probing duration.
    let mut vf_parts: Vec<String> = Vec::new();
    match fps {
        Some(f) => {
            if !(0.01..=60.0).contains(&f) {
                return Err(bad_input("`fps` must be in 0.01..=60"));
            }
            vf_parts.push(format!("fps={f}"));
        }
        None => {
            let duration = probe_duration_secs(&path)?;
            // Evenly-spaced sampling over the full duration.
            // `-vf fps=count/duration` with nominal bound.
            let rate = (count as f64 / duration.max(0.5)).clamp(0.01, 30.0);
            vf_parts.push(format!("fps={rate}"));
        }
    }
    if let Some(w) = width {
        vf_parts.push(format!("scale={w}:-2"));
    }
    let vf = vf_parts.join(",");

    let pattern = output_dir.join("frame-%04d.jpg");
    let pattern_s = pattern.to_string_lossy().into_owned();
    let path_s = path.to_string_lossy().into_owned();
    let frames_s = count.to_string();

    let mut args_ff: Vec<&str> = vec![
        "-y",
        "-i",
        &path_s,
        "-vf",
        vf.as_str(),
        "-vframes",
        frames_s.as_str(),
        "-q:v",
        "2",
        pattern_s.as_str(),
    ];
    if fps.is_some() {
        // With explicit fps we may not want -vframes; keep the cap as a
        // safety net to avoid disk blow-up.
        let _ = &mut args_ff;
    }
    ffmpeg::run_with_timeout("ffmpeg", &args_ff, timeout_from_env())?;

    let written = list_frames(&output_dir, "frame-", ".jpg");
    Ok(json!({
        "path": path_s,
        "output_dir": output_dir.display().to_string(),
        "fps": fps,
        "count_requested": count,
        "count_written": written.len(),
        "frames": written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    }))
}

fn extract_audio(args: &Value) -> Result<Value, ToolError> {
    let path = check_input_file(args)?;
    let output_path = resolve_output_path(args)?;
    let codec = optional_string(args, "codec").unwrap_or_else(|| DEFAULT_AUDIO_CODEC.to_string());
    if !["mp3", "wav"].contains(&codec.as_str()) {
        return Err(bad_input(format!("`codec` must be mp3|wav, got `{codec}`")));
    }
    let mono = args
        .get("mono")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let sample_rate = optional_u32(args, "sample_rate")?.unwrap_or(16_000);
    if !(8_000..=48_000).contains(&sample_rate) {
        return Err(bad_input("`sample_rate` must be in 8000..=48000"));
    }

    let path_s = path.to_string_lossy().into_owned();
    let output_s = output_path.to_string_lossy().into_owned();
    let sample_s = sample_rate.to_string();
    let channels = if mono { "1" } else { "2" };

    let codec_args: &[&str] = match codec.as_str() {
        "mp3" => &["-codec:a", "libmp3lame", "-qscale:a", "4"],
        "wav" => &["-codec:a", "pcm_s16le"],
        _ => unreachable!(),
    };

    let mut args_ff: Vec<&str> = vec!["-y", "-i", &path_s, "-vn", "-ac", channels, "-ar", &sample_s];
    args_ff.extend_from_slice(codec_args);
    args_ff.push(&output_s);

    ffmpeg::run_with_timeout("ffmpeg", &args_ff, timeout_from_env())?;

    let size = std::fs::metadata(&output_path)
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(json!({
        "input": path_s,
        "output_path": output_s,
        "codec": codec,
        "mono": mono,
        "sample_rate": sample_rate,
        "bytes": size,
    }))
}

fn probe_duration_secs(path: &Path) -> Result<f64, ToolError> {
    let path_s = path.to_string_lossy().into_owned();
    let out = ffmpeg::run_with_timeout(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            &path_s,
        ],
        30,
    )?;
    out.stdout
        .trim()
        .parse::<f64>()
        .map_err(|e| bad_input(format!("cannot parse duration from ffprobe: {e}")))
}

fn check_input_file(args: &Value) -> Result<PathBuf, ToolError> {
    let raw = required_string(args, "path")?;
    let path = PathBuf::from(&raw);
    let meta = std::fs::metadata(&path)
        .map_err(|e| bad_input(format!("cannot stat `{raw}`: {e}")))?;
    if !meta.is_file() {
        return Err(bad_input(format!("`{raw}` is not a regular file")));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(bad_input(format!(
            "input exceeds {MAX_INPUT_BYTES} bytes ({} bytes)",
            meta.len()
        )));
    }
    Ok(path.canonicalize().unwrap_or(path))
}

fn resolve_output_dir(args: &Value) -> Result<PathBuf, ToolError> {
    let raw = required_string(args, "output_dir")?;
    let p = PathBuf::from(&raw);
    if !p.exists() {
        std::fs::create_dir_all(&p)
            .map_err(|e| bad_input(format!("cannot create output_dir `{raw}`: {e}")))?;
    }
    ffmpeg::sandbox_output_dir(&p).map_err(Into::into)
}

fn resolve_output_path(args: &Value) -> Result<PathBuf, ToolError> {
    let raw = required_string(args, "output_path")?;
    let p = PathBuf::from(&raw);
    let parent = p.parent().ok_or_else(|| bad_input("output_path has no parent"))?;
    if !parent.exists() {
        std::fs::create_dir_all(parent)
            .map_err(|e| bad_input(format!("cannot create parent of `{raw}`: {e}")))?;
    }
    // Sandbox the parent dir.
    let parent_abs = ffmpeg::sandbox_output_dir(parent)?;
    let filename = p
        .file_name()
        .ok_or_else(|| bad_input("output_path is missing a filename"))?;
    Ok(parent_abs.join(filename))
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

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn optional_u32(args: &Value, key: &str) -> Result<Option<u32>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(n as u32))
            .ok_or_else(|| bad_input(format!("`{key}` must be a positive integer"))),
    }
}

fn optional_f64(args: &Value, key: &str) -> Result<Option<f64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| bad_input(format!("`{key}` must be a number"))),
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

fn list_frames(dir: &Path, prefix: &str, ext: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.starts_with(prefix) && n.ends_with(ext))
                        .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}
