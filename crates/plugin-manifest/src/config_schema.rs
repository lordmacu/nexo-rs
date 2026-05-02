//! Phase 83.17 — install-time config schema validation.
//!
//! A microapp ships `extensions/<id>/config.schema.json`
//! alongside `plugin.toml`. At install (or boot pre-flight)
//! the daemon validates the operator-supplied
//! `extensions_config.<id>` block against that schema. A bad
//! config aborts install with a structured error pointing at
//! the offending field — instead of failing at runtime inside
//! the microapp.
//!
//! This module ships a lightweight validator that handles the
//! JSON Schema subset every microapp needs:
//! - `type` (`"object" | "string" | "number" | "integer" |
//!   "boolean" | "array"`)
//! - `required` (list of field names on an object)
//! - `properties` (per-field schemas on an object)
//! - `additionalProperties: false` (reject unknown keys)
//! - `enum` (whitelisted values)
//!
//! Out of scope (deferred to 83.17.b if needed):
//! - `$ref` / `$defs`
//! - `oneOf` / `anyOf` / `allOf`
//! - format / pattern validators
//! - numeric `minimum` / `maximum`
//!
//! For richer validation a microapp can hand-roll its own
//! checks inside `initialize` or pull in `jsonschema` from
//! crates.io.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One validation failure. Multiple failures collected per
/// validation pass so the operator sees ALL the bad fields, not
/// just the first one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSchemaError {
    /// JSON Pointer to the offending field (e.g. `/regional`,
    /// `/limits/max_per_day`).
    pub pointer: String,
    /// Operator-readable explanation.
    pub message: String,
}

impl ConfigSchemaError {
    fn new(pointer: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            pointer: pointer.into(),
            message: message.into(),
        }
    }
}

/// Validate `config` against `schema`. Returns the full list of
/// failures (empty `Vec` = pass). Caller decides whether to
/// abort install or render the list to the operator.
///
/// `config` is the operator-supplied YAML / JSON block (already
/// parsed to `serde_json::Value` by the caller — typically
/// `serde_json::to_value(yaml_value)`).
pub fn validate_config(config: &Value, schema: &Value) -> Vec<ConfigSchemaError> {
    let mut errors = Vec::new();
    validate_at(config, schema, "", &mut errors);
    errors
}

fn validate_at(
    value: &Value,
    schema: &Value,
    pointer: &str,
    errors: &mut Vec<ConfigSchemaError>,
) {
    let Some(schema_obj) = schema.as_object() else {
        return;
    };

    if let Some(expected_type) = schema_obj.get("type").and_then(|v| v.as_str()) {
        if !type_matches(value, expected_type) {
            errors.push(ConfigSchemaError::new(
                if pointer.is_empty() { "/" } else { pointer },
                format!(
                    "expected type `{expected_type}`, got `{}`",
                    json_type_name(value)
                ),
            ));
            return;
        }
    }

    // Enum check — applies to any value type.
    if let Some(allowed) = schema_obj.get("enum").and_then(|v| v.as_array()) {
        let in_set = allowed.iter().any(|a| a == value);
        if !in_set {
            errors.push(ConfigSchemaError::new(
                if pointer.is_empty() { "/" } else { pointer },
                format!(
                    "value not in enum (allowed: {})",
                    serde_json::to_string(allowed).unwrap_or_default()
                ),
            ));
        }
    }

    // Object-specific checks.
    if let (Some(map), Some("object")) = (
        value.as_object(),
        schema_obj.get("type").and_then(|v| v.as_str()),
    ) {
        if let Some(required) =
            schema_obj.get("required").and_then(|v| v.as_array())
        {
            for r in required {
                if let Some(name) = r.as_str() {
                    if !map.contains_key(name) {
                        errors.push(ConfigSchemaError::new(
                            format!("{pointer}/{name}"),
                            format!("required field `{name}` is missing"),
                        ));
                    }
                }
            }
        }

        let properties = schema_obj.get("properties").and_then(|v| v.as_object());
        let allow_extra = schema_obj
            .get("additionalProperties")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        for (k, v) in map {
            let child_ptr = format!("{pointer}/{k}");
            match properties.and_then(|p| p.get(k)) {
                Some(child_schema) => {
                    validate_at(v, child_schema, &child_ptr, errors);
                }
                None => {
                    if !allow_extra {
                        errors.push(ConfigSchemaError::new(
                            child_ptr,
                            format!(
                                "unknown field `{k}` (additionalProperties: false)"
                            ),
                        ));
                    }
                }
            }
        }
    }

    // Array item validation — common enough to support.
    if let (Some(arr), Some("array")) = (
        value.as_array(),
        schema_obj.get("type").and_then(|v| v.as_str()),
    ) {
        if let Some(items_schema) = schema_obj.get("items") {
            for (i, item) in arr.iter().enumerate() {
                let child_ptr = format!("{pointer}/{i}");
                validate_at(item, items_schema, &child_ptr, errors);
            }
        }
    }
}

fn type_matches(value: &Value, expected: &str) -> bool {
    match (expected, value) {
        ("object", Value::Object(_)) => true,
        ("array", Value::Array(_)) => true,
        ("string", Value::String(_)) => true,
        ("boolean", Value::Bool(_)) => true,
        ("null", Value::Null) => true,
        ("number", Value::Number(_)) => true,
        ("integer", Value::Number(n)) => n.is_i64() || n.is_u64(),
        _ => false,
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Object(_) => "object",
        Value::Array(_) => "array",
        Value::String(_) => "string",
        Value::Bool(_) => "boolean",
        Value::Null => "null",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
    }
}

/// Operator-side opt-out env var name. Set
/// `NEXO_MICROAPP_SKIP_SCHEMA=<microapp_id>` (comma-separated)
/// to bypass validation for one-off debugging.
pub const SKIP_SCHEMA_ENV: &str = "NEXO_MICROAPP_SKIP_SCHEMA";

/// `true` when the env var lists `microapp_id`. Comma-separated;
/// trims whitespace; case-sensitive.
pub fn is_validation_bypassed(microapp_id: &str, env_value: Option<&str>) -> bool {
    let Some(v) = env_value else {
        return false;
    };
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|s| s == microapp_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_config_returns_empty_errors() {
        let schema = json!({
            "type": "object",
            "required": ["regional"],
            "properties": {
                "regional": { "type": "string" },
                "asesor_phone": { "type": "string" },
            },
            "additionalProperties": false
        });
        let config = json!({
            "regional": "bogota",
            "asesor_phone": "573115728852"
        });
        assert!(validate_config(&config, &schema).is_empty());
    }

    #[test]
    fn missing_required_field_reports_pointer() {
        let schema = json!({
            "type": "object",
            "required": ["regional"],
            "properties": { "regional": { "type": "string" } }
        });
        let config = json!({});
        let errors = validate_config(&config, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pointer, "/regional");
        assert!(errors[0].message.contains("required"));
    }

    #[test]
    fn extra_unknown_field_rejected_when_additional_properties_false() {
        let schema = json!({
            "type": "object",
            "properties": { "regional": { "type": "string" } },
            "additionalProperties": false
        });
        let config = json!({
            "regional": "bogota",
            "wat": "extra"
        });
        let errors = validate_config(&config, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pointer, "/wat");
        assert!(errors[0].message.contains("unknown"));
    }

    #[test]
    fn extra_field_allowed_when_additional_properties_default() {
        // Default JSON Schema behaviour: additionalProperties =
        // true, so extras pass.
        let schema = json!({
            "type": "object",
            "properties": { "regional": { "type": "string" } }
        });
        let config = json!({ "regional": "bogota", "extra": 42 });
        assert!(validate_config(&config, &schema).is_empty());
    }

    #[test]
    fn type_mismatch_reports_clearly() {
        let schema = json!({
            "type": "object",
            "properties": { "max_per_day": { "type": "integer" } }
        });
        let config = json!({ "max_per_day": "twenty" });
        let errors = validate_config(&config, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pointer, "/max_per_day");
        assert!(errors[0].message.contains("integer"));
        assert!(errors[0].message.contains("string"));
    }

    #[test]
    fn enum_constraint_rejects_unlisted() {
        let schema = json!({
            "type": "object",
            "properties": {
                "regional": {
                    "type": "string",
                    "enum": ["bogota", "cali", "medellin"]
                }
            }
        });
        let bad = json!({ "regional": "barranquilla" });
        let errors = validate_config(&bad, &schema);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("enum"));

        let good = json!({ "regional": "cali" });
        assert!(validate_config(&good, &schema).is_empty());
    }

    #[test]
    fn nested_object_validation_threads_pointer() {
        let schema = json!({
            "type": "object",
            "properties": {
                "limits": {
                    "type": "object",
                    "required": ["max_per_day"],
                    "properties": {
                        "max_per_day": { "type": "integer" }
                    }
                }
            }
        });
        let config = json!({
            "limits": { "max_per_day": "many" }
        });
        let errors = validate_config(&config, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pointer, "/limits/max_per_day");
    }

    #[test]
    fn array_items_validation_walks_indices() {
        let schema = json!({
            "type": "object",
            "properties": {
                "phones": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            }
        });
        let config = json!({
            "phones": ["+57311", 12345, "+57312"]
        });
        let errors = validate_config(&config, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pointer, "/phones/1");
    }

    #[test]
    fn multiple_errors_collected_in_one_pass() {
        // Operator sees ALL bad fields, not just the first.
        let schema = json!({
            "type": "object",
            "required": ["a", "b"],
            "properties": {
                "a": { "type": "string" },
                "b": { "type": "integer" }
            },
            "additionalProperties": false
        });
        let config = json!({
            "a": 1,        // wrong type
            // b missing
            "z": "extra"   // unknown
        });
        let errors = validate_config(&config, &schema);
        assert!(errors.len() >= 3, "expected ≥ 3 errors, got {errors:?}");
    }

    #[test]
    fn skip_schema_env_honored() {
        assert!(is_validation_bypassed(
            "agent-creator",
            Some("agent-creator")
        ));
        assert!(is_validation_bypassed(
            "agent-creator",
            Some("other, agent-creator , third")
        ));
        assert!(!is_validation_bypassed("agent-creator", Some("other")));
        assert!(!is_validation_bypassed("agent-creator", None));
        assert!(!is_validation_bypassed("agent-creator", Some("")));
    }

    #[test]
    fn skip_schema_env_constant_is_stable() {
        assert_eq!(SKIP_SCHEMA_ENV, "NEXO_MICROAPP_SKIP_SCHEMA");
    }
}
