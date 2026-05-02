//! Phase 83.8 — `ventas-etb` reference SaaS-shaped microapp.
//!
//! First real microapp built on Phase 83.1-83.7 primitives.
//! Validates the framework is usable for production work, not
//! just toy examples.
//!
//! Same binary serves multiple personas (Ana / Carlos / María)
//! by reading per-agent `extensions_config` (Phase 83.1) at
//! initialize time and routing each `tools/call` through the
//! agent-id carried in `BindingContext` (Phase 82.1).
//!
//! Stage 1 (this commit): scaffold + 4 stub tools + leads-store
//! placeholder. Real markdown tarifarios, SQLite leads.db,
//! compliance hooks, and skill markdown land in subsequent
//! commits.

use nexo_microapp_sdk::{Microapp, ToolCtx, ToolError, ToolReply};
use serde_json::{json, Value};

mod tools;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nexo_microapp_sdk::init_logging_from_env("ventas-etb");

    let app = Microapp::new("ventas-etb", env!("CARGO_PKG_VERSION"))
        .with_tool("ventas_etb_get_planes", tools::get_planes)
        .with_tool("ventas_etb_check_coverage", tools::check_coverage)
        .with_tool("ventas_etb_register_lead", tools::register_lead)
        .with_tool("ventas_etb_lead_summary", tools::lead_summary);

    app.run_stdio().await?;
    Ok(())
}

/// Helper used across tool handlers — extract the agent_id
/// from BindingContext or default to `"unknown"` so test +
/// delegation paths never panic. Tenant isolation is keyed on
/// this string.
pub(crate) fn agent_id_for(ctx: &ToolCtx) -> String {
    ctx.binding()
        .map(|b| b.agent_id.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Helper: short success reply with a generic JSON body.
pub(crate) fn ok(value: Value) -> Result<ToolReply, ToolError> {
    Ok(ToolReply::ok_json(value))
}

/// Helper: produce a uniform error envelope.
pub(crate) fn wire_err(message: impl Into<String>) -> ToolError {
    ToolError::wire(message)
}

#[allow(dead_code)]
pub(crate) fn placeholder_response(tool: &str, agent: &str) -> Value {
    json!({
        "tool": tool,
        "agent": agent,
        "status": "stub",
        "note": "ventas-etb stage 1 scaffold — real implementation lands in subsequent commits"
    })
}
