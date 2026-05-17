//! `ExtensionPoller` — legacy stdio JSON-RPC bridge for `Poller`
//! implementations written in other languages.
//!
//! **Deprecated post Phase 96** (`nexo-poller-ext 0.2.0`): out-of-tree
//! pollers should ship as plugin v2 subprocess plugins declaring the
//! `[plugin.poller]` manifest section, using the
//! `nexo-microapp-sdk::poller::PollerPluginAdapter` helper. This crate
//! is preserved one release cycle for migration.
//!
//! ## Wire protocol (V2)
//!
//! The extension MUST handle one method:
//!
//! ```text
//! method: poll_tick
//! params: {
//!   "kind":    "<the kind string this tick targets>",
//!   "job_id":  "<job id>",
//!   "agent_id":"<agent id>",
//!   "cursor":  null | "<base64 url-safe string>",
//!   "config":  <opaque JSON value — the job's config: block>,
//!   "now":     "<RFC3339 timestamp>"
//! }
//!
//! result: {
//!   "next_cursor":         null | "<base64 url-safe string>",
//!   "next_interval_secs":  null | <u64>,
//!   "metrics": {
//!     "items_seen":       <u32>,
//!     "items_dispatched": <u32>
//!   } | null
//! }
//! ```
//!
//! Pollers are responsible for their own outbound — the runner no
//! longer translates a `deliver[]` payload into channel topics.
//!
//! Errors must use a JSON-RPC error response with `code`:
//! - `-32001` for `Transient` (network blip, 5xx)
//! - `-32002` for `Permanent` (token revoked, scope changed)
//! - `-32602` for `Config` (validation failure)

#![allow(deprecated)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use nexo_extensions::StdioRuntime;
use nexo_poller::{PollContext, Poller, PollerError, TickAck, TickMetrics};
use serde::Deserialize;
use serde_json::{json, Value};

const ERR_TRANSIENT: i32 = -32001;
const ERR_PERMANENT: i32 = -32002;
const ERR_CONFIG: i32 = -32602;

#[deprecated(
    since = "0.2.0",
    note = "use `[plugin.poller]` manifest section + nexo-microapp-sdk::poller adapter"
)]
pub struct ExtensionPoller {
    kind: &'static str,
    runtime: Arc<StdioRuntime>,
    tools_cache: Vec<ToolDefinition>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
}

impl ExtensionPoller {
    pub fn new(kind: &'static str, runtime: Arc<StdioRuntime>) -> Self {
        Self {
            kind,
            runtime,
            tools_cache: Vec::new(),
        }
    }

    pub fn with_tools_cache(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools_cache = tools;
        self
    }
}

#[async_trait]
impl Poller for ExtensionPoller {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn description(&self) -> &'static str {
        "(extension — deprecated, migrate to [plugin.poller])"
    }

    fn validate(&self, _config: &Value) -> Result<(), PollerError> {
        Ok(())
    }

    fn custom_tools(&self) -> Vec<nexo_poller::CustomToolSpec> {
        let mut out = Vec::with_capacity(self.tools_cache.len());
        for t in &self.tools_cache {
            let kind = self.kind;
            let runtime_for_handler = Arc::clone(&self.runtime);
            let tool_name = t.name.clone();

            struct ExtToolHandler {
                runtime: Arc<StdioRuntime>,
                kind: &'static str,
                tool_name: String,
            }
            #[async_trait]
            impl nexo_poller::CustomToolHandler for ExtToolHandler {
                async fn call(
                    &self,
                    _runner: Arc<nexo_poller::PollerRunner>,
                    args: Value,
                ) -> anyhow::Result<Value> {
                    let params = json!({
                        "kind":      self.kind,
                        "tool_name": self.tool_name,
                        "args":      args,
                    });
                    self.runtime
                        .call("poll_tool_call", params)
                        .await
                        .map_err(|e| anyhow::anyhow!("ext '{}' poll_tool_call: {e}", self.kind))
                }
            }

            out.push(nexo_poller::CustomToolSpec {
                def: nexo_llm::ToolDef {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
                handler: Arc::new(ExtToolHandler {
                    runtime: runtime_for_handler,
                    kind,
                    tool_name,
                }),
            });
        }
        out
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickAck, PollerError> {
        let cursor_b64 = ctx
            .cursor
            .as_deref()
            .map(|b| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b));
        let params = json!({
            "kind":     self.kind,
            "job_id":   ctx.job_id,
            "agent_id": ctx.agent_id,
            "cursor":   cursor_b64,
            "config":   ctx.config,
            "now":      ctx.now.to_rfc3339(),
        });

        let resp = self
            .runtime
            .call("poll_tick", params)
            .await
            .map_err(map_call_error)?;

        let parsed: TickResponse = serde_json::from_value(resp).map_err(|e| {
            PollerError::Transient(anyhow::anyhow!(
                "extension '{}' returned malformed poll_tick response: {e}",
                self.kind
            ))
        })?;

        let next_cursor = parsed.next_cursor.as_deref().and_then(|s| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s.trim_end_matches('='))
                .ok()
        });

        let next_interval_hint = parsed.next_interval_secs.map(Duration::from_secs);

        Ok(TickAck {
            next_cursor,
            next_interval_hint,
            metrics: parsed.metrics,
        })
    }
}

/// Walk the runtime's manifest capabilities and register one
/// `ExtensionPoller` per `kind`. Deprecated alongside the crate.
pub async fn register_for_runtime(
    runner: &nexo_poller::PollerRunner,
    runtime: &Arc<StdioRuntime>,
    pollers: &[String],
) -> usize {
    let mut count = 0;
    for kind in pollers {
        let leaked: &'static str = Box::leak(kind.clone().into_boxed_str());

        let tools = match runtime
            .call("poll_list_tools", json!({ "kind": leaked }))
            .await
        {
            Ok(Value::Array(items)) => {
                let mut parsed = Vec::with_capacity(items.len());
                for it in items {
                    match serde_json::from_value::<ToolDefinition>(it) {
                        Ok(t) => parsed.push(t),
                        Err(e) => {
                            tracing::warn!(
                                kind = %leaked,
                                error = %e,
                                "extension custom-tool entry malformed; skipping"
                            );
                        }
                    }
                }
                parsed
            }
            Ok(other) => {
                tracing::warn!(
                    kind = %leaked,
                    "poll_list_tools returned non-array ({other}); ignoring"
                );
                Vec::new()
            }
            Err(e) => {
                tracing::debug!(
                    kind = %leaked,
                    error = %e,
                    "extension exposed no custom tools (poll_list_tools failed)"
                );
                Vec::new()
            }
        };

        let poller = ExtensionPoller::new(leaked, Arc::clone(runtime)).with_tools_cache(tools);
        runner.register(Arc::new(poller));
        count += 1;
    }
    count
}

fn map_call_error(err: nexo_extensions::CallError) -> PollerError {
    use nexo_extensions::CallError::*;
    match err {
        Rpc(rpc) => match rpc.code {
            ERR_PERMANENT => PollerError::Permanent(anyhow::anyhow!("ext: {}", rpc.message)),
            ERR_CONFIG => PollerError::Config {
                job: "<extension>".into(),
                reason: rpc.message,
            },
            ERR_TRANSIENT => PollerError::Transient(anyhow::anyhow!("ext: {}", rpc.message)),
            _ => PollerError::Transient(anyhow::anyhow!(
                "ext rpc error code={}: {}",
                rpc.code,
                rpc.message
            )),
        },
        other => PollerError::Transient(anyhow::anyhow!("ext call error: {other}")),
    }
}

#[derive(Debug, Deserialize)]
struct TickResponse {
    #[serde(default)]
    next_cursor: Option<String>,
    #[serde(default)]
    next_interval_secs: Option<u64>,
    #[serde(default)]
    metrics: Option<TickMetrics>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_response() {
        let raw = json!({
            "next_cursor": null,
            "metrics": { "items_seen": 3, "items_dispatched": 2 }
        });
        let parsed: TickResponse = serde_json::from_value(raw).unwrap();
        assert!(parsed.next_cursor.is_none());
        let m = parsed.metrics.unwrap();
        assert_eq!(m.items_seen, 3);
        assert_eq!(m.items_dispatched, 2);
    }

    #[test]
    fn parses_empty_response() {
        let raw = json!({});
        let parsed: TickResponse = serde_json::from_value(raw).unwrap();
        assert!(parsed.next_cursor.is_none());
        assert!(parsed.next_interval_secs.is_none());
        assert!(parsed.metrics.is_none());
    }

    #[test]
    fn permanent_error_is_classified() {
        let rpc = nexo_extensions::RpcError {
            code: ERR_PERMANENT,
            message: "revoked".into(),
            data: None,
        };
        let mapped = map_call_error(nexo_extensions::CallError::Rpc(rpc));
        assert!(matches!(mapped, PollerError::Permanent(_)));
    }

    #[test]
    fn transient_error_is_classified() {
        let rpc = nexo_extensions::RpcError {
            code: ERR_TRANSIENT,
            message: "503".into(),
            data: None,
        };
        let mapped = map_call_error(nexo_extensions::CallError::Rpc(rpc));
        assert!(matches!(mapped, PollerError::Transient(_)));
    }

    #[test]
    fn config_error_is_classified() {
        let rpc = nexo_extensions::RpcError {
            code: ERR_CONFIG,
            message: "missing field x".into(),
            data: None,
        };
        let mapped = map_call_error(nexo_extensions::CallError::Rpc(rpc));
        assert!(matches!(mapped, PollerError::Config { .. }));
    }
}
