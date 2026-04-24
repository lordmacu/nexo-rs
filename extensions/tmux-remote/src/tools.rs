use serde_json::{json, Value};

use crate::tmux_cli::{self, validate_session_name, TmuxError};

pub const CLIENT_VERSION: &str = "tmux-remote-0.1.0";
const MAX_CAPTURE_LINES: i64 = 2000;
const DEFAULT_CAPTURE_LINES: i64 = 200;

pub fn tool_schemas() -> Value {
    json!([
        { "name": "status", "description": "Returns tmux bin + socket path.", "input_schema": {"type":"object","additionalProperties":false}},
        { "name": "new_session",
          "description": "Create a detached tmux session. Names are restricted to [a-zA-Z0-9_-].",
          "input_schema": {
              "type": "object",
              "properties": {
                  "name":    { "type": "string", "description": "1..=64 chars, alphanumeric/dash/underscore" },
                  "command": { "type": "string", "description": "Optional initial command to run in the session" }
              },
              "required": ["name"], "additionalProperties": false
          }
        },
        { "name": "send_keys",
          "description": "Send literal text to a pane; set `enter: true` to also send Enter.",
          "input_schema": {
              "type": "object",
              "properties": {
                  "session": { "type": "string" },
                  "keys":    { "type": "string" },
                  "enter":   { "type": "boolean", "description": "default true" }
              },
              "required": ["session","keys"], "additionalProperties": false
          }
        },
        { "name": "capture_pane",
          "description": "Capture the last N lines of the active pane.",
          "input_schema": {
              "type":"object",
              "properties": {
                  "session":   { "type":"string" },
                  "lines":     { "type":"integer", "minimum": 1, "maximum": MAX_CAPTURE_LINES, "description": "default 200" }
              },
              "required": ["session"], "additionalProperties": false
          }
        },
        { "name": "list_sessions", "description": "List live sessions.",
          "input_schema": {"type":"object","additionalProperties":false}
        },
        { "name": "kill_session",
          "description": "Terminate a session.",
          "input_schema": {
              "type":"object",
              "properties": {"session": {"type":"string"}},
              "required": ["session"], "additionalProperties": false
          }
        }
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl From<TmuxError> for ToolError {
    fn from(e: TmuxError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

fn bad_input(msg: impl Into<String>) -> ToolError {
    ToolError { code: -32602, message: msg.into() }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "new_session" => new_session(args),
        "send_keys" => send_keys(args),
        "capture_pane" => capture_pane(args),
        "list_sessions" => list_sessions(),
        "kill_session" => kill_session(args),
        other => Err(ToolError { code: -32601, message: format!("unknown tool `{other}`") }),
    }
}

fn status() -> Value {
    let bin = tmux_cli::bin_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("missing: {}", e.message()));
    json!({
        "ok": !bin.starts_with("missing"),
        "provider": "tmux",
        "client_version": CLIENT_VERSION,
        "bin": bin,
        "socket": tmux_cli::socket_path().display().to_string(),
        "tools": ["status","new_session","send_keys","capture_pane","list_sessions","kill_session"],
        "requires": { "bins": ["tmux"], "env": [] }
    })
}

fn new_session(args: &Value) -> Result<Value, ToolError> {
    let name = required_string(args, "name")?;
    validate_session_name(&name)?;
    let mut cmd_args: Vec<&str> = vec!["new-session", "-d", "-s", &name];
    let command = optional_string(args, "command");
    if let Some(c) = command.as_deref() {
        cmd_args.push(c);
    }
    tmux_cli::run(&cmd_args)?;
    Ok(json!({ "ok": true, "session": name }))
}

fn send_keys(args: &Value) -> Result<Value, ToolError> {
    let session = required_string(args, "session")?;
    validate_session_name(&session)?;
    let keys = required_string(args, "keys")?;
    let enter = args.get("enter").and_then(|v| v.as_bool()).unwrap_or(true);

    // Two invocations: one for the literal text, one optional Enter. Avoids
    // tmux's own parsing of C-m/Enter inside the keys string.
    tmux_cli::run(&["send-keys", "-t", &session, &keys])?;
    if enter {
        tmux_cli::run(&["send-keys", "-t", &session, "Enter"])?;
    }
    Ok(json!({ "ok": true, "session": session, "sent_enter": enter }))
}

fn capture_pane(args: &Value) -> Result<Value, ToolError> {
    let session = required_string(args, "session")?;
    validate_session_name(&session)?;
    let lines = optional_i64(args, "lines")?.unwrap_or(DEFAULT_CAPTURE_LINES);
    if !(1..=MAX_CAPTURE_LINES).contains(&lines) {
        return Err(bad_input(format!("lines must be 1..={MAX_CAPTURE_LINES}")));
    }
    let start = format!("-{lines}");
    let out = tmux_cli::run(&["capture-pane", "-p", "-t", &session, "-S", &start])?;
    Ok(json!({
        "ok": true,
        "session": session,
        "lines_requested": lines,
        "output": out,
    }))
}

fn list_sessions() -> Result<Value, ToolError> {
    // `tmux ls` returns non-zero with stderr "no server running on ..." when
    // no sessions exist. Normalize that to an empty list.
    match tmux_cli::run(&["list-sessions", "-F", "#{session_name}|#{session_created}|#{session_windows}"]) {
        Ok(stdout) => {
            let sessions: Vec<Value> = stdout
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let parts: Vec<&str> = l.splitn(3, '|').collect();
                    json!({
                        "name": parts.first().copied().unwrap_or(""),
                        "created_unix": parts.get(1).and_then(|s| s.parse::<i64>().ok()),
                        "windows": parts.get(2).and_then(|s| s.parse::<i64>().ok()),
                    })
                })
                .collect();
            Ok(json!({ "ok": true, "count": sessions.len(), "sessions": sessions }))
        }
        Err(TmuxError::NonZeroExit { stderr, .. })
            if stderr.contains("no server")
                || stderr.contains("No such file or directory")
                || stderr.contains("error connecting") =>
        {
            Ok(json!({ "ok": true, "count": 0, "sessions": [] }))
        }
        Err(e) => Err(e.into()),
    }
}

fn kill_session(args: &Value) -> Result<Value, ToolError> {
    let session = required_string(args, "session")?;
    validate_session_name(&session)?;
    tmux_cli::run(&["kill-session", "-t", &session])?;
    Ok(json!({ "ok": true, "killed": session }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing or invalid `{key}`")))?
        .trim().to_string();
    if s.is_empty() {
        return Err(bad_input(format!("`{key}` cannot be empty")));
    }
    Ok(s)
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str())
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn optional_i64(args: &Value, key: &str) -> Result<Option<i64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_i64().map(Some).ok_or_else(|| bad_input(format!("`{key}` must be integer"))),
    }
}
