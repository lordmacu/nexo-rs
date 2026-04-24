//! ffmpeg subprocess runner shared between snapshot (single frame) and clip
//! (short recording). Same watchdog pattern as extensions/video-frames.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 60;
pub const MAX_TIMEOUT_SECS: u64 = 600;

#[derive(Debug)]
pub enum FfError {
    MissingBinary,
    SpawnFailed(String),
    NonZeroExit { code: Option<i32>, stderr: String },
    Timeout(u64),
    BadUrl(String),
    IoError(String),
}

impl FfError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            FfError::MissingBinary => -32030,
            FfError::SpawnFailed(_) => -32031,
            FfError::NonZeroExit { .. } => -32032,
            FfError::Timeout(_) => -32033,
            FfError::BadUrl(_) => -32602,
            FfError::IoError(_) => -32034,
        }
    }
    pub fn message(&self) -> String {
        match self {
            FfError::MissingBinary => "`ffmpeg` not found on PATH".into(),
            FfError::SpawnFailed(s) => format!("spawn failed: {s}"),
            FfError::NonZeroExit { code, stderr } => match code {
                Some(c) => format!("ffmpeg exited {c}: {}", truncate(stderr, 400)),
                None => format!("ffmpeg killed by signal: {}", truncate(stderr, 400)),
            },
            FfError::Timeout(s) => format!("ffmpeg exceeded {s}s and was killed"),
            FfError::BadUrl(m) => m.clone(),
            FfError::IoError(s) => format!("io: {s}"),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

pub fn timeout_from_env() -> u64 {
    std::env::var("RTSP_SNAPSHOT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| v.min(MAX_TIMEOUT_SECS))
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

pub fn bin_path() -> Result<PathBuf, FfError> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        let cand = dir.join("ffmpeg");
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(FfError::MissingBinary)
}

pub fn sandbox_root() -> Result<PathBuf, FfError> {
    let root = match std::env::var("RTSP_SNAPSHOT_OUTPUT_ROOT") {
        Ok(r) if !r.trim().is_empty() => PathBuf::from(r),
        _ => std::env::temp_dir(),
    };
    std::fs::create_dir_all(&root).map_err(|e| FfError::IoError(e.to_string()))?;
    root.canonicalize().map_err(|e| FfError::IoError(e.to_string()))
}

/// Ensure `requested` sits under the sandbox root. Creates the parent if missing.
pub fn sandbox_output_path(requested: &Path) -> Result<PathBuf, FfError> {
    let parent = requested
        .parent()
        .ok_or_else(|| FfError::IoError(format!("output `{}` has no parent", requested.display())))?;
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| FfError::IoError(e.to_string()))?;
    }
    let parent_abs = parent
        .canonicalize()
        .map_err(|e| FfError::IoError(e.to_string()))?;
    let root = sandbox_root()?;
    if !parent_abs.starts_with(&root) {
        return Err(FfError::IoError(format!(
            "output `{}` is outside sandbox `{}`",
            requested.display(),
            root.display()
        )));
    }
    let filename = requested
        .file_name()
        .ok_or_else(|| FfError::IoError("output has no filename".into()))?;
    Ok(parent_abs.join(filename))
}

pub fn run(args: &[&str], timeout_secs: u64) -> Result<String, FfError> {
    let bin = bin_path()?;
    let child = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FfError::SpawnFailed(e.to_string()))?;

    let (tx, rx) = mpsc::channel::<()>();
    let child_id = child.id();
    let timeout = Duration::from_secs(timeout_secs);
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            #[cfg(unix)]
            unsafe { kill(child_id as i32, 9); }
            return true;
        }
        false
    });

    let output = child.wait_with_output().map_err(|e| FfError::IoError(e.to_string()))?;
    let _ = tx.send(());
    let killed = watchdog.join().unwrap_or(false);

    if killed && !output.status.success() {
        return Err(FfError::Timeout(timeout_secs));
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(FfError::NonZeroExit { code: output.status.code(), stderr });
    }
    Ok(stderr) // ffmpeg writes status to stderr; useful to return for debugging
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Validate a URL destined for ffmpeg `-i`. Accepts rtsp://, rtsps://, http://,
/// https:// only. Prevents ffmpeg's `concat:` / `file:` gadgets (local file
/// read via URL trick) by rejecting anything else.
pub fn validate_stream_url(url_raw: &str) -> Result<(), FfError> {
    let parsed = url::Url::parse(url_raw)
        .map_err(|e| FfError::BadUrl(format!("bad url: {e}")))?;
    match parsed.scheme() {
        "rtsp" | "rtsps" | "http" | "https" => Ok(()),
        other => Err(FfError::BadUrl(format!(
            "scheme `{other}` not allowed; use rtsp/rtsps/http/https"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_rtsp() {
        assert!(validate_stream_url("rtsp://user:pass@cam.local/stream").is_ok());
        assert!(validate_stream_url("rtsps://cam.local/s").is_ok());
        assert!(validate_stream_url("http://cam.local/live.m3u8").is_ok());
    }

    #[test]
    fn validate_rejects_file_scheme() {
        assert!(validate_stream_url("file:///etc/passwd").is_err());
        assert!(validate_stream_url("concat:/etc/passwd").is_err());
    }

    #[test]
    fn validate_rejects_garbage() {
        assert!(validate_stream_url("not a url").is_err());
        assert!(validate_stream_url("").is_err());
    }

    #[test]
    fn sandbox_rejects_outside_root() {
        let tmp = tempfile::tempdir().unwrap().into_path();
        std::env::set_var("RTSP_SNAPSHOT_OUTPUT_ROOT", &tmp);
        let outside = std::env::temp_dir().join(format!("outside-{}.jpg", std::process::id()));
        let err = sandbox_output_path(&outside).unwrap_err();
        assert!(matches!(err, FfError::IoError(_)));
    }

    #[test]
    fn sandbox_accepts_inside_root() {
        let tmp = tempfile::tempdir().unwrap().into_path();
        std::env::set_var("RTSP_SNAPSHOT_OUTPUT_ROOT", &tmp);
        let inside = tmp.join("cam.jpg");
        let ok = sandbox_output_path(&inside).unwrap();
        assert!(ok.starts_with(&tmp));
    }
}
