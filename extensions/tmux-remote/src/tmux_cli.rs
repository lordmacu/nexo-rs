//! Thin wrapper over `tmux`. All invocations use the same socket
//! (`TMUX_REMOTE_SOCKET` env or default `$TMPDIR/agent-rs-tmux.sock`) so
//! the extension's sessions are isolated from any user-facing tmux server.

use std::path::PathBuf;
use std::process::{Command, Stdio};

pub const DEFAULT_TIMEOUT_SECS: u64 = 10;

#[derive(Debug)]
pub enum TmuxError {
    MissingBinary,
    SpawnFailed(String),
    NonZeroExit { code: Option<i32>, stderr: String },
    BadSession(String),
    IoError(String),
}

impl TmuxError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            TmuxError::MissingBinary => -32050,
            TmuxError::SpawnFailed(_) => -32031,
            TmuxError::NonZeroExit { .. } => -32051,
            TmuxError::BadSession(_) => -32602,
            TmuxError::IoError(_) => -32034,
        }
    }
    pub fn message(&self) -> String {
        match self {
            TmuxError::MissingBinary => "`tmux` binary not on PATH".into(),
            TmuxError::SpawnFailed(s) => format!("spawn failed: {s}"),
            TmuxError::NonZeroExit { code, stderr } => match code {
                Some(c) => format!("tmux exited {c}: {}", truncate(stderr, 300)),
                None => format!("tmux killed by signal: {}", truncate(stderr, 300)),
            },
            TmuxError::BadSession(s) => format!("invalid session name `{s}`"),
            TmuxError::IoError(s) => format!("io error: {s}"),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

pub fn bin_path() -> Result<PathBuf, TmuxError> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        let cand = dir.join("tmux");
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(TmuxError::MissingBinary)
}

pub fn socket_path() -> PathBuf {
    if let Ok(s) = std::env::var("TMUX_REMOTE_SOCKET") {
        if !s.trim().is_empty() {
            return PathBuf::from(s);
        }
    }
    std::env::temp_dir().join("agent-rs-tmux.sock")
}

/// Validate session names: letters/digits/dash/underscore only, 1..=64 chars.
/// Prevents shell injection via tmux `-t` argument.
pub fn validate_session_name(name: &str) -> Result<(), TmuxError> {
    if name.is_empty() || name.len() > 64 {
        return Err(TmuxError::BadSession(name.to_string()));
    }
    for c in name.chars() {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(TmuxError::BadSession(name.to_string()));
        }
    }
    Ok(())
}

pub fn run(args: &[&str]) -> Result<String, TmuxError> {
    let bin = bin_path()?;
    let socket = socket_path();
    let socket_s = socket.to_string_lossy().to_string();
    let mut full: Vec<&str> = vec!["-S", &socket_s];
    full.extend_from_slice(args);

    let output = Command::new(&bin)
        .args(&full)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| TmuxError::SpawnFailed(e.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(TmuxError::NonZeroExit {
            code: output.status.code(),
            stderr,
        });
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn name_validator_accepts_reasonable() {
        assert!(validate_session_name("build-123").is_ok());
        assert!(validate_session_name("dev_server").is_ok());
    }
    #[test]
    fn name_validator_rejects_unsafe() {
        assert!(validate_session_name("").is_err());
        assert!(validate_session_name("a;b").is_err());
        assert!(validate_session_name("bad name").is_err());
        assert!(validate_session_name("`inj`").is_err());
        assert!(validate_session_name(&"x".repeat(65)).is_err());
    }
    #[test]
    fn socket_path_respects_env() {
        std::env::set_var("TMUX_REMOTE_SOCKET", "/tmp/test-override.sock");
        assert_eq!(socket_path().to_str().unwrap(), "/tmp/test-override.sock");
        std::env::remove_var("TMUX_REMOTE_SOCKET");
    }
}
