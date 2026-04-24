//! Thin wrapper over `docker` CLI. Container-name validation prevents shell
//! injection; outputs parsed as JSON where possible.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug)]
pub enum DockerError {
    MissingBinary,
    SpawnFailed(String),
    NonZeroExit { code: Option<i32>, stderr: String },
    Timeout(u64),
    BadName(String),
    IoError(String),
}

impl DockerError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            DockerError::MissingBinary => -32080,
            DockerError::SpawnFailed(_) => -32031,
            DockerError::NonZeroExit { .. } => -32081,
            DockerError::Timeout(_) => -32033,
            DockerError::BadName(_) => -32602,
            DockerError::IoError(_) => -32034,
        }
    }
    pub fn message(&self) -> String {
        match self {
            DockerError::MissingBinary => "`docker` not on PATH".into(),
            DockerError::SpawnFailed(s) => format!("spawn: {s}"),
            DockerError::NonZeroExit{code, stderr} => match code {
                Some(c) => format!("docker exited {c}: {}", truncate(stderr, 300)),
                None => format!("docker killed by signal: {}", truncate(stderr, 300)),
            },
            DockerError::Timeout(s) => format!("docker timeout {s}s"),
            DockerError::BadName(s) => format!("invalid container name/id `{s}`"),
            DockerError::IoError(s) => format!("io: {s}"),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

pub fn bin_path() -> Result<PathBuf, DockerError> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        let cand = dir.join("docker");
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(DockerError::MissingBinary)
}

/// Docker container names/ids are `[a-zA-Z0-9][a-zA-Z0-9_.-]*`. We accept
/// both IDs (hex) and names; strict validation prevents shell metacharacters
/// from ever reaching `docker` args.
pub fn validate_target(name: &str) -> Result<(), DockerError> {
    if name.is_empty() || name.len() > 128 {
        return Err(DockerError::BadName(name.to_string()));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(DockerError::BadName(name.to_string()));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
            return Err(DockerError::BadName(name.to_string()));
        }
    }
    Ok(())
}

pub fn run(args: &[&str]) -> Result<String, DockerError> {
    run_with_timeout(args, DEFAULT_TIMEOUT_SECS)
}

pub fn run_with_timeout(args: &[&str], timeout_secs: u64) -> Result<String, DockerError> {
    let bin = bin_path()?;
    let child = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DockerError::SpawnFailed(e.to_string()))?;

    let (tx, rx) = mpsc::channel::<()>();
    let child_id = child.id();
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(Duration::from_secs(timeout_secs)).is_err() {
            #[cfg(unix)]
            unsafe { kill(child_id as i32, 9); }
            return true;
        }
        false
    });

    let output = child.wait_with_output().map_err(|e| DockerError::IoError(e.to_string()))?;
    let _ = tx.send(());
    let killed = watchdog.join().unwrap_or(false);
    if killed && !output.status.success() {
        return Err(DockerError::Timeout(timeout_secs));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(DockerError::NonZeroExit { code: output.status.code(), stderr });
    }
    Ok(stdout)
}

#[cfg(unix)]
extern "C" { fn kill(pid: i32, sig: i32) -> i32; }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validate_name_accepts_typical() {
        assert!(validate_target("mi-container").is_ok());
        assert!(validate_target("abc123").is_ok());
        assert!(validate_target("container.1").is_ok());
    }
    #[test]
    fn validate_name_rejects_injection() {
        assert!(validate_target("").is_err());
        assert!(validate_target("a; rm -rf /").is_err());
        assert!(validate_target("-flag").is_err());
        assert!(validate_target("$(id)").is_err());
    }
}
