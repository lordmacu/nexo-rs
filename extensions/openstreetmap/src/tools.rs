use serde_json::{json, Value};

use crate::client::{self, OsmError, SearchParams, CLIENT_VERSION};

const DEFAULT_LIMIT: u32 = 5;
const MAX_LIMIT: u32 = 20;
const DEFAULT_REVERSE_ZOOM: u32 = 18;

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns OSM extension status and Nominatim endpoint info",
            "input_schema": {
                "type": "object",
                "additionalProperties": false
            }
        },
        {
            "name": "search",
            "description": "Forward geocoding via OpenStreetMap Nominatim",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Place query (e.g., 'Museo del Prado, Madrid')"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "description": "Max result count, default 5"
                    },
                    "country_codes": {
                        "type": "string",
                        "description": "Optional comma-separated ISO country codes (e.g. es,pt)"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "reverse",
            "description": "Reverse geocoding (lat/lon to address) via Nominatim",
            "input_schema": {
                "type": "object",
                "properties": {
                    "lat": { "type": "number", "minimum": -90, "maximum": 90 },
                    "lon": { "type": "number", "minimum": -180, "maximum": 180 },
                    "zoom": { "type": "integer", "minimum": 0, "maximum": 18, "description": "Detail level, default 18" }
                },
                "required": ["lat", "lon"],
                "additionalProperties": false
            }
        }
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl From<OsmError> for ToolError {
    fn from(e: OsmError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "search" => search(args),
        "reverse" => reverse(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "openstreetmap-nominatim",
        "endpoint": client::base_url(),
        "client_version": CLIENT_VERSION,
        "user_agent": client::user_agent(),
        "tools": ["status", "search", "reverse"],
        "rate_limit": "1 req/sec (Nominatim usage policy)",
        "requires": { "bins": [], "env": [] }
    })
}

fn search(args: &Value) -> Result<Value, ToolError> {
    let query = required_string(args, "query")?;
    let limit = optional_u32(args, "limit")?.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ToolError {
            code: -32602,
            message: format!("`limit` must be between 1 and {MAX_LIMIT}"),
        });
    }
    let country_codes = optional_string(args, "country_codes")?;
    let hits = client::search(SearchParams {
        query: &query,
        limit,
        country_codes: country_codes.as_deref(),
    })?;

    let results: Vec<Value> = hits
        .into_iter()
        .map(|h| {
            json!({
                "display_name": h.display_name,
                "lat": h.lat,
                "lon": h.lon,
                "class": h.class,
                "type": h.kind,
                "importance": h.importance,
                "boundingbox": h.boundingbox,
            })
        })
        .collect();

    Ok(json!({
        "query": query,
        "limit": limit,
        "country_codes": country_codes,
        "results": results,
    }))
}

fn reverse(args: &Value) -> Result<Value, ToolError> {
    let lat = required_f64(args, "lat")?;
    let lon = required_f64(args, "lon")?;
    let zoom = optional_u32(args, "zoom")?.unwrap_or(DEFAULT_REVERSE_ZOOM);
    let res = client::reverse(lat, lon, zoom)?;
    Ok(json!({
        "lat": lat,
        "lon": lon,
        "zoom": zoom,
        "display_name": res.display_name,
        "resolved": {
            "lat": res.lat,
            "lon": res.lon,
        },
        "address": res.address,
    }))
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let value = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError {
            code: -32602,
            message: format!("missing or invalid `{key}`"),
        })?
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(ToolError {
            code: -32602,
            message: format!("`{key}` cannot be empty"),
        });
    }
    Ok(value)
}

fn optional_string(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.trim().to_string()))
            .ok_or_else(|| ToolError {
                code: -32602,
                message: format!("`{key}` must be a string"),
            }),
    }
}

fn required_f64(args: &Value, key: &str) -> Result<f64, ToolError> {
    args.get(key)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| ToolError {
            code: -32602,
            message: format!("missing or invalid `{key}` (expected number)"),
        })
}

fn optional_u32(args: &Value, key: &str) -> Result<Option<u32>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(n as u32)).ok_or_else(|| ToolError {
            code: -32602,
            message: format!("`{key}` must be a positive integer"),
        }),
    }
}
