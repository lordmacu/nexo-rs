use serde_json::{json, Value};

use crate::cache::GeoEntry;
use crate::client::{self, Units, WeatherError, CLIENT_VERSION};
use crate::wmo::weather_desc;

const DEFAULT_FORECAST_DAYS: u32 = 3;
const MAX_FORECAST_DAYS: u32 = 16;

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns weather extension defaults and provider info",
            "input_schema": {
                "type": "object",
                "additionalProperties": false
            }
        },
        {
            "name": "current",
            "description": "Returns current weather for a location using Open-Meteo",
            "input_schema": {
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "City, region, or place name (e.g., Madrid, New York)"
                    },
                    "units": {
                        "type": "string",
                        "enum": ["metric", "imperial"],
                        "description": "Unit system, default: metric"
                    }
                },
                "required": ["location"],
                "additionalProperties": false
            }
        },
        {
            "name": "forecast",
            "description": "Returns a daily forecast (up to 16 days) for a location using Open-Meteo",
            "input_schema": {
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "City, region, or place name"
                    },
                    "days": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 16,
                        "description": "Forecast days, default 3"
                    },
                    "units": {
                        "type": "string",
                        "enum": ["metric", "imperial"],
                        "description": "Unit system, default: metric"
                    }
                },
                "required": ["location"],
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

impl From<WeatherError> for ToolError {
    fn from(e: WeatherError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "current" => current(args),
        "forecast" => forecast(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    json!({
        "ok": true,
        "provider": "open-meteo",
        "endpoints": {
            "geocoding": client::geocoding_endpoint(),
            "forecast": client::forecast_endpoint(),
        },
        "client_version": CLIENT_VERSION,
        "user_agent": client::user_agent(),
        "tools": ["status", "current", "forecast"],
        "requires": { "bins": [], "env": [] }
    })
}

fn current(args: &Value) -> Result<Value, ToolError> {
    let location = required_string(args, "location")?;
    let units = parse_units(args)?;
    let geo = client::geocode(&location)?;
    let payload = client::forecast(geo.lat, geo.lon, 1, &units)?;

    let cur = payload
        .get("current")
        .cloned()
        .ok_or_else(|| ToolError {
            code: -32006,
            message: "provider response missing `current`".into(),
        })?;
    let observed_at = cur
        .get("time")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let code = cur.get("weather_code").and_then(|v| v.as_u64()).unwrap_or(0) as u16;

    Ok(json!({
        "location_query": location,
        "units": units.label(),
        "resolved": resolved_block(&geo),
        "current": {
            "temperature": cur.get("temperature_2m"),
            "feels_like": cur.get("apparent_temperature"),
            "humidity_pct": cur.get("relative_humidity_2m"),
            "wind_speed": cur.get("wind_speed_10m"),
            "wind_dir_deg": cur.get("wind_direction_10m"),
            "precipitation": cur.get("precipitation"),
            "weather_code": code,
            "weather_desc": weather_desc(code),
            "is_day": cur.get("is_day").and_then(|v| v.as_u64()).map(|n| n == 1),
            "observed_at": observed_at,
        }
    }))
}

fn forecast(args: &Value) -> Result<Value, ToolError> {
    let location = required_string(args, "location")?;
    let units = parse_units(args)?;
    let days = optional_u32(args, "days")?.unwrap_or(DEFAULT_FORECAST_DAYS);
    if days == 0 || days > MAX_FORECAST_DAYS {
        return Err(ToolError {
            code: -32602,
            message: format!("`days` must be between 1 and {MAX_FORECAST_DAYS}"),
        });
    }
    let geo = client::geocode(&location)?;
    let payload = client::forecast(geo.lat, geo.lon, days, &units)?;

    let daily = payload
        .get("daily")
        .cloned()
        .ok_or_else(|| ToolError {
            code: -32006,
            message: "provider response missing `daily`".into(),
        })?;

    let times = daily.get("time").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mins = daily
        .get("temperature_2m_min")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let maxs = daily
        .get("temperature_2m_max")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let precs = daily
        .get("precipitation_sum")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let winds = daily
        .get("wind_speed_10m_max")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let codes = daily
        .get("weather_code")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let n = times.len();
    let mut items = Vec::with_capacity(n);
    for i in 0..n {
        let code = codes.get(i).and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        items.push(json!({
            "date": times.get(i),
            "min": mins.get(i),
            "max": maxs.get(i),
            "precipitation_sum": precs.get(i),
            "wind_max": winds.get(i),
            "weather_code": code,
            "weather_desc": weather_desc(code),
        }));
    }

    Ok(json!({
        "location_query": location,
        "units": units.label(),
        "days": days,
        "resolved": resolved_block(&geo),
        "forecast": items,
    }))
}

fn resolved_block(geo: &GeoEntry) -> Value {
    json!({
        "name": geo.name,
        "country": geo.country,
        "timezone": geo.timezone,
        "lat": geo.lat,
        "lon": geo.lon,
    })
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

fn optional_u32(args: &Value, key: &str) -> Result<Option<u32>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(n as u32)).ok_or_else(|| ToolError {
            code: -32602,
            message: format!("`{key}` must be a positive integer"),
        }),
    }
}

fn parse_units(args: &Value) -> Result<Units, ToolError> {
    match args.get("units").and_then(|v| v.as_str()) {
        None => Ok(Units::Metric),
        Some("metric") => Ok(Units::Metric),
        Some("imperial") => Ok(Units::Imperial),
        Some(other) => Err(ToolError {
            code: -32602,
            message: format!("`units` must be 'metric' or 'imperial', got `{other}`"),
        }),
    }
}
