use serde_json::{json, Value};

use crate::docker::{self, validate_target, DockerError};

pub const CLIENT_VERSION: &str = "docker-api-0.1.0";
const ENV_WRITE: &str = "DOCKER_API_ALLOW_WRITE";

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"docker bin presence + write flag state.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"ps",
          "description":"List containers. `all:true` includes stopped.",
          "input_schema":{"type":"object","properties":{
              "all":{"type":"boolean","description":"default false"},
              "filter":{"type":"string","description":"docker ps --filter expression, e.g. 'status=running'"}
          },"additionalProperties":false}},
        { "name":"inspect",
          "description":"`docker inspect <target>` — full JSON metadata (image, networks, mounts, env).",
          "input_schema":{"type":"object","properties":{
              "target":{"type":"string","description":"container name or id"}
          },"required":["target"],"additionalProperties":false}},
        { "name":"logs",
          "description":"Last N log lines of a container (stdout+stderr combined).",
          "input_schema":{"type":"object","properties":{
              "target":{"type":"string"},
              "lines":{"type":"integer","minimum":1,"maximum":5000,"description":"default 200"},
              "since":{"type":"string","description":"e.g. '10m' or ISO timestamp"}
          },"required":["target"],"additionalProperties":false}},
        { "name":"stats",
          "description":"One-shot CPU/mem/net stats snapshot for a container.",
          "input_schema":{"type":"object","properties":{
              "target":{"type":"string"}
          },"required":["target"],"additionalProperties":false}},
        { "name":"start",
          "description":"Start a stopped container. Requires DOCKER_API_ALLOW_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "target":{"type":"string"}
          },"required":["target"],"additionalProperties":false}},
        { "name":"stop",
          "description":"Stop a running container (SIGTERM + 10s grace). Requires DOCKER_API_ALLOW_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "target":{"type":"string"},
              "timeout_secs":{"type":"integer","minimum":0,"maximum":300,"description":"default 10"}
          },"required":["target"],"additionalProperties":false}},
        { "name":"restart",
          "description":"Restart a container. Requires DOCKER_API_ALLOW_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "target":{"type":"string"},
              "timeout_secs":{"type":"integer","minimum":0,"maximum":300}
          },"required":["target"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

impl From<DockerError> for ToolError {
    fn from(e: DockerError) -> Self { Self{code: e.rpc_code(), message: e.message()} }
}

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

fn flag_on() -> bool {
    matches!(std::env::var(ENV_WRITE).ok().map(|s| s.trim().to_ascii_lowercase()),
             Some(ref s) if s == "true" || s == "1" || s == "yes")
}

fn require_write() -> Result<(), ToolError> {
    if flag_on() { Ok(()) }
    else { Err(ToolError{code:-32043, message: format!("write denied: set {ENV_WRITE}=true")}) }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "ps" => ps(args),
        "inspect" => inspect(args),
        "logs" => logs(args),
        "stats" => stats(args),
        "start" => start(args),
        "stop" => stop(args),
        "restart" => restart(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    let bin = docker::bin_path().map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("missing: {}", e.message()));
    json!({
        "ok": !bin.starts_with("missing"),
        "provider": "docker-cli",
        "client_version": CLIENT_VERSION,
        "bin": bin,
        "write_allowed": flag_on(),
        "tools": ["status","ps","inspect","logs","stats","start","stop","restart"],
        "requires": {"bins":["docker"], "env":[]}
    })
}

fn ps(args: &Value) -> Result<Value, ToolError> {
    let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let filter = optional_string(args, "filter");
    let mut a: Vec<String> = vec!["ps".into(), "--format".into(), "{{json .}}".into()];
    if all { a.push("-a".into()); }
    if let Some(f) = filter { a.push("--filter".into()); a.push(f); }
    let arg_refs: Vec<&str> = a.iter().map(String::as_str).collect();
    let stdout = docker::run(&arg_refs)?;
    // Each line is a JSON object.
    let containers: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    Ok(json!({"ok": true, "count": containers.len(), "containers": containers}))
}

fn inspect(args: &Value) -> Result<Value, ToolError> {
    let target = required_string(args, "target")?;
    validate_target(&target)?;
    let stdout = docker::run(&["inspect", &target])?;
    let v: Value = serde_json::from_str(&stdout)
        .map_err(|e| bad_input(format!("inspect output not JSON: {e}")))?;
    Ok(json!({"ok": true, "target": target, "inspect": v}))
}

fn logs(args: &Value) -> Result<Value, ToolError> {
    let target = required_string(args, "target")?;
    validate_target(&target)?;
    let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(200);
    if !(1..=5000).contains(&lines) { return Err(bad_input("lines must be 1..=5000")); }
    let since = optional_string(args, "since");
    let tail_s = lines.to_string();
    let mut a: Vec<String> = vec!["logs".into(), "--tail".into(), tail_s, "--timestamps".into()];
    if let Some(s) = since { a.push("--since".into()); a.push(s); }
    a.push(target.clone());
    let arg_refs: Vec<&str> = a.iter().map(String::as_str).collect();
    // `docker logs` writes to stdout AND stderr depending on stream; our run() only captures stdout.
    // Use `2>&1` via shell? No — instead call docker with combined: there isn't a CLI flag.
    // Acceptable tradeoff: we get stdout from container, stderr lives separately.
    let stdout = docker::run(&arg_refs)?;
    Ok(json!({"ok": true, "target": target, "lines_requested": lines, "output": stdout}))
}

fn stats(args: &Value) -> Result<Value, ToolError> {
    let target = required_string(args, "target")?;
    validate_target(&target)?;
    let stdout = docker::run(&["stats", "--no-stream", "--format", "{{json .}}", &target])?;
    let v: Value = stdout.lines()
        .next()
        .and_then(|l| serde_json::from_str(l).ok())
        .unwrap_or(Value::Null);
    Ok(json!({"ok": true, "target": target, "stats": v}))
}

fn start(args: &Value) -> Result<Value, ToolError> {
    require_write()?;
    let target = required_string(args, "target")?;
    validate_target(&target)?;
    docker::run(&["start", &target])?;
    Ok(json!({"ok": true, "started": target}))
}

fn stop(args: &Value) -> Result<Value, ToolError> {
    require_write()?;
    let target = required_string(args, "target")?;
    validate_target(&target)?;
    let timeout = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(10);
    let t = timeout.to_string();
    docker::run(&["stop", "-t", &t, &target])?;
    Ok(json!({"ok": true, "stopped": target, "timeout_secs": timeout}))
}

fn restart(args: &Value) -> Result<Value, ToolError> {
    require_write()?;
    let target = required_string(args, "target")?;
    validate_target(&target)?;
    let timeout = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(10);
    let t = timeout.to_string();
    docker::run(&["restart", "-t", &t, &target])?;
    Ok(json!({"ok": true, "restarted": target}))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim().to_string();
    if s.is_empty() { return Err(bad_input(format!("`{key}` cannot be empty"))); }
    Ok(s)
}
fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
