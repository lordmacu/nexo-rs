use std::sync::OnceLock;
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{json, Value};

pub const CLIENT_VERSION: &str = "proxmox-0.1.0";
const ENV_WRITE: &str = "PROXMOX_ALLOW_WRITE";

fn base_url() -> Option<String> {
    std::env::var("PROXMOX_URL").ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

fn token() -> Option<String> {
    std::env::var("PROXMOX_TOKEN").ok().filter(|s| !s.trim().is_empty())
}

fn accept_invalid_certs() -> bool {
    matches!(std::env::var("PROXMOX_INSECURE_TLS").ok().map(|s| s.to_ascii_lowercase()),
             Some(ref s) if s == "true" || s == "1" || s == "yes")
}

fn http() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("agent-rs-proxmox/0.1")
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .danger_accept_invalid_certs(accept_invalid_certs())
            .build()
            .unwrap()
    })
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Proxmox endpoint + token presence + write flag.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"list_nodes","description":"List cluster nodes + their status.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"list_vms","description":"List all QEMU VMs across the cluster (or a single node).",
          "input_schema":{"type":"object","properties":{
              "node":{"type":"string","description":"restrict to one node"}
          },"additionalProperties":false}},
        { "name":"list_containers","description":"List all LXC containers.",
          "input_schema":{"type":"object","properties":{
              "node":{"type":"string"}
          },"additionalProperties":false}},
        { "name":"vm_status","description":"Current status of a VM or container by vmid on a specific node.",
          "input_schema":{"type":"object","properties":{
              "node":{"type":"string"},
              "vmid":{"type":"integer","minimum":100,"maximum":999999999},
              "kind":{"type":"string","enum":["qemu","lxc"],"description":"default qemu"}
          },"required":["node","vmid"],"additionalProperties":false}},
        { "name":"vm_action","description":"Lifecycle action on a VM/container: start, stop, shutdown, reboot, suspend, resume. Requires PROXMOX_ALLOW_WRITE=true.",
          "input_schema":{"type":"object","properties":{
              "node":{"type":"string"},
              "vmid":{"type":"integer","minimum":100},
              "kind":{"type":"string","enum":["qemu","lxc"]},
              "action":{"type":"string","enum":["start","stop","shutdown","reboot","suspend","resume"]}
          },"required":["node","vmid","action"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

fn flag_on() -> bool {
    matches!(std::env::var(ENV_WRITE).ok().map(|s| s.trim().to_ascii_lowercase()),
             Some(ref s) if s == "true" || s == "1" || s == "yes")
}

fn require_write() -> Result<(), ToolError> {
    if flag_on() { Ok(()) }
    else { Err(ToolError{code:-32043, message:format!("write denied: set {ENV_WRITE}=true")}) }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "list_nodes" => list_nodes(),
        "list_vms" => list_vms(args),
        "list_containers" => list_containers(args),
        "vm_status" => vm_status(args),
        "vm_action" => vm_action(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    json!({
        "ok": base_url().is_some() && token().is_some(),
        "provider": "proxmox-ve",
        "client_version": CLIENT_VERSION,
        "endpoint": base_url(),
        "token_present": token().is_some(),
        "insecure_tls": accept_invalid_certs(),
        "write_allowed": flag_on(),
        "tools": ["status","list_nodes","list_vms","list_containers","vm_status","vm_action"],
        "requires": {"bins":[],"env":["PROXMOX_URL","PROXMOX_TOKEN"]}
    })
}

fn pve_get(path: &str, query: &[(&str, String)]) -> Result<Value, ToolError> {
    let base = base_url().ok_or_else(|| ToolError{code:-32041,message:"PROXMOX_URL not set".into()})?;
    let tok = token().ok_or_else(|| ToolError{code:-32041,message:"PROXMOX_TOKEN not set".into()})?;
    let url = format!("{}/api2/json{}", base, path);
    let resp = http().get(&url)
        .header("Authorization", format!("PVEAPIToken={tok}"))
        .query(query)
        .send()
        .map_err(|e| ToolError{code:-32003, message: e.to_string()})?;
    let s = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(match s {
            401|403 => ToolError{code:-32011, message: format!("unauthorized: {}", truncate(&body, 200))},
            404 => ToolError{code:-32001, message: format!("not found: {}", truncate(&body, 200))},
            _ => ToolError{code:-32003, message: format!("HTTP {s}: {}", truncate(&body, 200))}
        });
    }
    resp.json::<Value>().map_err(|e| ToolError{code:-32006, message: e.to_string()})
}

fn pve_post(path: &str) -> Result<Value, ToolError> {
    let base = base_url().ok_or_else(|| ToolError{code:-32041,message:"PROXMOX_URL not set".into()})?;
    let tok = token().ok_or_else(|| ToolError{code:-32041,message:"PROXMOX_TOKEN not set".into()})?;
    let url = format!("{}/api2/json{}", base, path);
    let resp = http().post(&url)
        .header("Authorization", format!("PVEAPIToken={tok}"))
        .send()
        .map_err(|e| ToolError{code:-32003, message: e.to_string()})?;
    let s = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(match s {
            401|403 => ToolError{code:-32011, message: format!("unauthorized: {}", truncate(&body, 200))},
            404 => ToolError{code:-32001, message: format!("not found: {}", truncate(&body, 200))},
            _ => ToolError{code:-32003, message: format!("HTTP {s}: {}", truncate(&body, 200))}
        });
    }
    resp.json::<Value>().map_err(|e| ToolError{code:-32006, message: e.to_string()})
}

fn validate_node(n: &str) -> Result<(), ToolError> {
    if n.is_empty() || n.len() > 100 || !n.chars().all(|c| c.is_ascii_alphanumeric() || c=='-' || c=='.') {
        return Err(bad_input(format!("invalid node `{n}`")));
    }
    Ok(())
}

fn list_nodes() -> Result<Value, ToolError> {
    let v = pve_get("/nodes", &[])?;
    Ok(json!({"ok": true, "data": v.get("data")}))
}

fn list_vms(args: &Value) -> Result<Value, ToolError> {
    let node = args.get("node").and_then(|v| v.as_str()).map(|s| s.trim());
    if let Some(n) = node {
        validate_node(n)?;
        let v = pve_get(&format!("/nodes/{n}/qemu"), &[])?;
        Ok(json!({"ok": true, "node": n, "vms": v.get("data")}))
    } else {
        let v = pve_get("/cluster/resources", &[("type", "vm".into())])?;
        Ok(json!({"ok": true, "vms": v.get("data")}))
    }
}

fn list_containers(args: &Value) -> Result<Value, ToolError> {
    let node = args.get("node").and_then(|v| v.as_str()).map(|s| s.trim());
    if let Some(n) = node {
        validate_node(n)?;
        let v = pve_get(&format!("/nodes/{n}/lxc"), &[])?;
        Ok(json!({"ok": true, "node": n, "containers": v.get("data")}))
    } else {
        // no first-class "all containers across cluster" — /cluster/resources with type=vm returns both
        let v = pve_get("/cluster/resources", &[])?;
        let filtered: Vec<Value> = v.get("data")
            .and_then(|a| a.as_array()).cloned().unwrap_or_default()
            .into_iter()
            .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("lxc"))
            .collect();
        Ok(json!({"ok": true, "containers": filtered}))
    }
}

fn vm_status(args: &Value) -> Result<Value, ToolError> {
    let node = required_string(args, "node")?;
    validate_node(&node)?;
    let vmid = args.get("vmid").and_then(|v| v.as_u64())
        .ok_or_else(|| bad_input("`vmid` required as integer"))?;
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("qemu");
    if !["qemu","lxc"].contains(&kind) { return Err(bad_input("kind must be qemu|lxc")); }
    let path = format!("/nodes/{node}/{kind}/{vmid}/status/current");
    let v = pve_get(&path, &[])?;
    Ok(json!({"ok": true, "node": node, "vmid": vmid, "kind": kind, "status": v.get("data")}))
}

fn vm_action(args: &Value) -> Result<Value, ToolError> {
    require_write()?;
    let node = required_string(args, "node")?;
    validate_node(&node)?;
    let vmid = args.get("vmid").and_then(|v| v.as_u64())
        .ok_or_else(|| bad_input("`vmid` required as integer"))?;
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("qemu");
    if !["qemu","lxc"].contains(&kind) { return Err(bad_input("kind must be qemu|lxc")); }
    let action = required_string(args, "action")?;
    if !["start","stop","shutdown","reboot","suspend","resume"].contains(&action.as_str()) {
        return Err(bad_input("action must be start|stop|shutdown|reboot|suspend|resume"));
    }
    let path = format!("/nodes/{node}/{kind}/{vmid}/status/{action}");
    let v = pve_post(&path)?;
    // Response is typically `{"data": "UPID:node:..."}` — a Proxmox task id.
    Ok(json!({"ok": true, "node": node, "vmid": vmid, "kind": kind, "action": action, "task": v.get("data")}))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args.get(key).and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing `{key}`")))?
        .trim().to_string();
    if s.is_empty() { return Err(bad_input(format!("`{key}` cannot be empty"))); }
    Ok(s)
}
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}
