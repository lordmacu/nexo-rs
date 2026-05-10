//! Phase 83.12.audit-page — `nexo/admin/microapp_audit/tail`
//! handler. Backs the agent-creator-microapp's audit log page.
//!
//! Read-only. The matching write path lives in
//! [`crate::agent::admin_rpc::audit::AdminAuditWriter`] which
//! the dispatcher calls automatically after every dispatch
//! (independent code path; the reader trait below only sees
//! committed rows).

use serde_json::Value;

use crate::agent::admin_rpc::audit::{AdminAuditReader, AuditTailFilter};
use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// `nexo/admin/microapp_audit/tail` — return one paginated page
/// of audit rows. Filter shape is fully optional; `limit = 0`
/// resolves server-side to 50 rows. Newest-first order.
pub async fn tail(reader: &dyn AdminAuditReader, params: Value) -> AdminRpcResult {
    let filter: AuditTailFilter = match serde_json::from_value(params) {
        Ok(f) => f,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };

    let page = match reader.tail(&filter).await {
        Ok(p) => p,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "audit_reader.tail: {e}"
            )));
        }
    };

    AdminRpcResult::ok(serde_json::to_value(page).unwrap_or(Value::Null))
}
