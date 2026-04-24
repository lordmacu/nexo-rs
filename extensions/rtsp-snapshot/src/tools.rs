use std::path::PathBuf;

use serde_json::{json, Value};

use crate::ffmpeg::{self, timeout_from_env, FfError, MAX_TIMEOUT_SECS};

pub const CLIENT_VERSION: &str = "rtsp-snapshot-0.1.0";
const MAX_CLIP_SECS: u32 = 300;

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"ffmpeg bin + sandbox info.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"snapshot",
          "description":"Capture a single JPG frame from an RTSP/HTTP stream. Writes to `output_path` (must lie under RTSP_SNAPSHOT_OUTPUT_ROOT).",
          "input_schema":{
              "type":"object",
              "properties":{
                  "url":{"type":"string","description":"rtsp://, rtsps://, http://, or https://"},
                  "output_path":{"type":"string"},
                  "transport":{"type":"string","enum":["tcp","udp"],"description":"RTSP transport. Default tcp (more reliable over NAT)."},
                  "width":{"type":"integer","minimum":16,"maximum":4096}
              },
              "required":["url","output_path"],"additionalProperties":false
          }
        },
        { "name":"clip",
          "description":"Record a short clip (MP4) from an RTSP/HTTP stream. Max 300 seconds.",
          "input_schema":{
              "type":"object",
              "properties":{
                  "url":{"type":"string"},
                  "output_path":{"type":"string"},
                  "duration_secs":{"type":"integer","minimum":1,"maximum":MAX_CLIP_SECS},
                  "transport":{"type":"string","enum":["tcp","udp"]}
              },
              "required":["url","output_path","duration_secs"],"additionalProperties":false
          }
        }
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl From<FfError> for ToolError {
    fn from(e: FfError) -> Self {
        Self { code: e.rpc_code(), message: e.message() }
    }
}

fn bad_input(msg: impl Into<String>) -> ToolError {
    ToolError { code: -32602, message: msg.into() }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "snapshot" => snapshot(args),
        "clip" => clip(args),
        other => Err(ToolError { code: -32601, message: format!("unknown tool `{other}`") }),
    }
}

fn status() -> Value {
    let bin = ffmpeg::bin_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("missing: {}", e.message()));
    let sandbox = ffmpeg::sandbox_root().map(|p| p.display().to_string());
    json!({
        "ok": !bin.starts_with("missing"),
        "provider": "rtsp-snapshot (ffmpeg)",
        "client_version": CLIENT_VERSION,
        "tools": ["status","snapshot","clip"],
        "bin": bin,
        "sandbox_root": sandbox.unwrap_or_else(|e| format!("error: {e:?}")),
        "limits": {
            "max_clip_secs": MAX_CLIP_SECS,
            "max_timeout_secs": MAX_TIMEOUT_SECS,
        },
        "requires": {"bins":["ffmpeg"], "env":[]}
    })
}

fn snapshot(args: &Value) -> Result<Value, ToolError> {
    let url = required_string(args, "url")?;
    ffmpeg::validate_stream_url(&url)?;
    let output = resolve_output(args)?;
    let transport = optional_string(args, "transport").unwrap_or_else(|| "tcp".into());
    let width = optional_u32(args, "width")?;

    let mut ff_args: Vec<String> = Vec::new();
    ff_args.push("-y".into());
    // RTSP needs -rtsp_transport; harmless for http URLs (ffmpeg ignores it).
    if url.starts_with("rtsp") {
        ff_args.push("-rtsp_transport".into());
        ff_args.push(transport.clone());
    }
    ff_args.push("-i".into());
    ff_args.push(url.clone());
    ff_args.push("-frames:v".into());
    ff_args.push("1".into());
    ff_args.push("-q:v".into());
    ff_args.push("2".into());
    if let Some(w) = width {
        ff_args.push("-vf".into());
        ff_args.push(format!("scale={w}:-2"));
    }
    ff_args.push(output.to_string_lossy().into_owned());

    let arg_refs: Vec<&str> = ff_args.iter().map(String::as_str).collect();
    ffmpeg::run(&arg_refs, timeout_from_env())?;

    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "ok": true,
        "url": url,
        "output_path": output.display().to_string(),
        "bytes": size,
        "transport": transport,
    }))
}

fn clip(args: &Value) -> Result<Value, ToolError> {
    let url = required_string(args, "url")?;
    ffmpeg::validate_stream_url(&url)?;
    let output = resolve_output(args)?;
    let duration = optional_u32(args, "duration_secs")?
        .ok_or_else(|| bad_input("`duration_secs` required"))?;
    if duration == 0 || duration > MAX_CLIP_SECS {
        return Err(bad_input(format!("duration_secs must be 1..={MAX_CLIP_SECS}")));
    }
    let transport = optional_string(args, "transport").unwrap_or_else(|| "tcp".into());

    let mut ff_args: Vec<String> = Vec::new();
    ff_args.push("-y".into());
    if url.starts_with("rtsp") {
        ff_args.push("-rtsp_transport".into());
        ff_args.push(transport.clone());
    }
    ff_args.push("-i".into());
    ff_args.push(url.clone());
    ff_args.push("-t".into());
    ff_args.push(duration.to_string());
    ff_args.push("-c:v".into());
    ff_args.push("copy".into()); // stream copy — no re-encode unless needed
    ff_args.push("-c:a".into());
    ff_args.push("copy".into());
    ff_args.push(output.to_string_lossy().into_owned());

    let arg_refs: Vec<&str> = ff_args.iter().map(String::as_str).collect();
    // Give the subprocess duration + buffer; cap at MAX.
    let timeout = (duration as u64 + 30).min(MAX_TIMEOUT_SECS);
    ffmpeg::run(&arg_refs, timeout)?;

    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "ok": true,
        "url": url,
        "output_path": output.display().to_string(),
        "bytes": size,
        "duration_secs": duration,
        "transport": transport,
    }))
}

fn resolve_output(args: &Value) -> Result<PathBuf, ToolError> {
    let raw = required_string(args, "output_path")?;
    ffmpeg::sandbox_output_path(std::path::Path::new(&raw)).map_err(Into::into)
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing or invalid `{key}`")))?
        .trim().to_string();
    if s.is_empty() { return Err(bad_input(format!("`{key}` cannot be empty"))); }
    Ok(s)
}
fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
fn optional_u32(args: &Value, key: &str) -> Result<Option<u32>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(n as u32)).ok_or_else(|| bad_input(format!("`{key}` must be integer"))),
    }
}
