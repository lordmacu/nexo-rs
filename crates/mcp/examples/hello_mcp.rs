//! Phase 76.9 — minimal MCP server in <50 LOC of operator code.
//!
//! Demonstrates the new ergonomic surface:
//!   * `#[derive(Deserialize, JsonSchema)]` on the args struct
//!     auto-derives the JSON Schema published in `tools/list`.
//!   * `Tool` trait — typed `Args` and `Output`, no hand-rolled
//!     JSON literals.
//!   * `McpServerBuilder::new(...).tool(...).build_handler()` —
//!     fluent registration, ready to plug into stdio.
//!
//! Compare with the pre-76.9 path
//! (`crates/core/src/agent/web_search_tool.rs:26-42`) where the
//! schema was a `serde_json::json!(...)` literal hand-maintained
//! parallel to the args struct — drift was guaranteed.

use async_trait::async_trait;
use nexo_mcp::server::builder::{McpServerBuilder, Tool, ToolCtx};
use nexo_mcp::McpError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
struct GreetArgs {
    /// The person to greet.
    name: String,
    /// Optional greeting language code (en/es). Defaults to "en".
    #[serde(default)]
    lang: Option<String>,
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
        "Greet a person, optionally in their preferred language."
    }
    async fn call(&self, args: Self::Args, _ctx: ToolCtx<'_>) -> Result<Self::Output, McpError> {
        let hello = match args.lang.as_deref().unwrap_or("en") {
            "es" => "Hola",
            "fr" => "Bonjour",
            _ => "Hello",
        };
        Ok(Greeting {
            text: format!("{hello}, {}!", args.name),
        })
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    use tokio_util::sync::CancellationToken;
    let handler = McpServerBuilder::new("hello-mcp", "0.1.0")
        .tool(Greet)
        .build_handler();
    let shutdown = CancellationToken::new();
    nexo_mcp::server::run_stdio_server(handler, shutdown).await
}
