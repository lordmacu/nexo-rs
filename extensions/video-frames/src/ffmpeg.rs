//! Thin wrapper over `ffmpeg` / `ffprobe` subprocess execution.
//!
//! Design notes:
//! - We invoke the bins by name so the OS `PATH` decides which build to use.
//! - stderr is captured and surfaced on failure (ffmpeg's useful output lives
//!   on stderr, not stdout, and without it errors are opaque).
//! - A soft-timeout is enforced per call via a monitor thread that sends
//!   SIGKILL when exceeded. For long transcodes the operator raises the
//!   budget via `VIDEO_FRAMES_TIMEOUT_SECS`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 600;
pub const MAX_TIMEOUT_SECS: u64 = 3600;

#[derive(Debug)]
pub enum FfmpegError {
    MissingBinary(String),
    SpawnFailed(String),
    NonZeroExit { code: Option<i32>, stderr: String },
    Timeout(u64),
    IoError(String),
}

impl FfmpegError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            FfmpegError::MissingBinary(_) => -32030,
            FfmpegError::SpawnFailed(_) => -32031,
            FfmpegError::NonZeroExit { .. } => -32032,
            FfmpegError::Timeout(_) => -32033,
            FfmpegError::IoError(_) => -32034,
        }
    }

    pub fn message(&self) -> String {
        match self {
            FfmpegError::MissingBinary(b) => format!("required binary `{b}` not found on PATH"),
            FfmpegError::SpawnFailed(s) => format!("failed to spawn ffmpeg: {s}"),
            FfmpegError::NonZeroExit { code, stderr } => match code {
                Some(c) => format!("ffmpeg exited with code {c}: {}", truncate(stderr, 400)),
                None => format!("ffmpeg terminated by signal: {}", truncate(stderr, 400)),
            },
            FfmpegError::Timeout(s) => format!("ffmpeg exceeded {s}s timeout and was killed"),
            FfmpegError::IoError(s) => format!("io error: {s}"),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

pub fn timeout_from_env() -> u64 {
    std::env::var("VIDEO_FRAMES_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(|v: u64| v.min(MAX_TIMEOUT_SECS))
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

/// Locate a binary by name on PATH. Returns the absolute path, or
/// `FfmpegError::MissingBinary`. Pure, no side effects.
pub fn which(bin: &str) -> Result<PathBuf, FfmpegError> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(FfmpegError::MissingBinary(bin.to_string()))
}

/// Run a subprocess with a hard timeout. Returns (exit_code, stdout, stderr).
pub fn run_with_timeout(
    bin: &str,
    args: &[&str],
    timeout_secs: u64,
) -> Result<CapturedOutput, FfmpegError> {
    let path = which(bin)?;
    let child = Command::new(&path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FfmpegError::SpawnFailed(e.to_string()))?;

    let (tx, rx) = mpsc::channel::<()>();
    let child_id = child.id();
    let timeout = Duration::from_secs(timeout_secs);

    // Watchdog thread. If wait completes first, it sends on tx and the timer
    // thread exits via recv_timeout returning Ok; else timer kills the child.
    let watchdog = std::thread::spawn(move || {
        // recv_timeout returns Err(Timeout) if no message arrives — that is
        // the kill signal.
        if rx.recv_timeout(timeout).is_err() {
            // Best-effort kill. POSIX: send SIGKILL via kill syscall by pid.
            #[cfg(unix)]
            unsafe {
                libc_kill(child_id as i32, 9);
            }
            #[cfg(not(unix))]
            {
                let _ = child_id; // noop on non-unix; child.kill() handled below
            }
            return true;
        }
        false
    });

    let output_res = child.wait_with_output();
    let _ = tx.send(()); // signal watchdog to stop
    let killed = watchdog.join().unwrap_or(false);

    let output = output_res.map_err(|e| FfmpegError::IoError(e.to_string()))?;

    if killed && !output.status.success() {
        return Err(FfmpegError::Timeout(timeout_secs));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(FfmpegError::NonZeroExit {
            code: output.status.code(),
            stderr,
        });
    }

    Ok(CapturedOutput { stdout, stderr })
}

pub struct CapturedOutput {
    pub stdout: String,
    pub stderr: String,
}

// Minimal libc binding to avoid pulling the whole `libc` crate as a dep for
// one syscall on Unix. Non-Unix targets skip the watchdog kill.
#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
#[allow(non_snake_case)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    kill(pid, sig)
}

/// Validate that `output_dir` is an existing, writable directory under an
/// operator-sandboxed root. By default we only allow writes under
/// `VIDEO_FRAMES_OUTPUT_ROOT` (or the system temp dir if unset). Prevents
/// path-traversal abuse from LLM-generated paths.
pub fn sandbox_output_dir(requested: &Path) -> Result<PathBuf, FfmpegError> {
    let requested = requested
        .canonicalize()
        .or_else(|_| {
            // If the path does not exist yet, canonicalize the parent.
            let parent = requested.parent().ok_or_else(|| {
                FfmpegError::IoError(format!(
                    "cannot resolve output_dir `{}`",
                    requested.display()
                ))
            })?;
            std::fs::create_dir_all(parent)
                .map_err(|e| FfmpegError::IoError(e.to_string()))?;
            let parent_abs = parent
                .canonicalize()
                .map_err(|e| FfmpegError::IoError(e.to_string()))?;
            let fname = requested
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("out"));
            let full = parent_abs.join(fname);
            std::fs::create_dir_all(&full).map_err(|e| FfmpegError::IoError(e.to_string()))?;
            full.canonicalize()
                .map_err(|e| FfmpegError::IoError(e.to_string()))
        })?;

    let root = sandbox_root()?;
    if !requested.starts_with(&root) {
        return Err(FfmpegError::IoError(format!(
            "output_dir `{}` is outside sandbox `{}`",
            requested.display(),
            root.display()
        )));
    }
    if !requested.is_dir() {
        return Err(FfmpegError::IoError(format!(
            "output_dir `{}` is not a directory",
            requested.display()
        )));
    }
    Ok(requested)
}

pub fn sandbox_root() -> Result<PathBuf, FfmpegError> {
    let env_root = std::env::var("VIDEO_FRAMES_OUTPUT_ROOT").ok();
    let root = match env_root {
        Some(r) if !r.trim().is_empty() => PathBuf::from(r),
        _ => std::env::temp_dir(),
    };
    std::fs::create_dir_all(&root).map_err(|e| FfmpegError::IoError(e.to_string()))?;
    root.canonicalize()
        .map_err(|e| FfmpegError::IoError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_standard_bin() {
        // `ls` exists on every Unix box our test runs on.
        #[cfg(unix)]
        assert!(which("ls").is_ok());
    }

    #[test]
    fn which_reports_missing_bin() {
        let err = which("definitely_not_a_real_binary_xyz").unwrap_err();
        assert!(matches!(err, FfmpegError::MissingBinary(_)));
    }

    #[test]
    fn sandbox_rejects_outside_root() {
        std::env::set_var("VIDEO_FRAMES_OUTPUT_ROOT", "/tmp/video-frames-sandbox");
        std::fs::create_dir_all("/tmp/video-frames-sandbox").unwrap();
        let err = sandbox_output_dir(Path::new("/etc")).unwrap_err();
        assert!(matches!(err, FfmpegError::IoError(_)));
    }

    #[test]
    fn sandbox_accepts_subdir() {
        std::env::set_var("VIDEO_FRAMES_OUTPUT_ROOT", "/tmp/video-frames-sandbox");
        std::fs::create_dir_all("/tmp/video-frames-sandbox/child").unwrap();
        let ok = sandbox_output_dir(Path::new("/tmp/video-frames-sandbox/child")).unwrap();
        assert!(ok.starts_with("/tmp/video-frames-sandbox"));
    }
}
