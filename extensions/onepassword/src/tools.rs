use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit::{self, AuditEntry};
use crate::op_cli::{
    self, timeout_from_env, OpError, DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS,
};
use crate::redact;

const DEFAULT_INJECT_STDOUT_CAP: usize = 4096;
const MAX_INJECT_STDOUT_CAP: usize = 16384;

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
        },
        {
            "name": "inject_template",
            "description": "Render a template containing op:// references and either return it (template-only mode) or pipe it as stdin to an allowlisted command. The rendered template is never returned when `command` is set — the LLM only sees exit_code and (redacted) stdout/stderr.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "template": {
                        "type": "string",
                        "description": "Template body. Embed secrets as {{ op://Vault/Item/field }}."
                    },
                    "command": {
                        "type": "string",
                        "description": "Optional executable. Must appear in OP_INJECT_COMMAND_ALLOWLIST."
                    },
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Arguments passed to `command`."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "Validate references and return the list without resolving values."
                    },
                    "max_stdout_bytes": {
                        "type": "integer",
                        "minimum": 256,
                        "maximum": MAX_INJECT_STDOUT_CAP,
                        "description": "Cap on returned stdout bytes (default 4096, max 16384)."
                    }
                },
                "required": ["template"],
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
        "inject_template" => inject_template(args),
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
        "tools": ["status", "whoami", "list_vaults", "list_items", "read_secret", "inject_template"],
        "inject_command_allowlist": op_cli::inject_command_allowlist(),
        "inject_timeout_secs": op_cli::inject_timeout_secs(),
        "audit_log_path": audit::audit_log_path().display().to_string(),
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
    let (vault, item, field) = match op_cli::validate_ref(&reference) {
        Ok(v) => v,
        Err(e) => {
            audit::append(AuditEntry {
                action: "read_secret",
                fields: json!({
                    "op_reference": reference,
                    "ok": false,
                    "error": "invalid_reference",
                }),
            });
            return Err(ToolError::from(e));
        }
    };

    let raw = match op_cli::run(&["read", &reference], timeout_from_env()) {
        Ok(s) => s,
        Err(e) => {
            audit::append(AuditEntry {
                action: "read_secret",
                fields: json!({
                    "op_reference": reference,
                    "ok": false,
                    "error": e.message(),
                }),
            });
            return Err(ToolError::from(e));
        }
    };
    let trimmed = raw.trim_end_matches('\n').to_string();
    let length = trimmed.len();
    let fingerprint = sha256_prefix(&trimmed);
    let reveal = op_cli::reveal_allowed();

    audit::append(AuditEntry {
        action: "read_secret",
        fields: json!({
            "op_reference": reference,
            "fingerprint_sha256_prefix": fingerprint,
            "reveal_allowed": reveal,
            "ok": true,
        }),
    });

    let mut out = json!({
        "ok": true,
        "reference": reference,
        "vault": vault,
        "item": item,
        "field": field,
        "length": length,
        "fingerprint_sha256_prefix": fingerprint,
    });

    if reveal {
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

fn inject_template(args: &Value) -> Result<Value, ToolError> {
    let template = required_string(args, "template")?;
    let command = optional_string(args, "command");
    let extra_args: Vec<String> = args
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let dry_run = args.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);
    let cap = args
        .get("max_stdout_bytes")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).min(MAX_INJECT_STDOUT_CAP).max(256))
        .unwrap_or(DEFAULT_INJECT_STDOUT_CAP);

    let references = op_cli::extract_template_references(&template);

    if dry_run {
        // Validate each reference's shape; we do not resolve values.
        let mut bad: Vec<String> = Vec::new();
        for r in &references {
            if op_cli::validate_ref(r).is_err() {
                bad.push(r.clone());
            }
        }
        let ok = bad.is_empty();
        audit::append(AuditEntry {
            action: "inject_template",
            fields: json!({
                "references": references,
                "command": command,
                "dry_run": true,
                "ok": ok,
                "error": if ok { Value::Null } else { json!("invalid_reference") },
            }),
        });
        if !ok {
            return Err(bad_input(format!("invalid op:// references: {bad:?}")));
        }
        return Ok(json!({
            "ok": true,
            "dry_run": true,
            "references_validated": references,
        }));
    }

    if let Some(cmd) = command.as_deref() {
        let allowlist = op_cli::inject_command_allowlist();
        if !allowlist.iter().any(|c| c == cmd) {
            audit::append(AuditEntry {
                action: "inject_template",
                fields: json!({
                    "references": references,
                    "command": cmd,
                    "args_count": extra_args.len(),
                    "dry_run": false,
                    "ok": false,
                    "error": "command_not_in_allowlist",
                }),
            });
            return Err(bad_input(format!(
                "command `{cmd}` is not in OP_INJECT_COMMAND_ALLOWLIST"
            )));
        }
        let timeout = op_cli::inject_timeout_secs();
        let result = op_cli::run_inject_with_command(&template, cmd, &extra_args, cap, timeout);
        match result {
            Ok(r) => {
                let stdout = redact::redact(&r.stdout);
                let stderr = redact::redact(&r.stderr);
                audit::append(AuditEntry {
                    action: "inject_template",
                    fields: json!({
                        "references": references,
                        "command": cmd,
                        "args_count": extra_args.len(),
                        "dry_run": false,
                        "ok": true,
                        "exit_code": r.exit_code,
                        "stdout_total_bytes": r.stdout_total_bytes,
                        "stdout_returned_bytes": r.stdout_returned_bytes,
                        "stdout_truncated": r.stdout_truncated,
                    }),
                });
                Ok(json!({
                    "ok": true,
                    "exit_code": r.exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                    "stdout_truncated": r.stdout_truncated,
                    "stdout_total_bytes": r.stdout_total_bytes,
                    "stdout_returned_bytes": r.stdout_returned_bytes,
                }))
            }
            Err(e) => {
                audit::append(AuditEntry {
                    action: "inject_template",
                    fields: json!({
                        "references": references,
                        "command": cmd,
                        "args_count": extra_args.len(),
                        "dry_run": false,
                        "ok": false,
                        "error": e.message(),
                    }),
                });
                Err(ToolError::from(e))
            }
        }
    } else {
        // Template-only mode.
        let timeout = op_cli::inject_timeout_secs();
        let rendered = match op_cli::run_inject_template_only(&template, timeout) {
            Ok(s) => s,
            Err(e) => {
                audit::append(AuditEntry {
                    action: "inject_template",
                    fields: json!({
                        "references": references,
                        "command": Value::Null,
                        "dry_run": false,
                        "ok": false,
                        "error": e.message(),
                    }),
                });
                return Err(ToolError::from(e));
            }
        };
        let length = rendered.len();
        let fingerprint = sha256_prefix(&rendered);
        let reveal = op_cli::reveal_allowed();
        audit::append(AuditEntry {
            action: "inject_template",
            fields: json!({
                "references": references,
                "command": Value::Null,
                "dry_run": false,
                "ok": true,
                "fingerprint_sha256_prefix": fingerprint,
                "reveal_allowed": reveal,
                "rendered_length": length,
            }),
        });
        let mut out = json!({
            "ok": true,
            "references": references,
            "rendered_length": length,
            "fingerprint_sha256_prefix": fingerprint,
        });
        if reveal {
            out["rendered"] = Value::String(rendered);
            out["reveal"] = Value::Bool(true);
        } else {
            out["reveal"] = Value::Bool(false);
            out["reveal_hint"] = Value::String(
                "set OP_ALLOW_REVEAL=true on the agent process to include the rendered template"
                    .to_string(),
            );
        }
        Ok(out)
    }
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
