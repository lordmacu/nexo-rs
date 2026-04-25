//! Thin wrapper over the `op` 1Password CLI. We shell out because:
//! - 1Password has no stable Rust SDK for service accounts
//! - `op` already implements all auth + caching + protocol negotiation
//! - Service-account tokens make it a non-interactive headless flow
//!
//! The binary can be overridden for testing with `OP_BIN_OVERRIDE` (absolute
//! path to a drop-in script that mimics `op`'s JSON outputs).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const MAX_TIMEOUT_SECS: u64 = 300;

#[derive(Debug)]
pub enum OpError {
    MissingBinary,
    MissingServiceToken,
    SpawnFailed(String),
    NonZeroExit { code: Option<i32>, stderr: String },
    Timeout(u64),
    BadRef(String),
    JsonError(String),
    RevealDenied,
    IoError(String),
}

impl OpError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            OpError::MissingBinary => -32040,
            OpError::MissingServiceToken => -32041,
            OpError::SpawnFailed(_) => -32031,
            OpError::NonZeroExit { .. } => -32042,
            OpError::Timeout(_) => -32033,
            OpError::BadRef(_) => -32602,
            OpError::JsonError(_) => -32006,
            OpError::RevealDenied => -32043,
            OpError::IoError(_) => -32034,
        }
    }

    pub fn message(&self) -> String {
        match self {
            OpError::MissingBinary => {
                "`op` binary not found on PATH and OP_BIN_OVERRIDE is unset".into()
            }
            OpError::MissingServiceToken => {
                "OP_SERVICE_ACCOUNT_TOKEN env var is not set (or empty)".into()
            }
            OpError::SpawnFailed(e) => format!("failed to spawn op: {e}"),
            OpError::NonZeroExit { code, stderr } => match code {
                Some(c) => format!("op exited with code {c}: {}", truncate(stderr, 400)),
                None => format!("op terminated by signal: {}", truncate(stderr, 400)),
            },
            OpError::Timeout(s) => format!("op exceeded {s}s timeout and was killed"),
            OpError::BadRef(r) => format!("invalid secret reference `{r}`"),
            OpError::JsonError(e) => format!("op returned non-JSON output: {e}"),
            OpError::RevealDenied => {
                "reveal_denied: set OP_ALLOW_REVEAL=true to include secret values in tool output"
                    .into()
            }
            OpError::IoError(s) => format!("io error: {s}"),
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

pub fn bin_path() -> Result<PathBuf, OpError> {
    if let Ok(override_) = std::env::var("OP_BIN_OVERRIDE") {
        let p = PathBuf::from(override_);
        if p.is_file() {
            return Ok(p);
        }
        return Err(OpError::IoError(format!(
            "OP_BIN_OVERRIDE `{}` is not a regular file",
            p.display()
        )));
    }
    // Otherwise walk PATH for `op`.
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        let cand = dir.join("op");
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(OpError::MissingBinary)
}

pub fn service_token() -> Result<String, OpError> {
    match std::env::var("OP_SERVICE_ACCOUNT_TOKEN") {
        Ok(t) if !t.trim().is_empty() => Ok(t),
        _ => Err(OpError::MissingServiceToken),
    }
}

pub fn timeout_from_env() -> u64 {
    std::env::var("OP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(|v: u64| v.min(MAX_TIMEOUT_SECS))
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

pub fn reveal_allowed() -> bool {
    matches!(
        std::env::var("OP_ALLOW_REVEAL")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase()),
        Some(ref s) if s == "true" || s == "1" || s == "yes"
    )
}

/// Run `op <args>` with the service-account token in env and a hard timeout.
/// Returns stdout on success. Callers parse JSON themselves.
pub fn run(args: &[&str], timeout_secs: u64) -> Result<String, OpError> {
    let bin = bin_path()?;
    let token = service_token()?;

    let child = Command::new(&bin)
        .args(args)
        .env("OP_SERVICE_ACCOUNT_TOKEN", token)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| OpError::SpawnFailed(e.to_string()))?;

    let (tx, rx) = mpsc::channel::<()>();
    let child_id = child.id();
    let timeout = Duration::from_secs(timeout_secs);

    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            #[cfg(unix)]
            unsafe {
                kill(child_id as i32, 9);
            }
            return true;
        }
        false
    });

    let output = child
        .wait_with_output()
        .map_err(|e| OpError::IoError(e.to_string()))?;
    let _ = tx.send(());
    let killed = watchdog.join().unwrap_or(false);

    if killed && !output.status.success() {
        return Err(OpError::Timeout(timeout_secs));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(OpError::NonZeroExit {
            code: output.status.code(),
            stderr,
        });
    }
    Ok(stdout)
}

pub fn inject_command_allowlist() -> Vec<String> {
    std::env::var("OP_INJECT_COMMAND_ALLOWLIST")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub fn inject_timeout_secs() -> u64 {
    std::env::var("OP_INJECT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|v: &u64| *v > 0)
        .map(|v| v.min(MAX_TIMEOUT_SECS))
        .unwrap_or(30)
}

/// Extract every `op://Vault/Item/field` reference appearing inside
/// `{{ ... }}` placeholders in a template. Used for both auditing
/// and the `dry_run` validation path.
pub fn extract_template_references(template: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\{\{\s*(op://[^\s}]+)\s*\}\}").expect("static regex compiles")
    });
    let mut out = Vec::new();
    for cap in re.captures_iter(template) {
        if let Some(m) = cap.get(1) {
            out.push(m.as_str().to_string());
        }
    }
    out
}

/// Run `op inject` reading the template from stdin, returning the
/// rendered output on stdout.
pub fn run_inject_template_only(template: &str, timeout_secs: u64) -> Result<String, OpError> {
    let bin = bin_path()?;
    let token = service_token()?;

    let mut child = Command::new(&bin)
        .args(["inject"])
        .env("OP_SERVICE_ACCOUNT_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| OpError::SpawnFailed(e.to_string()))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(template.as_bytes())
            .map_err(|e| OpError::IoError(e.to_string()))?;
    }

    let (tx, rx) = mpsc::channel::<()>();
    let child_id = child.id();
    let timeout = Duration::from_secs(timeout_secs);
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            #[cfg(unix)]
            unsafe {
                kill(child_id as i32, 9);
            }
            return true;
        }
        false
    });

    let output = child
        .wait_with_output()
        .map_err(|e| OpError::IoError(e.to_string()))?;
    let _ = tx.send(());
    let killed = watchdog.join().unwrap_or(false);
    if killed && !output.status.success() {
        return Err(OpError::Timeout(timeout_secs));
    }
    if !output.status.success() {
        return Err(OpError::NonZeroExit {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Result of a piped exec invocation.
pub struct InjectExecResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_total_bytes: usize,
    pub stdout_returned_bytes: usize,
    pub stdout_truncated: bool,
}

/// Pipe `template` through `op inject` and feed its stdout to
/// `<command> <args...>`. The downstream stdout/stderr are captured
/// (stdout truncated to `cap_bytes`). The template is rendered in
/// memory only — the caller never sees it.
pub fn run_inject_with_command(
    template: &str,
    command: &str,
    args: &[String],
    cap_bytes: usize,
    timeout_secs: u64,
) -> Result<InjectExecResult, OpError> {
    let rendered = run_inject_template_only(template, timeout_secs)?;
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| OpError::SpawnFailed(e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(rendered.as_bytes())
            .map_err(|e| OpError::IoError(e.to_string()))?;
    }
    let (tx, rx) = mpsc::channel::<()>();
    let child_id = child.id();
    let timeout = Duration::from_secs(timeout_secs);
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            #[cfg(unix)]
            unsafe {
                kill(child_id as i32, 9);
            }
            return true;
        }
        false
    });
    let output = child
        .wait_with_output()
        .map_err(|e| OpError::IoError(e.to_string()))?;
    let _ = tx.send(());
    let killed = watchdog.join().unwrap_or(false);
    if killed && output.status.code().is_none() {
        return Err(OpError::Timeout(timeout_secs));
    }
    let stdout_full = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr_full = String::from_utf8_lossy(&output.stderr).to_string();
    let total = stdout_full.len();
    let truncated = total > cap_bytes;
    let stdout = if truncated {
        let mut s: String = stdout_full.chars().take(cap_bytes).collect();
        s.push_str("…[truncated]");
        s
    } else {
        stdout_full
    };
    let stdout_returned_bytes = stdout.len();
    Ok(InjectExecResult {
        exit_code: output.status.code(),
        stdout,
        stderr: stderr_full,
        stdout_total_bytes: total,
        stdout_returned_bytes,
        stdout_truncated: truncated,
    })
}

/// Strict validator for `op://Vault/Item/field` references. Rejects
/// wildcards, empty segments, and query strings.
pub fn validate_ref(reference: &str) -> Result<(String, String, String), OpError> {
    let r = reference.trim();
    if !r.starts_with("op://") {
        return Err(OpError::BadRef(reference.to_string()));
    }
    let rest = &r[5..];
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() != 3 {
        return Err(OpError::BadRef(reference.to_string()));
    }
    for segment in &parts {
        if segment.is_empty() || segment.contains('*') || segment.contains('?') {
            return Err(OpError::BadRef(reference.to_string()));
        }
    }
    Ok((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ))
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_ref_happy_path() {
        let (v, i, f) = validate_ref("op://Prod/Stripe/api_key").unwrap();
        assert_eq!(v, "Prod");
        assert_eq!(i, "Stripe");
        assert_eq!(f, "api_key");
    }

    #[test]
    fn validate_ref_rejects_wildcard() {
        assert!(validate_ref("op://Prod/*/api_key").is_err());
        assert!(validate_ref("op://*/Stripe/api_key").is_err());
    }

    #[test]
    fn validate_ref_rejects_missing_segments() {
        assert!(validate_ref("op://Prod/Stripe").is_err());
        assert!(validate_ref("op://Prod//api_key").is_err());
        assert!(validate_ref("op://").is_err());
    }

    #[test]
    fn validate_ref_rejects_non_op_scheme() {
        assert!(validate_ref("https://example.com").is_err());
        assert!(validate_ref("/vault/item/field").is_err());
    }

    #[test]
    fn reveal_allowed_reads_env() {
        std::env::remove_var("OP_ALLOW_REVEAL");
        assert!(!reveal_allowed());
        std::env::set_var("OP_ALLOW_REVEAL", "true");
        assert!(reveal_allowed());
        std::env::set_var("OP_ALLOW_REVEAL", "1");
        assert!(reveal_allowed());
        std::env::set_var("OP_ALLOW_REVEAL", "FALSE");
        assert!(!reveal_allowed());
        std::env::remove_var("OP_ALLOW_REVEAL");
    }
}
