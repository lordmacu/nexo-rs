//! Sample tools shipped with the template. Swap these for your own.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "ping",
            "description": "Returns pong + unix timestamp of receipt",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "add",
            "description": "Returns the sum of numbers a and b",
            "input_schema": {
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["a", "b"],
                "additionalProperties": false
            }
        }
    ])
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "ping" => Ok(json!({
            "pong": true,
            "received_at_unix": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        })),
        "add" => {
            let a = args.get("a").and_then(|v| v.as_f64())
                .ok_or_else(|| "missing or non-numeric `a`".to_string())?;
            let b = args.get("b").and_then(|v| v.as_f64())
                .ok_or_else(|| "missing or non-numeric `b`".to_string())?;
            Ok(json!({ "sum": a + b }))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}
