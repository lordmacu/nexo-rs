use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "ssh-exec-0.1.0";

fn ssh_bin() -> String {
    std::env::var("SSH_BIN").unwrap_or_else(|_| "ssh".into())
}
fn scp_bin() -> String {
    std::env::var("SCP_BIN").unwrap_or_else(|_| "scp".into())
}

fn allowlist() -> Vec<String> {
    std::env::var("SSH_EXEC_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn is_host_allowed(host: &str) -> bool {
    let list = allowlist();
    if list.is_empty() {
        return false;
    }
    list.iter().any(|h| h == host)
}

fn default_timeout() -> u64 {
    std::env::var("SSH_EXEC_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
}

fn allow_writes() -> bool {
    std::env::var("SSH_EXEC_ALLOW_WRITES").ok().as_deref() == Some("true")
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Allowlist + bin info.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"exec",
          "description":"Run a command on a remote host. Host must be in SSH_EXEC_ALLOWED_HOSTS.",
          "input_schema":{"type":"object","properties":{
              "host":{"type":"string","description":"`user@hostname` or alias from ~/.ssh/config"},
              "command":{"type":"string"},
              "timeout_secs":{"type":"integer","minimum":1,"maximum":600},
              "identity_file":{"type":"string","description":"Path to private key (optional)"}
          },"required":["host","command"],"additionalProperties":false}},
        { "name":"scp_upload","description":"Upload local file to remote path. Requires SSH_EXEC_ALLOW_WRITES=true.",
          "input_schema":{"type":"object","properties":{
              "host":{"type":"string"},
              "local_path":{"type":"string"},
              "remote_path":{"type":"string"},
              "identity_file":{"type":"string"}
          },"required":["host","local_path","remote_path"],"additionalProperties":false}},
        { "name":"scp_download","description":"Download remote file to local path.",
          "input_schema":{"type":"object","properties":{
              "host":{"type":"string"},
              "remote_path":{"type":"string"},
              "local_path":{"type":"string"},
              "identity_file":{"type":"string"}
          },"required":["host","remote_path","local_path"],"additionalProperties":false}}
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
fn denied(m: impl Into<String>) -> ToolError {
    ToolError {
        code: -32041,
        message: m.into(),
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "exec" => exec(args),
        "scp_upload" => {
            if !allow_writes() {
                return Err(denied("SSH_EXEC_ALLOW_WRITES not set to `true`"));
            }
            scp_upload(args)
        }
        "scp_download" => scp_download(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    json!({
        "ok": !allowlist().is_empty(),
        "provider": "ssh-exec",
        "client_version": CLIENT_VERSION,
        "ssh_bin": ssh_bin(),
        "scp_bin": scp_bin(),
        "allowed_hosts": allowlist(),
        "allow_writes": allow_writes(),
        "write_gate": {
            "enabled": allow_writes(),
            "env": "SSH_EXEC_ALLOW_WRITES",
            "hint": "set SSH_EXEC_ALLOW_WRITES=true to enable `scp_upload`"
        },
        "default_timeout_secs": default_timeout(),
        "tools": ["status","exec","scp_upload","scp_download"],
        "requires": {"bins":["ssh","scp"],"env":["SSH_EXEC_ALLOWED_HOSTS"]}
    })
}

fn check_host(host: &str) -> Result<(), ToolError> {
    if !is_host_allowed(host) {
        return Err(denied(format!(
            "host `{host}` not in SSH_EXEC_ALLOWED_HOSTS"
        )));
    }
    Ok(())
}

fn common_ssh_args(identity_file: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if let Some(idf) = identity_file {
        v.push("-i".into());
        v.push(idf.to_string());
    }
    v
}

fn exec(args: &Value) -> Result<Value, ToolError> {
    let host = required_string(args, "host")?;
    check_host(&host)?;
    let command = required_string(args, "command")?;
    let timeout_secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(default_timeout)
        .min(600);
    let identity_file = args.get("identity_file").and_then(|v| v.as_str());

    let mut cmd = Command::new(ssh_bin());
    for a in common_ssh_args(identity_file) {
        cmd.arg(a);
    }
    cmd.arg(&host)
        .arg("--")
        .arg(&command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = cmd.spawn().map_err(|e| ToolError {
        code: -32003,
        message: format!("spawn failed: {e}"),
    })?;
    let waited = wait_with_timeout(&mut child, Duration::from_secs(timeout_secs))?;

    let stdout = String::from_utf8_lossy(&waited.stdout).to_string();
    let stderr = String::from_utf8_lossy(&waited.stderr).to_string();
    let code = waited.status.code();
    Ok(json!({
        "ok": waited.status.success(),
        "host": host,
        "exit_code": code,
        "stdout": truncate(&stdout, 16_000),
        "stderr": truncate(&stderr, 8_000),
        "stdout_truncated": stdout.len() > 16_000,
        "stderr_truncated": stderr.len() > 8_000,
    }))
}

struct Waited {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<Waited, ToolError> {
    use std::io::Read;
    use std::time::Instant;
    let start = Instant::now();
    loop {
        match child.try_wait().map_err(|e| ToolError {
            code: -32003,
            message: e.to_string(),
        })? {
            Some(status) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_end(&mut stdout);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_end(&mut stderr);
                }
                return Ok(Waited {
                    status,
                    stdout,
                    stderr,
                });
            }
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ToolError {
                        code: -32005,
                        message: format!("timeout after {}s", timeout.as_secs()),
                    });
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn scp_upload(args: &Value) -> Result<Value, ToolError> {
    let host = required_string(args, "host")?;
    check_host(&host)?;
    let local = required_string(args, "local_path")?;
    let remote = required_string(args, "remote_path")?;
    let identity_file = args.get("identity_file").and_then(|v| v.as_str());

    if !std::path::Path::new(&local).exists() {
        return Err(bad_input(format!("local_path does not exist: {local}")));
    }

    let mut cmd = Command::new(scp_bin());
    for a in common_ssh_args(identity_file) {
        cmd.arg(a);
    }
    cmd.arg(&local).arg(format!("{host}:{remote}"));
    let out = cmd.output().map_err(|e| ToolError {
        code: -32003,
        message: format!("spawn failed: {e}"),
    })?;
    Ok(json!({
        "ok": out.status.success(),
        "exit_code": out.status.code(),
        "stderr": String::from_utf8_lossy(&out.stderr).to_string(),
        "direction": "upload",
        "local_path": local,
        "remote": format!("{host}:{remote}"),
    }))
}

fn scp_download(args: &Value) -> Result<Value, ToolError> {
    let host = required_string(args, "host")?;
    check_host(&host)?;
    let remote = required_string(args, "remote_path")?;
    let local = required_string(args, "local_path")?;
    let identity_file = args.get("identity_file").and_then(|v| v.as_str());

    if let Some(parent) = std::path::Path::new(&local).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut cmd = Command::new(scp_bin());
    for a in common_ssh_args(identity_file) {
        cmd.arg(a);
    }
    cmd.arg(format!("{host}:{remote}")).arg(&local);
    let out = cmd.output().map_err(|e| ToolError {
        code: -32003,
        message: format!("spawn failed: {e}"),
    })?;
    let size = std::fs::metadata(&local).map(|m| m.len()).ok();
    Ok(json!({
        "ok": out.status.success(),
        "exit_code": out.status.code(),
        "stderr": String::from_utf8_lossy(&out.stderr).to_string(),
        "direction": "download",
        "remote": format!("{host}:{remote}"),
        "local_path": local,
        "bytes": size,
    }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim()
        .to_string();
    if s.is_empty() {
        return Err(bad_input(format!("`{key}` cannot be empty")));
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
        std::env::remove_var("SSH_EXEC_ALLOW_WRITES");
        let out = dispatch("status", &json!({})).unwrap();
        assert_eq!(out["write_gate"]["enabled"], false);
        assert_eq!(out["write_gate"]["env"], "SSH_EXEC_ALLOW_WRITES");
    }

    #[test]
    fn scp_upload_denied_when_gate_is_off() {
        std::env::remove_var("SSH_EXEC_ALLOW_WRITES");
        let err = dispatch(
            "scp_upload",
            &json!({"host":"user@example.com","local_path":"/tmp/a","remote_path":"/tmp/b"}),
        )
        .unwrap_err();
        assert_eq!(err.code, -32041);
        assert!(err.message.contains("SSH_EXEC_ALLOW_WRITES"));
    }
}
