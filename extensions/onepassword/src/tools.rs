use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::op_cli::{
    self, timeout_from_env, OpError, DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS,
};

pub const CLIENT_VERSION: &str = "onepassword-0.1.0";

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns 1Password extension info: op binary presence, service-token presence, reveal policy, limits.",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "whoami",
            "description": "Runs `op whoami` to verify the service-account token is valid.",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "list_vaults",
            "description": "Returns every vault the service account can read.",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "list_items",
            "description": "List items in a vault. Returns titles + categories only, never secret fields.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "vault": { "type": "string", "description": "Vault name or id" }
                },
                "required": ["vault"],
                "additionalProperties": false
            }
        },
        {
            "name": "read_secret",
            "description": "Read a secret via `op://Vault/Item/field`. By default returns only a fingerprint (sha256 prefix) and length — set OP_ALLOW_REVEAL=true on the agent process to include the actual value.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "reference": {
                        "type": "string",
                        "description": "Strict `op://Vault/Item/field` reference. Wildcards rejected."
                    }
                },
                "required": ["reference"],
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

impl From<OpError> for ToolError {
    fn from(e: OpError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

fn bad_input(msg: impl Into<String>) -> ToolError {
    ToolError {
        code: -32602,
        message: msg.into(),
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "whoami" => whoami(),
        "list_vaults" => list_vaults(),
        "list_items" => list_items(args),
        "read_secret" => read_secret(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    let bin = op_cli::bin_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("missing: {}", e.message()));
    let token_present = op_cli::service_token().is_ok();
    let reveal_allowed = op_cli::reveal_allowed();
    json!({
        "ok": token_present,
        "provider": "1password (op CLI)",
        "client_version": CLIENT_VERSION,
        "bin": bin,
        "token_present": token_present,
        "reveal_allowed": reveal_allowed,
        "tools": ["status", "whoami", "list_vaults", "list_items", "read_secret"],
        "limits": {
            "default_timeout_secs": DEFAULT_TIMEOUT_SECS,
            "max_timeout_secs": MAX_TIMEOUT_SECS
        },
        "requires": { "bins": ["op"], "env": ["OP_SERVICE_ACCOUNT_TOKEN"] }
    })
}

fn whoami() -> Result<Value, ToolError> {
    let stdout = op_cli::run(&["whoami", "--format", "json"], timeout_from_env())?;
    let parsed: Value = serde_json::from_str(&stdout)
        .map_err(|e| ToolError::from(OpError::JsonError(e.to_string())))?;
    Ok(json!({ "ok": true, "whoami": parsed }))
}

fn list_vaults() -> Result<Value, ToolError> {
    let stdout = op_cli::run(&["vault", "list", "--format", "json"], timeout_from_env())?;
    let parsed: Value = serde_json::from_str(&stdout)
        .map_err(|e| ToolError::from(OpError::JsonError(e.to_string())))?;
    let summary = parsed
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| {
                    json!({
                        "id": v.get("id").and_then(|s| s.as_str()),
                        "name": v.get("name").and_then(|s| s.as_str()),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(json!({ "ok": true, "count": summary.len(), "vaults": summary }))
}

fn list_items(args: &Value) -> Result<Value, ToolError> {
    let vault = required_string(args, "vault")?;
    let stdout = op_cli::run(
        &["item", "list", "--vault", &vault, "--format", "json"],
        timeout_from_env(),
    )?;
    let parsed: Value = serde_json::from_str(&stdout)
        .map_err(|e| ToolError::from(OpError::JsonError(e.to_string())))?;
    // Strip anything that smells like a field value — we intentionally only
    // expose titles + metadata at list time.
    let items = parsed
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| {
                    json!({
                        "id": v.get("id").and_then(|s| s.as_str()),
                        "title": v.get("title").and_then(|s| s.as_str()),
                        "category": v.get("category").and_then(|s| s.as_str()),
                        "vault": v.get("vault").cloned(),
                        "tags": v.get("tags").cloned(),
                        "updated_at": v.get("updated_at").cloned(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(json!({ "ok": true, "vault": vault, "count": items.len(), "items": items }))
}

fn read_secret(args: &Value) -> Result<Value, ToolError> {
    let reference = required_string(args, "reference")?;
    let (vault, item, field) = op_cli::validate_ref(&reference).map_err(ToolError::from)?;

    // `op read` returns the raw value on stdout. We never log or print it;
    // it only flows through `stdout` -> String in memory.
    let raw = op_cli::run(&["read", &reference], timeout_from_env())?;
    let trimmed = raw.trim_end_matches('\n').to_string();
    let length = trimmed.len();
    let fingerprint = sha256_prefix(&trimmed);

    let mut out = json!({
        "ok": true,
        "reference": reference,
        "vault": vault,
        "item": item,
        "field": field,
        "length": length,
        "fingerprint_sha256_prefix": fingerprint,
    });

    if op_cli::reveal_allowed() {
        out["value"] = Value::String(trimmed);
        out["reveal"] = Value::Bool(true);
    } else {
        out["reveal"] = Value::Bool(false);
        out["reveal_hint"] = Value::String(
            "set OP_ALLOW_REVEAL=true on the agent process to include the secret value"
                .to_string(),
        );
    }

    Ok(out)
}

fn sha256_prefix(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    // First 8 bytes hex = 16 chars. Enough to verify identity without leaking the secret.
    hex::encode(&digest[..8])
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad_input(format!("missing or invalid `{key}`")))?
        .trim()
        .to_string();
    if s.is_empty() {
        return Err(bad_input(format!("`{key}` cannot be empty")));
    }
    Ok(s)
}
