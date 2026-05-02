//! Tool handlers — Stage 1 stubs.
//!
//! Each handler reads `BindingContext.agent_id` to drive
//! per-persona behaviour and returns a placeholder JSON shape.
//! Real implementation lands in subsequent commits:
//!
//! - Stage 2: SQLite leads.db schema with per-agent isolation
//! - Stage 3: `get_planes` reads markdown tarifarios
//! - Stage 4: `register_lead` + `lead_summary` against SQLite
//! - Stage 5: compliance hook wiring (anti-loop / opt-out / PII)

use nexo_microapp_sdk::{ToolCtx, ToolError, ToolReply};
use serde_json::{json, Value};

use crate::{agent_id_for, ok, placeholder_response, wire_err};

/// Return the tarifario applicable to the agent's regional
/// (Stage 3 will read real markdown via MarkdownWatcher).
pub async fn get_planes(args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    let agent = agent_id_for(&ctx);
    let regional = args
        .get("regional")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    ok(json!({
        "tool": "ventas_etb_get_planes",
        "agent": agent,
        "regional": regional,
        "planes": [],
        "status": "stub"
    }))
}

/// Stub — Stage 3 will hit the coverage API per regional.
pub async fn check_coverage(args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    let agent = agent_id_for(&ctx);
    let address = args
        .get("address")
        .and_then(|v| v.as_str())
        .ok_or_else(|| wire_err("address required"))?;
    ok(json!({
        "tool": "ventas_etb_check_coverage",
        "agent": agent,
        "address": address,
        "covered": null,
        "status": "stub"
    }))
}

/// Stub — Stage 4 will INSERT into the per-agent leads.db.
pub async fn register_lead(args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    let agent = agent_id_for(&ctx);
    let phone = args
        .get("phone")
        .and_then(|v| v.as_str())
        .ok_or_else(|| wire_err("phone required"))?;
    ok(json!({
        "tool": "ventas_etb_register_lead",
        "agent": agent,
        "phone": phone,
        "lead_id": null,
        "status": "stub"
    }))
}

/// Stub — Stage 4 will SELECT KPIs from per-agent leads.db.
pub async fn lead_summary(_args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    let agent = agent_id_for(&ctx);
    ok(placeholder_response("ventas_etb_lead_summary", &agent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_ctx() -> ToolCtx {
        ToolCtx {
            binding: None,
            inbound: None,
            outbound: nexo_microapp_sdk::OutboundDispatcher::stub(),
        }
    }

    #[tokio::test]
    async fn get_planes_returns_stub_with_default_regional() {
        let v = get_planes(json!({}), empty_ctx()).await.unwrap();
        let body = v.as_value();
        assert_eq!(body["regional"], "default");
        assert_eq!(body["status"], "stub");
    }

    #[tokio::test]
    async fn check_coverage_requires_address() {
        let err = check_coverage(json!({}), empty_ctx()).await.err().unwrap();
        assert!(format!("{err}").contains("address"));
    }

    #[tokio::test]
    async fn register_lead_requires_phone() {
        let err = register_lead(json!({}), empty_ctx()).await.err().unwrap();
        assert!(format!("{err}").contains("phone"));
    }

    #[tokio::test]
    async fn lead_summary_returns_stub() {
        let v = lead_summary(json!({}), empty_ctx()).await.unwrap();
        let body = v.as_value();
        assert_eq!(body["tool"], "ventas_etb_lead_summary");
        assert_eq!(body["status"], "stub");
    }
}
