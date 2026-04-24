use reqwest::Method;
use serde_json::{json, Value};

use crate::client::{self, Request, SpotifyError, CLIENT_VERSION};

pub fn tool_schemas() -> Value {
    json!([
        { "name":"status","description":"Returns token presence + endpoint info.","input_schema":{"type":"object","additionalProperties":false}},
        { "name":"now_playing","description":"Current playback state: track, artist, album, is_playing, progress_ms, device.",
          "input_schema":{"type":"object","additionalProperties":false}
        },
        { "name":"search",
          "description":"Search Spotify catalog. `types` is comma-sep subset of track,artist,album,playlist.",
          "input_schema":{
              "type":"object",
              "properties":{
                  "query":{"type":"string"},
                  "types":{"type":"string","description":"default 'track'"},
                  "limit":{"type":"integer","minimum":1,"maximum":50,"description":"default 10"}
              },
              "required":["query"],"additionalProperties":false
          }
        },
        { "name":"play",
          "description":"Start/resume playback. Omit `uri` to resume; provide a spotify URI to play that track/playlist/album.",
          "input_schema":{
              "type":"object",
              "properties":{
                  "uri":{"type":"string","description":"spotify:track:... / spotify:album:... / spotify:playlist:..."},
                  "device_id":{"type":"string"}
              },
              "additionalProperties":false
          }
        },
        { "name":"pause","description":"Pause playback on the active device.",
          "input_schema":{"type":"object","properties":{"device_id":{"type":"string"}},"additionalProperties":false}
        },
        { "name":"next","description":"Skip to next track.",
          "input_schema":{"type":"object","properties":{"device_id":{"type":"string"}},"additionalProperties":false}
        },
        { "name":"previous","description":"Skip to previous track.",
          "input_schema":{"type":"object","properties":{"device_id":{"type":"string"}},"additionalProperties":false}
        }
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl From<SpotifyError> for ToolError {
    fn from(e: SpotifyError) -> Self {
        Self { code: e.rpc_code(), message: e.message() }
    }
}

fn bad_input(msg: impl Into<String>) -> ToolError {
    ToolError { code: -32602, message: msg.into() }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "now_playing" => now_playing(),
        "search" => search(args),
        "play" => play(args),
        "pause" => control("/me/player/pause", args, Method::PUT),
        "next" => control("/me/player/next", args, Method::POST),
        "previous" => control("/me/player/previous", args, Method::POST),
        other => Err(ToolError { code: -32601, message: format!("unknown tool `{other}`") }),
    }
}

fn status() -> Value {
    json!({
        "ok": client::token_present(),
        "provider": "spotify-web-api",
        "client_version": CLIENT_VERSION,
        "endpoint": client::base_url(),
        "user_agent": client::user_agent(),
        "token_present": client::token_present(),
        "tools": ["status","now_playing","search","play","pause","next","previous"],
        "requires": { "bins": [], "env": ["SPOTIFY_ACCESS_TOKEN"] }
    })
}

fn now_playing() -> Result<Value, ToolError> {
    let resp = client::call(Request {
        method: Method::GET,
        path: "/me/player",
        query: &[],
        body: None,
    })?;
    let v = match resp {
        Some(v) => v,
        None => {
            return Ok(json!({
                "ok": true,
                "is_playing": false,
                "reason": "no active device"
            }))
        }
    };
    let item = v.get("item");
    let track = item.and_then(|i| i.get("name")).and_then(|s| s.as_str()).map(String::from);
    let artists = item
        .and_then(|i| i.get("artists"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let album = item
        .and_then(|i| i.get("album"))
        .and_then(|a| a.get("name"))
        .and_then(|s| s.as_str())
        .map(String::from);
    Ok(json!({
        "ok": true,
        "is_playing": v.get("is_playing").and_then(|b| b.as_bool()).unwrap_or(false),
        "progress_ms": v.get("progress_ms").cloned(),
        "track": track,
        "artists": artists,
        "album": album,
        "uri": item.and_then(|i| i.get("uri")).cloned(),
        "device": v.get("device").cloned(),
    }))
}

fn search(args: &Value) -> Result<Value, ToolError> {
    let query = required_string(args, "query")?;
    let types = optional_string(args, "types").unwrap_or_else(|| "track".to_string());
    let limit = optional_u64(args, "limit")?.unwrap_or(10);
    if !(1..=50).contains(&limit) {
        return Err(bad_input("limit must be 1..=50"));
    }
    validate_types(&types)?;
    let limit_s = limit.to_string();
    let q = vec![
        ("q", query.clone()),
        ("type", types.clone()),
        ("limit", limit_s),
    ];
    let resp = client::call(Request {
        method: Method::GET,
        path: "/search",
        query: &q,
        body: None,
    })?
    .ok_or_else(|| ToolError::from(SpotifyError::InvalidJson("empty search response".into())))?;

    Ok(json!({
        "ok": true,
        "query": query,
        "types": types,
        "results": resp,
    }))
}

fn play(args: &Value) -> Result<Value, ToolError> {
    let device_q: Vec<(&str, String)> = match optional_string(args, "device_id") {
        Some(d) => vec![("device_id", d)],
        None => vec![],
    };
    let body = match optional_string(args, "uri") {
        Some(uri) => {
            validate_spotify_uri(&uri)?;
            if uri.contains(":track:") {
                Some(json!({ "uris": [uri] }))
            } else {
                Some(json!({ "context_uri": uri }))
            }
        }
        None => None,
    };
    client::call(Request {
        method: Method::PUT,
        path: "/me/player/play",
        query: &device_q,
        body,
    })?;
    Ok(json!({ "ok": true, "action": "play" }))
}

fn control(path: &str, args: &Value, method: Method) -> Result<Value, ToolError> {
    let device_q: Vec<(&str, String)> = match optional_string(args, "device_id") {
        Some(d) => vec![("device_id", d)],
        None => vec![],
    };
    client::call(Request {
        method,
        path,
        query: &device_q,
        body: None,
    })?;
    Ok(json!({ "ok": true, "path": path }))
}

fn validate_types(types: &str) -> Result<(), ToolError> {
    for t in types.split(',') {
        match t.trim() {
            "track" | "artist" | "album" | "playlist" | "show" | "episode" | "audiobook" => {}
            other => return Err(bad_input(format!("unknown type `{other}`"))),
        }
    }
    Ok(())
}

fn validate_spotify_uri(uri: &str) -> Result<(), ToolError> {
    if !uri.starts_with("spotify:") {
        return Err(bad_input(format!("not a spotify URI: `{uri}`")));
    }
    let parts: Vec<&str> = uri.split(':').collect();
    if parts.len() < 3 {
        return Err(bad_input(format!("malformed URI: `{uri}`")));
    }
    match parts[1] {
        "track" | "album" | "playlist" | "artist" | "show" | "episode" => Ok(()),
        other => Err(bad_input(format!("unsupported URI kind `{other}`"))),
    }
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
    args.get(key).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| bad_input(format!("`{key}` must be integer"))),
    }
}
