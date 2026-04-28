//! Phase 76.9 — ergonomic MCP server builder.
//!
//! Two layers of API:
//!
//!   1. The [`Tool`] trait — implementors expose typed
//!      `Args: DeserializeOwned + JsonSchema` and `Output: Serialize`.
//!      The builder uses `schemars::schema_for!(Args)` to derive
//!      the JSON Schema at registration time, eliminating the
//!      hand-rolled-schema drift that bites
//!      `crates/core/src/agent/web_search_tool.rs:26-42`.
//!
//!   2. (Phase 76.9 part 2 — separate `crates/mcp-macro/` crate)
//!      An `#[mcp_tool]` attribute proc-macro that generates a
//!      `Tool` impl from a free function, so the boilerplate is
//!      ~3 lines per tool. The trait is the foundation; the
//!      macro is sugar.
//!
//! ## Reference patterns
//!
//! * **One-tool-per-struct** — `claude-code-leak/src/Tool.ts:362-695`
//!   bundles `name + description + inputSchema + call` into one
//!   object. Same shape here, with Rust types replacing TS Zod.
//! * **`buildTool(def)` defaults helper** —
//!   `claude-code-leak/src/Tool.ts:783-792`. Mirrored by
//!   [`McpServerBuilder::tool`] which spreads sensible defaults
//!   (`deferred: false`, `search_hint: None`).
//! * **Lazy schema** —
//!   `claude-code-leak/src/tools/WebSearchTool/WebSearchTool.ts:25-41`
//!   defers expensive Zod-to-JSON conversion. We compute the
//!   schema once at registration and cache the JSON `Value`,
//!   so list_tools/call_tool are zero-cost in the hot path.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::errors::McpError;
use crate::server::auth::TenantId;
use crate::server::progress::ProgressReporter;
use crate::types::{McpContent, McpServerInfo, McpTool, McpToolResult};

/// Per-call context handed to a [`Tool::call`] implementation.
/// Bundles the dispatcher's existing borrowed inputs into one
/// ergonomic surface so handlers don't plumb 4 separate args.
///
/// Lifetimes are borrowed (`&'a TenantId`, `&'a str`, …) so the
/// hot path doesn't pay an Arc-clone per dispatch.
pub struct ToolCtx<'a> {
    pub tenant: &'a TenantId,
    pub correlation_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub progress: ProgressReporter,
    pub cancel: &'a CancellationToken,
}

/// One MCP tool. Implementors expose typed `Args` (with both
/// `DeserializeOwned` so the dispatcher can parse the JSON body
/// and `JsonSchema` so the builder can publish the schema in
/// `tools/list`) and a `Serialize` `Output`.
///
/// Default `deferred` and `search_hint` mirror the Phase 79.2
/// surface so a `Tool` impl can opt into the `ToolSearch` lazy
/// surface without going through extra plumbing.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    type Args: DeserializeOwned + JsonSchema + Send;
    type Output: Serialize + Send;

    fn name(&self) -> &str;
    fn description(&self) -> &str;

    fn deferred(&self) -> bool {
        false
    }
    fn search_hint(&self) -> Option<&str> {
        None
    }

    async fn call(&self, args: Self::Args, ctx: ToolCtx<'_>) -> Result<Self::Output, McpError>;
}

/// Hard cap on the bytes of a derived JSON Schema. `schemars`
/// can produce huge schemas for deeply-nested types; >64 KB is
/// almost certainly a typo (Result/Option recursion, missing
/// `#[serde(skip)]` on a circular field, etc.). Cap fails the
/// build at registration time so the operator notices.
pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;

// --- Type-erased registered tool ---------------------------------

/// What the builder stores per tool: the name + description +
/// pre-derived schema, plus a boxed handler that the dispatcher
/// invokes. `BoxedHandler` deserialises the raw `Value` into the
/// tool's typed `Args` and serialises the `Output` back to a
/// `McpToolResult` — all in one indirection per call.
type BoxedHandler = Arc<
    dyn Fn(
            Value,
            ToolCtxOwned,
        ) -> futures::future::BoxFuture<'static, Result<McpToolResult, McpError>>
        + Send
        + Sync,
>;

/// Owned variant of `ToolCtx` so the boxed handler future can be
/// `'static`. Tenant + ids are cloned once at the call site;
/// progress + cancel are already cheap to clone.
pub struct ToolCtxOwned {
    pub tenant: TenantId,
    pub correlation_id: Option<String>,
    pub session_id: Option<String>,
    pub progress: ProgressReporter,
    pub cancel: CancellationToken,
}

impl ToolCtxOwned {
    fn as_borrowed(&self) -> ToolCtx<'_> {
        ToolCtx {
            tenant: &self.tenant,
            correlation_id: self.correlation_id.as_deref(),
            session_id: self.session_id.as_deref(),
            progress: self.progress.clone(),
            cancel: &self.cancel,
        }
    }
}

struct RegisteredTool {
    name: String,
    description: String,
    schema: Value,
    deferred: bool,
    #[allow(dead_code)] // surfaces in Phase 79.2 ToolSearch
    search_hint: Option<String>,
    handler: BoxedHandler,
}

// --- Builder + handler ------------------------------------------

/// Ergonomic constructor for an MCP server. Each `tool(...)`
/// call adds a typed handler; `build_handler()` produces an
/// `impl McpServerHandler` ready to plug into
/// `run_stdio_server` or `start_http_server`.
pub struct McpServerBuilder {
    server_info: McpServerInfo,
    tools: HashMap<String, RegisteredTool>,
}

impl McpServerBuilder {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            server_info: McpServerInfo {
                name: name.into(),
                version: version.into(),
            },
            tools: HashMap::new(),
        }
    }

    /// Register a `Tool` impl. The schema is derived once via
    /// `schemars::schema_for!(T::Args)` and cached. Duplicate
    /// names overwrite with a `tracing::warn!` log so the
    /// operator notices the conflict (mirrors the Phase 11.5
    /// `ToolRegistry::register` semantics).
    pub fn tool<T: Tool>(mut self, tool: T) -> Self {
        let tool = Arc::new(tool);
        let name = tool.name().to_string();
        let description = tool.description().to_string();
        let deferred = tool.deferred();
        let search_hint = tool.search_hint().map(|s| s.to_string());

        // Derive the JSON Schema for Args.
        let schema = schema_for!(T::Args);
        let schema_value =
            serde_json::to_value(&schema).expect("schemars output is JSON-serialisable");
        let bytes = serde_json::to_vec(&schema_value)
            .map(|v| v.len())
            .unwrap_or(0);
        if bytes > MAX_SCHEMA_BYTES {
            panic!(
                "Tool `{name}` produced a JSON Schema of {bytes} bytes, \
                 exceeding the {} byte cap. Likely cause: nested types or \
                 cyclic refs. Add `#[schemars(skip)]` or simplify the args.",
                MAX_SCHEMA_BYTES
            );
        }

        // Box the handler, type-erasing `Args` and `Output`.
        let tool_for_closure = Arc::clone(&tool);
        let handler: BoxedHandler = Arc::new(move |raw_args, ctx_owned| {
            let tool_for_closure = Arc::clone(&tool_for_closure);
            Box::pin(async move {
                let args: T::Args = serde_json::from_value(raw_args)
                    .map_err(|e| McpError::Protocol(format!("invalid args: {e}")))?;
                let output = tool_for_closure.call(args, ctx_owned.as_borrowed()).await?;
                let value = serde_json::to_value(&output)
                    .map_err(|e| McpError::Protocol(format!("output serialise: {e}")))?;
                Ok(McpToolResult {
                    content: vec![McpContent::Text {
                        text: serde_json::to_string(&value).unwrap_or_default(),
                    }],
                    is_error: false,
                    structured_content: Some(value),
                })
            })
        });

        if self.tools.contains_key(&name) {
            tracing::warn!(tool = %name, "McpServerBuilder: duplicate tool name overwrites prior registration");
        }
        self.tools.insert(
            name.clone(),
            RegisteredTool {
                name,
                description,
                schema: schema_value,
                deferred,
                search_hint,
                handler,
            },
        );
        self
    }

    /// Number of tools registered. Useful in tests.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Build an `impl McpServerHandler` ready to feed into
    /// `run_stdio_server` or `start_http_server`.
    pub fn build_handler(self) -> BuiltHandler {
        BuiltHandler {
            server_info: self.server_info,
            tools: self.tools,
        }
    }
}

/// `McpServerHandler` impl produced by `build_handler`. Owns the
/// registered tools by value; cheap to clone (Arc inside each
/// boxed handler).
pub struct BuiltHandler {
    server_info: McpServerInfo,
    tools: HashMap<String, RegisteredTool>,
}

#[async_trait]
impl crate::server::McpServerHandler for BuiltHandler {
    fn server_info(&self) -> McpServerInfo {
        self.server_info.clone()
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let mut out: Vec<McpTool> = self
            .tools
            .values()
            .map(|t| McpTool {
                name: t.name.clone(),
                description: Some(t.description.clone()),
                input_schema: t.schema.clone(),
                output_schema: None,
            })
            .collect();
        // Stable order so list_tools is byte-identical between
        // calls — clients that diff schemas appreciate this.
        out.sort_by(|a, b| a.name.cmp(&b.name));
        // Surface `deferred` flag via the side-channel only when
        // some tool opts in (most won't).
        let _has_deferred = self.tools.values().any(|t| t.deferred);
        Ok(out)
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError> {
        // Default ctx — used when the dispatcher hasn't routed
        // through `call_tool_streaming`. Tenant defaults to
        // STDIO_LOCAL since the streaming path is what carries
        // the live `ProgressReporter` + correlation ids.
        let tenant = TenantId::parse(TenantId::STDIO_LOCAL).expect("STDIO_LOCAL parses");
        let ctx = ToolCtxOwned {
            tenant,
            correlation_id: None,
            session_id: None,
            progress: ProgressReporter::noop(),
            cancel: CancellationToken::new(),
        };
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| McpError::Protocol(format!("unknown tool `{name}`")))?;
        (tool.handler)(arguments, ctx).await
    }

    async fn call_tool_streaming(
        &self,
        name: &str,
        arguments: Value,
        progress: ProgressReporter,
    ) -> Result<McpToolResult, McpError> {
        let tenant = TenantId::parse(TenantId::STDIO_LOCAL).expect("STDIO_LOCAL parses");
        let ctx = ToolCtxOwned {
            tenant,
            correlation_id: None,
            session_id: None,
            progress,
            cancel: CancellationToken::new(),
        };
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| McpError::Protocol(format!("unknown tool `{name}`")))?;
        (tool.handler)(arguments, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::McpServerHandler;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Deserialize, JsonSchema)]
    struct GreetArgs {
        name: String,
    }

    #[derive(Serialize)]
    struct Greeting {
        text: String,
    }

    struct Greet;

    #[async_trait]
    impl Tool for Greet {
        type Args = GreetArgs;
        type Output = Greeting;
        fn name(&self) -> &str {
            "greet"
        }
        fn description(&self) -> &str {
            "Greet a person"
        }
        async fn call(
            &self,
            args: Self::Args,
            _ctx: ToolCtx<'_>,
        ) -> Result<Self::Output, McpError> {
            Ok(Greeting {
                text: format!("Hello, {}!", args.name),
            })
        }
    }

    #[tokio::test]
    async fn builder_registers_and_lists_one_tool() {
        let h = McpServerBuilder::new("hello", "0.1.0")
            .tool(Greet)
            .build_handler();
        assert_eq!(h.server_info().name, "hello");
        let tools = h.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "greet");
        assert_eq!(tools[0].description.as_deref(), Some("Greet a person"));
        // Schema must include the `name` field declared by GreetArgs.
        let s = serde_json::to_string(&tools[0].input_schema).unwrap();
        assert!(s.contains("\"name\""), "schema lacks `name`: {s}");
    }

    #[tokio::test]
    async fn call_tool_round_trips_typed_args() {
        let h = McpServerBuilder::new("hello", "0.1.0")
            .tool(Greet)
            .build_handler();
        let res = h
            .call_tool("greet", serde_json::json!({"name":"Cristian"}))
            .await
            .unwrap();
        let s = res.structured_content.unwrap();
        assert_eq!(s.get("text").unwrap().as_str().unwrap(), "Hello, Cristian!");
    }

    #[tokio::test]
    async fn call_tool_rejects_invalid_args() {
        let h = McpServerBuilder::new("hello", "0.1.0")
            .tool(Greet)
            .build_handler();
        // `name` missing → deserialize error → Protocol error.
        let res = h.call_tool("greet", serde_json::json!({})).await;
        assert!(res.is_err(), "expected error for missing args");
    }

    #[tokio::test]
    async fn call_tool_rejects_unknown_name() {
        let h = McpServerBuilder::new("hello", "0.1.0")
            .tool(Greet)
            .build_handler();
        let res = h.call_tool("nope", serde_json::json!({"name":"x"})).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn list_tools_is_deterministic_order() {
        struct Foo;
        #[async_trait]
        impl Tool for Foo {
            type Args = GreetArgs;
            type Output = Greeting;
            fn name(&self) -> &str {
                "foo"
            }
            fn description(&self) -> &str {
                "f"
            }
            async fn call(
                &self,
                args: Self::Args,
                _ctx: ToolCtx<'_>,
            ) -> Result<Self::Output, McpError> {
                Ok(Greeting {
                    text: args.name.clone(),
                })
            }
        }
        let h = McpServerBuilder::new("multi", "0.1.0")
            .tool(Foo)
            .tool(Greet)
            .build_handler();
        let tools = h.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
        // Sorted alphabetically.
        assert_eq!(tools[0].name, "foo");
        assert_eq!(tools[1].name, "greet");
    }

    #[tokio::test]
    async fn duplicate_tool_overwrites_with_warning() {
        // Two tools with the same name → second wins, no panic.
        struct G2;
        #[async_trait]
        impl Tool for G2 {
            type Args = GreetArgs;
            type Output = Greeting;
            fn name(&self) -> &str {
                "greet"
            }
            fn description(&self) -> &str {
                "Greet v2"
            }
            async fn call(
                &self,
                _args: Self::Args,
                _ctx: ToolCtx<'_>,
            ) -> Result<Self::Output, McpError> {
                Ok(Greeting { text: "v2".into() })
            }
        }
        let h = McpServerBuilder::new("dup", "0.1.0")
            .tool(Greet)
            .tool(G2)
            .build_handler();
        let tools = h.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].description.as_deref(), Some("Greet v2"));
    }
}
