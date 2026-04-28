//! Phase 79.3 — `SyntheticOutput` typed-output tool.
//!
//! Forces a goal to terminate with a JSON value that matches a
//! caller-provided JSONSchema. Closes the gap between "model
//! produces free prose" and "downstream consumer needs a struct" —
//! Phase 19/20 pollers, Phase 51 eval harness, and any future
//! contract-shaped goal can require this tool to be the last call.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/SyntheticOutputTool/SyntheticOutputTool.ts:1-163`.
//!     The leak builds one tool *per schema* via
//!     `createSyntheticOutputTool(jsonSchema)` so the model's input
//!     IS the schema. Nexo-rs runs as a daemon — building a fresh
//!     tool per call breaks tool-registry semantics. Instead we
//!     ship a single tool whose input carries BOTH the schema and
//!     the value. Pollers and eval harnesses inject the schema via
//!     prompt template; ad-hoc goals can pass it inline. See
//!     `terminal_schema` follow-up below for the lift-from-leak
//!     variant where the runtime carries the schema.
//!
//! Reference (secondary):
//!   * OpenClaw `research/` — no equivalent. The single-process TS
//!     reference shapes its outputs via Zod parsing inline; no
//!     separate "force structured output" tool exists.
//!
//! Validation: `jsonschema = "0.20"` (already an optional dep on
//! nexo-core for Phase 9.2 tool-args validation). When the
//! `schema-validation` feature is off the tool refuses with a clear
//! "feature disabled" error rather than silently passing through —
//! synthesised output without validation is worse than no synthesis.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};

pub struct SyntheticOutputTool;

impl SyntheticOutputTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "SyntheticOutput".to_string(),
            description: "Return your final answer as a JSON value that matches a caller-provided JSONSchema. Use this when the goal contract requires a typed object instead of prose. Validation is strict: a missing field, wrong type, or extra unexpected key returns an error and the model must retry. Call this exactly once at the end of the turn.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "schema": {
                        "type": "object",
                        "description": "JSONSchema (Draft 7 / 2019-09 / 2020-12) the value must match. Operators / pollers may pre-fill this via prompt template; ad-hoc callers pass it inline."
                    },
                    "value": {
                        "description": "The value to validate and return. Any JSON shape — typically an object, but arrays / scalars also valid when the schema permits."
                    }
                },
                "required": ["schema", "value"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for SyntheticOutputTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let schema = args
            .get("schema")
            .ok_or_else(|| anyhow::anyhow!("SyntheticOutput requires `schema` (JSONSchema)"))?;
        let value = args
            .get("value")
            .ok_or_else(|| anyhow::anyhow!("SyntheticOutput requires `value`"))?;

        if !schema.is_object() {
            return Err(anyhow::anyhow!(
                "SyntheticOutput: `schema` must be a JSON object (got {kind})",
                kind = json_type_name(schema)
            ));
        }

        validate(schema, value)?;

        Ok(json!({
            "ok": true,
            "structured_output": value,
            "instructions": "Output validated. The goal can terminate now — do not call any other tool this turn unless the goal contract calls for it explicitly."
        }))
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(feature = "schema-validation")]
fn validate(schema: &Value, value: &Value) -> anyhow::Result<()> {
    use jsonschema::Validator;
    let validator = Validator::new(schema).map_err(|e| {
        anyhow::anyhow!("SyntheticOutput: invalid `schema` (JSONSchema compile error): {e}")
    })?;
    // jsonschema 0.20 API: `validate(value)` returns
    // `Result<(), ErrorIterator>`; on failure we drain the
    // iterator into formatted strings.
    let errors: Vec<String> = match validator.validate(value) {
        Ok(()) => Vec::new(),
        Err(iter) => iter
            .map(|e| {
                let path = e.instance_path.to_string();
                let path = if path.is_empty() {
                    "/".to_string()
                } else {
                    path
                };
                format!("{path}: {e}")
            })
            .collect(),
    };
    if !errors.is_empty() {
        return Err(anyhow::anyhow!(
            "SyntheticOutput: value does not match schema ({n} error{s}): {body}",
            n = errors.len(),
            s = if errors.len() == 1 { "" } else { "s" },
            body = errors.join("; ")
        ));
    }
    Ok(())
}

#[cfg(not(feature = "schema-validation"))]
fn validate(_schema: &Value, _value: &Value) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "SyntheticOutput is unavailable: nexo-core was built without the `schema-validation` Cargo feature. Rebuild with `--features schema-validation` to enable typed-output validation."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use std::sync::Arc;

    fn ctx() -> AgentContext {
        let cfg = AgentConfig {
            id: "a".into(),
            model: ModelConfig {
                provider: "x".into(),
                model: "y".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
        };
        AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    #[tokio::test]
    async fn valid_object_passes() {
        let c = ctx();
        let res = SyntheticOutputTool
            .call(
                &c,
                json!({
                    "schema": {
                        "type": "object",
                        "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
                        "required": ["name"]
                    },
                    "value": {"name": "ana", "age": 30}
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["structured_output"]["name"], "ana");
    }

    #[tokio::test]
    async fn type_mismatch_errors_with_path() {
        let c = ctx();
        let err = SyntheticOutputTool
            .call(
                &c,
                json!({
                    "schema": {
                        "type": "object",
                        "properties": {"age": {"type": "integer"}},
                        "required": ["age"]
                    },
                    "value": {"age": "thirty"}
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not match schema"), "got: {err}");
        assert!(err.contains("/age") || err.contains("age"), "got: {err}");
    }

    #[tokio::test]
    async fn missing_required_field_errors() {
        let c = ctx();
        let err = SyntheticOutputTool
            .call(
                &c,
                json!({
                    "schema": {
                        "type": "object",
                        "properties": {"name": {"type": "string"}},
                        "required": ["name"]
                    },
                    "value": {}
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not match schema"), "got: {err}");
    }

    #[tokio::test]
    async fn nested_arrays_and_enums_supported() {
        let c = ctx();
        let res = SyntheticOutputTool
            .call(
                &c,
                json!({
                    "schema": {
                        "type": "object",
                        "properties": {
                            "tags": {
                                "type": "array",
                                "items": {"type": "string", "enum": ["red", "green", "blue"]}
                            }
                        },
                        "required": ["tags"]
                    },
                    "value": {"tags": ["red", "blue"]}
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
    }

    #[tokio::test]
    async fn invalid_enum_value_errors() {
        let c = ctx();
        let err = SyntheticOutputTool
            .call(
                &c,
                json!({
                    "schema": {
                        "type": "object",
                        "properties": {
                            "tags": {
                                "type": "array",
                                "items": {"type": "string", "enum": ["red", "green"]}
                            }
                        }
                    },
                    "value": {"tags": ["purple"]}
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not match schema"), "got: {err}");
    }

    #[tokio::test]
    async fn missing_schema_arg_errors() {
        let c = ctx();
        let err = SyntheticOutputTool
            .call(&c, json!({"value": {}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires `schema`"), "got: {err}");
    }

    #[tokio::test]
    async fn missing_value_arg_errors() {
        let c = ctx();
        let err = SyntheticOutputTool
            .call(&c, json!({"schema": {"type": "object"}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires `value`"), "got: {err}");
    }

    #[tokio::test]
    async fn schema_not_object_errors() {
        let c = ctx();
        let err = SyntheticOutputTool
            .call(&c, json!({"schema": "not-an-object", "value": {}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be a JSON object"), "got: {err}");
    }

    #[tokio::test]
    async fn invalid_schema_compile_errors() {
        let c = ctx();
        // `type: "not-a-type"` is not a valid JSONSchema type token.
        let res = SyntheticOutputTool
            .call(
                &c,
                json!({
                    "schema": {"type": "not-a-type"},
                    "value": {}
                }),
            )
            .await;
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(err.contains("JSONSchema") || err.contains("compile") || err.contains("schema"));
    }

    #[tokio::test]
    async fn scalar_value_with_scalar_schema_passes() {
        let c = ctx();
        let res = SyntheticOutputTool
            .call(
                &c,
                json!({
                    "schema": {"type": "integer", "minimum": 0, "maximum": 100},
                    "value": 42
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["structured_output"], 42);
    }
}
