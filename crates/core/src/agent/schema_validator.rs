//! Phase 9.2 follow-up — JSON Schema validation of `tools/call`
//! arguments before handler dispatch. Gated behind the
//! `schema-validation` feature so the `jsonschema` dep is optional.
//!
//! When the feature is off the validator is a no-op; when on, callers
//! get detailed error messages with JSON pointer paths that the LLM can
//! use to fix its next attempt.
use dashmap::DashMap;
#[cfg(feature = "schema-validation")]
use jsonschema::Validator;
use nexo_llm::ToolDef;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
pub struct ToolArgsValidator {
    #[cfg(feature = "schema-validation")]
    cache: DashMap<u64, Arc<Validator>>,
    enabled: bool,
}
impl ToolArgsValidator {
    pub fn new(enabled: bool) -> Self {
        // If the operator asked for validation but the binary was
        // built without the `schema-validation` Cargo feature, calls
        // would silently fail open. Log once so the mismatch is
        // visible on startup — the caller only invokes `new` once per
        // agent.
        #[cfg(not(feature = "schema-validation"))]
        if enabled {
            tracing::warn!(
                "tool_args_validation.enabled=true but binary built without `schema-validation` feature — args are not being validated"
            );
        }
        Self {
            #[cfg(feature = "schema-validation")]
            cache: DashMap::new(),
            enabled,
        }
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    /// Validate `args` against `def.parameters`. Returns `Ok(())` when
    /// valid, disabled, or the schema is unsuitable for validation
    /// (non-object/empty); `Err(errors)` when args violate the schema.
    pub fn validate(&self, def: &ToolDef, args: &Value) -> Result<(), Vec<String>> {
        if !self.enabled {
            return Ok(());
        }
        // Only validate when parameters looks like a real JSON Schema
        // object. Empty params / non-object types are silently allowed
        // — legacy tools may skip the declaration entirely.
        if !def.parameters.is_object() {
            return Ok(());
        }
        let Some(obj) = def.parameters.as_object() else {
            return Ok(());
        };
        if obj.is_empty() {
            return Ok(());
        }
        #[cfg(feature = "schema-validation")]
        {
            let fingerprint = schema_fingerprint(&def.parameters);
            let validator = match self.cache.get(&fingerprint) {
                Some(v) => v.value().clone(),
                None => {
                    let compiled = match Validator::new(&def.parameters) {
                        Ok(v) => v,
                        Err(e) => {
                            // Fail open: bug in the schema shouldn't block
                            // the tool forever; log and let the call through.
                            tracing::warn!(
                                tool = %def.name,
                                error = %e,
                                "jsonschema compile failed; skipping validation"
                            );
                            return Ok(());
                        }
                    };
                    let arc = Arc::new(compiled);
                    self.cache.insert(fingerprint, arc.clone());
                    arc
                }
            };
            let msgs: Vec<String> = match validator.validate(args) {
                Ok(()) => Vec::new(),
                Err(errors) => errors
                    .map(|e| format!("at {}: {}", e.instance_path, e))
                    .collect(),
            };
            if msgs.is_empty() {
                Ok(())
            } else {
                Err(msgs)
            }
        }
        #[cfg(not(feature = "schema-validation"))]
        {
            let _ = args;
            Ok(())
        }
    }
}
fn schema_fingerprint(schema: &Value) -> u64 {
    let bytes = serde_json::to_vec(schema).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn tool(name: &str, params: Value) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: String::new(),
            parameters: params,
        }
    }
    #[test]
    fn validates_required_field() {
        let v = ToolArgsValidator::new(true);
        let def = tool(
            "search",
            json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
        );
        v.validate(&def, &json!({"query": "rust"})).unwrap();
        let err = v.validate(&def, &json!({})).unwrap_err();
        assert!(err.iter().any(|e| e.contains("query")), "{err:?}");
    }
    #[cfg(feature = "schema-validation")]
    #[test]
    fn rejects_wrong_type() {
        let v = ToolArgsValidator::new(true);
        let def = tool(
            "connect",
            json!({
                "type": "object",
                "properties": {"port": {"type": "integer"}}
            }),
        );
        let err = v.validate(&def, &json!({"port": "80"})).unwrap_err();
        assert!(err.iter().any(|e| e.contains("/port")), "{err:?}");
    }
    #[test]
    fn empty_schema_is_skipped() {
        let v = ToolArgsValidator::new(true);
        let def = tool("nothing", json!({}));
        v.validate(&def, &json!({"anything": 1})).unwrap();
    }
    #[test]
    fn non_object_schema_is_skipped() {
        let v = ToolArgsValidator::new(true);
        let def = tool("odd", json!(null));
        v.validate(&def, &json!({})).unwrap();
    }
    #[test]
    fn disabled_never_errors() {
        let v = ToolArgsValidator::new(false);
        let def = tool(
            "x",
            json!({"type":"object","required":["q"],"properties":{"q":{"type":"string"}}}),
        );
        v.validate(&def, &json!({})).unwrap();
    }
    #[cfg(feature = "schema-validation")]
    #[test]
    fn cache_reuses_compiled_validator() {
        let v = ToolArgsValidator::new(true);
        let def = tool(
            "search",
            json!({
                "type": "object",
                "properties": {"q": {"type": "string"}},
                "required": ["q"]
            }),
        );
        for _ in 0..3 {
            v.validate(&def, &json!({"q": "x"})).unwrap();
        }
        assert_eq!(v.cache.len(), 1);
    }
}
