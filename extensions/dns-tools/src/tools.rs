use std::net::IpAddr;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;

use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::Resolver;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;

pub const CLIENT_VERSION: &str = "dns-tools-0.1.0";

fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap())
}

fn resolver_system() -> Result<Resolver, ToolError> {
    Resolver::from_system_conf().or_else(|_| Resolver::new(ResolverConfig::cloudflare(), ResolverOpts::default()))
        .map_err(|e| ToolError{code:-32003,message:format!("resolver init failed: {e}")})
}

#[doc(hidden)]
pub fn reset_state() {}

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Resolver info.",
          "input_schema":{"type":"object","additionalProperties":false}},
        { "name":"resolve",
          "description":"DNS lookup. Default type A. Returns list of records (stringified).",
          "input_schema":{"type":"object","properties":{
              "name":{"type":"string"},
              "type":{"type":"string","enum":["A","AAAA","CNAME","MX","TXT","NS","SOA","SRV","PTR","CAA"],"description":"default A"}
          },"required":["name"],"additionalProperties":false}},
        { "name":"reverse","description":"PTR lookup for an IP address.",
          "input_schema":{"type":"object","properties":{
              "ip":{"type":"string"}
          },"required":["ip"],"additionalProperties":false}},
        { "name":"whois","description":"WHOIS record for a domain. Queries whois.iana.org then follows referral.",
          "input_schema":{"type":"object","properties":{
              "domain":{"type":"string"}
          },"required":["domain"],"additionalProperties":false}}
    ])
}

#[derive(Debug)]
pub struct ToolError { pub code: i32, pub message: String }

fn bad_input(m: impl Into<String>) -> ToolError { ToolError{code:-32602,message:m.into()} }

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "resolve" => resolve(args),
        "reverse" => reverse(args),
        "whois" => whois(args),
        other => Err(ToolError{code:-32601,message:format!("unknown tool `{other}`")}),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "dns-tools",
        "client_version": CLIENT_VERSION,
        "tools": ["status","resolve","reverse","whois"],
        "requires": {"bins":[],"env":[]}
    })
}

fn resolve(args: &Value) -> Result<Value, ToolError> {
    let name = required_string(args, "name")?;
    let ty = args.get("type").and_then(|v| v.as_str()).unwrap_or("A").to_ascii_uppercase();
    let r = resolver_system()?;
    let records: Vec<String> = match ty.as_str() {
        "A" => r.ipv4_lookup(&name).map(|l| l.into_iter().map(|a| a.to_string()).collect())
            .map_err(|e| ToolError{code:-32003,message:e.to_string()})?,
        "AAAA" => r.ipv6_lookup(&name).map(|l| l.into_iter().map(|a| a.to_string()).collect())
            .map_err(|e| ToolError{code:-32003,message:e.to_string()})?,
        "CNAME" => lookup_generic(&r, &name, hickory_resolver::proto::rr::RecordType::CNAME)?,
        "MX" => r.mx_lookup(&name).map(|l| l.into_iter().map(|m| format!("{} {}", m.preference(), m.exchange())).collect())
            .map_err(|e| ToolError{code:-32003,message:e.to_string()})?,
        "TXT" => r.txt_lookup(&name).map(|l| l.into_iter().map(|t| t.to_string()).collect())
            .map_err(|e| ToolError{code:-32003,message:e.to_string()})?,
        "NS" => r.ns_lookup(&name).map(|l| l.into_iter().map(|n| n.to_string()).collect())
            .map_err(|e| ToolError{code:-32003,message:e.to_string()})?,
        "SOA" => r.soa_lookup(&name).map(|l| l.into_iter().map(|s| s.to_string()).collect())
            .map_err(|e| ToolError{code:-32003,message:e.to_string()})?,
        "SRV" => r.srv_lookup(&name).map(|l| l.into_iter().map(|s| format!("{} {} {} {}", s.priority(), s.weight(), s.port(), s.target())).collect())
            .map_err(|e| ToolError{code:-32003,message:e.to_string()})?,
        "CAA" => lookup_generic(&r, &name, hickory_resolver::proto::rr::RecordType::CAA)?,
        "PTR" => return Err(bad_input("use `reverse` tool for PTR")),
        other => return Err(bad_input(format!("unsupported type: {other}"))),
    };
    Ok(json!({"ok": true, "name": name, "type": ty, "count": records.len(), "records": records}))
}

fn lookup_generic(r: &Resolver, name: &str, ty: hickory_resolver::proto::rr::RecordType) -> Result<Vec<String>, ToolError> {
    let resp = r.lookup(name, ty).map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
    Ok(resp.iter().map(|d| d.to_string()).collect())
}

fn reverse(args: &Value) -> Result<Value, ToolError> {
    let ip_str = required_string(args, "ip")?;
    let ip = IpAddr::from_str(&ip_str).map_err(|e| bad_input(format!("invalid ip: {e}")))?;
    let r = resolver_system()?;
    let lookup = r.reverse_lookup(ip).map_err(|e| ToolError{code:-32003,message:e.to_string()})?;
    let names: Vec<String> = lookup.iter().map(|n| n.to_string()).collect();
    Ok(json!({"ok": true, "ip": ip_str, "count": names.len(), "names": names}))
}

fn whois(args: &Value) -> Result<Value, ToolError> {
    let domain = required_string(args, "domain")?;
    let res = rt().block_on(async {
        let iana = whois_query("whois.iana.org", &domain).await?;
        let referral = iana.lines()
            .find(|l| l.to_ascii_lowercase().starts_with("whois:"))
            .and_then(|l| l.splitn(2, ':').nth(1))
            .map(|s| s.trim().to_string());
        let (server, body) = if let Some(s) = referral {
            let b = whois_query(&s, &domain).await.unwrap_or_else(|e| format!("(referral {s} failed: {e})"));
            (s, b)
        } else {
            ("whois.iana.org".to_string(), iana)
        };
        Ok::<_, String>((server, body))
    }).map_err(|e| ToolError{code:-32003,message:e})?;
    Ok(json!({"ok": true, "domain": domain, "server": res.0, "body": truncate(&res.1, 8000)}))
}

async fn whois_query(server: &str, query: &str) -> Result<String, String> {
    let addr = format!("{server}:43");
    let mut stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&addr))
        .await.map_err(|_| "connect timeout".to_string())?
        .map_err(|e| format!("connect failed: {e}"))?;
    stream.write_all(format!("{query}\r\n").as_bytes()).await.map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut buf))
        .await.map_err(|_| "read timeout".to_string())?
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).to_string())
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
