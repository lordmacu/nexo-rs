//! Sample tools for the MCP server template.
//!
//! Reference patterns:
//! * `crates/mcp/src/server/builder.rs:340-398` — the canonical
//!   `Greet` tool used in unit tests; this file is the same
//!   pattern wired into a binary.
//! * `claude-code-leak/src/entrypoints/mcp.ts:35-78` — the leak's
//!   minimal stdio server pattern. Our equivalent is even shorter
//!   because `McpServerBuilder` derives the JSON Schema for us;
//!   the leak hand-rolls the schema in each `setRequestHandler`.

use async_trait::async_trait;
use nexo_mcp::server::builder::{Tool, ToolCtx};
use nexo_mcp::McpError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Typed input for the `echo` sample tool. `JsonSchema` derives
/// the `input_schema` published in `tools/list`; `Deserialize`
/// produces a typed value out of the JSON-RPC `arguments` blob.
#[derive(Deserialize, JsonSchema)]
pub struct EchoArgs {
    /// Text the server will echo back. Intentionally `String` —
    /// swap for your own typed shape (struct of fields, enum
    /// variants) when you fork the template.
    pub text: String,
}

/// Typed output. `Serialize` lets the dispatcher emit it in the
/// `tools/call` response's `structuredContent` field, which Claude
/// Code 2.1 (Phase 74) consumes for richer rendering. The plain
/// text content is auto-derived from the structured JSON.
#[derive(Serialize, JsonSchema)]
pub struct EchoOut {
    pub echoed: String,
}

pub struct Echo;

#[async_trait]
impl Tool for Echo {
    type Args = EchoArgs;
    type Output = EchoOut;

    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Return the input text wrapped in `{ \"echoed\": ... }`. Demo only."
    }

    async fn call(&self, args: Self::Args, _ctx: ToolCtx<'_>) -> Result<Self::Output, McpError> {
        Ok(EchoOut { echoed: args.text })
    }
}
