use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "yt-dlp-0.1.0";

fn bin() -> String {
    std::env::var("YTDLP_BIN").unwrap_or_else(|_| "yt-dlp".into())
}

fn default_output_dir() -> PathBuf {
    if let Ok(d) = std::env::var("YTDLP_OUTPUT_DIR") {
        return PathBuf::from(d);
    }
    std::env::temp_dir().join("agent-yt-dlp")
}

fn allow_download() -> bool {
    std::env::var("YTDLP_ALLOW_DOWNLOAD").ok().as_deref() == Some("true")
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Binary + output dir info.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"info",
          "description":"Fetch metadata JSON for a URL without downloading. Returns title, duration, channel, formats summary.",
          "input_schema":{"type":"object","properties":{
              "url":{"type":"string"}
          },"required":["url"],"additionalProperties":false}},
        { "name":"formats","description":"List available formats for a URL.",
          "input_schema":{"type":"object","properties":{
              "url":{"type":"string"}
          },"required":["url"],"additionalProperties":false}},
        { "name":"download",
          "description":"Download a video or audio-only. Requires YTDLP_ALLOW_DOWNLOAD=true.",
          "input_schema":{"type":"object","properties":{
              "url":{"type":"string"},
              "mode":{"type":"string","enum":["video","audio"],"description":"default video"},
              "format":{"type":"string","description":"Override yt-dlp -f selector"},
              "output_dir":{"type":"string","description":"Override default output dir"},
              "audio_format":{"type":"string","description":"For mode=audio, e.g. mp3, m4a, opus; default mp3"}
          },"required":["url"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

fn bad_input(m: impl Into<String>) -> ToolError {
    ToolError {
        code: -32602,
        message: m.into(),
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "info" => info(args),
        "formats" => formats(args),
        "download" => download(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    let version = Command::new(bin())
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        });
    json!({
        "ok": version.is_some(),
        "provider": "yt-dlp",
        "client_version": CLIENT_VERSION,
        "bin": bin(),
        "bin_version": version,
        "output_dir": default_output_dir().display().to_string(),
        "allow_download": allow_download(),
        "write_gate": {
            "enabled": allow_download(),
            "env": "YTDLP_ALLOW_DOWNLOAD",
            "hint": "set YTDLP_ALLOW_DOWNLOAD=true to enable the `download` tool"
        },
        "tools": ["status","info","formats","download"],
        "requires": {"bins":["yt-dlp"],"env":[]}
    })
}

fn info(args: &Value) -> Result<Value, ToolError> {
    let url = required_url(args, "url")?;
    let out = Command::new(bin())
        .args(["-j", "--no-warnings", "--no-playlist", "--skip-download"])
        .arg(&url)
        .output()
        .map_err(|e| ToolError {
            code: -32003,
            message: format!("spawn failed: {e}"),
        })?;
    if !out.status.success() {
        return Err(ToolError {
            code: -32003,
            message: format!(
                "yt-dlp failed: {}",
                truncate(&String::from_utf8_lossy(&out.stderr), 400)
            ),
        });
    }
    let meta: Value = serde_json::from_slice(&out.stdout).map_err(|e| ToolError {
        code: -32006,
        message: format!("json parse: {e}"),
    })?;
    Ok(json!({
        "ok": true,
        "id": meta.get("id"),
        "title": meta.get("title"),
        "description": meta.get("description"),
        "uploader": meta.get("uploader"),
        "channel": meta.get("channel"),
        "duration": meta.get("duration"),
        "view_count": meta.get("view_count"),
        "upload_date": meta.get("upload_date"),
        "webpage_url": meta.get("webpage_url"),
        "thumbnail": meta.get("thumbnail"),
        "ext": meta.get("ext"),
    }))
}

fn formats(args: &Value) -> Result<Value, ToolError> {
    let url = required_url(args, "url")?;
    let out = Command::new(bin())
        .args(["-j", "--no-warnings", "--no-playlist", "--skip-download"])
        .arg(&url)
        .output()
        .map_err(|e| ToolError {
            code: -32003,
            message: format!("spawn failed: {e}"),
        })?;
    if !out.status.success() {
        return Err(ToolError {
            code: -32003,
            message: format!(
                "yt-dlp failed: {}",
                truncate(&String::from_utf8_lossy(&out.stderr), 400)
            ),
        });
    }
    let meta: Value = serde_json::from_slice(&out.stdout).unwrap_or(json!({}));
    let fmts: Vec<Value> = meta
        .get("formats")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|f| {
            json!({
                "format_id": f.get("format_id"),
                "ext": f.get("ext"),
                "resolution": f.get("resolution"),
                "fps": f.get("fps"),
                "vcodec": f.get("vcodec"),
                "acodec": f.get("acodec"),
                "filesize": f.get("filesize").or(f.get("filesize_approx")),
                "tbr": f.get("tbr"),
                "note": f.get("format_note"),
            })
        })
        .collect();
    Ok(json!({"ok": true, "title": meta.get("title"), "count": fmts.len(), "formats": fmts}))
}

fn download(args: &Value) -> Result<Value, ToolError> {
    if !allow_download() {
        return Err(ToolError {
            code: -32041,
            message: "YTDLP_ALLOW_DOWNLOAD not set to `true`".into(),
        });
    }
    let url = required_url(args, "url")?;
    let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("video");
    let out_dir = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(default_output_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| ToolError {
        code: -32003,
        message: format!("mkdir: {e}"),
    })?;
    let template = out_dir.join("%(title)s [%(id)s].%(ext)s");
    let template_str = template.to_string_lossy().to_string();

    let mut cmd = Command::new(bin());
    cmd.args([
        "--no-warnings",
        "--no-playlist",
        "--print",
        "after_move:filepath",
        "--no-progress",
        "-o",
        &template_str,
    ]);

    match mode {
        "audio" => {
            let af = args
                .get("audio_format")
                .and_then(|v| v.as_str())
                .unwrap_or("mp3");
            cmd.args(["-x", "--audio-format", af]);
        }
        "video" => {
            if let Some(f) = args.get("format").and_then(|v| v.as_str()) {
                cmd.args(["-f", f]);
            }
        }
        other => return Err(bad_input(format!("invalid mode `{other}`"))),
    }
    cmd.arg(&url);

    let output = cmd.output().map_err(|e| ToolError {
        code: -32003,
        message: format!("spawn failed: {e}"),
    })?;
    if !output.status.success() {
        return Err(ToolError {
            code: -32003,
            message: format!(
                "yt-dlp failed: {}",
                truncate(&String::from_utf8_lossy(&output.stderr), 400)
            ),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = stdout
        .lines()
        .last()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let size = std::fs::metadata(&path).map(|m| m.len()).ok();
    Ok(json!({
        "ok": true,
        "mode": mode,
        "url": url,
        "output_path": path,
        "output_dir": out_dir.display().to_string(),
        "bytes": size,
        "filename": Path::new(&path).file_name().and_then(|s| s.to_str()),
    }))
}

fn required_url(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim()
        .to_string();
    if !is_url(&s) {
        return Err(bad_input(format!("`{key}` must be http(s) URL")));
    }
    Ok(s)
}
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_exposes_write_gate_hint() {
        std::env::remove_var("YTDLP_ALLOW_DOWNLOAD");
        let out = dispatch("status", &json!({})).unwrap();
        assert_eq!(out["write_gate"]["enabled"], false);
        assert_eq!(out["write_gate"]["env"], "YTDLP_ALLOW_DOWNLOAD");
    }

    #[test]
    fn download_denied_when_gate_is_off() {
        std::env::remove_var("YTDLP_ALLOW_DOWNLOAD");
        let err = dispatch("download", &json!({"url":"https://example.com/video"})).unwrap_err();
        assert_eq!(err.code, -32041);
        assert!(err.message.contains("YTDLP_ALLOW_DOWNLOAD"));
    }
}
