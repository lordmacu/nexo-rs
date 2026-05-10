//! Phase 83.12.audit-page — wire-shape types for the
//! `nexo/admin/microapp_audit/tail` admin RPC method that backs
//! the `agent-creator-microapp/frontend/` audit log page.
//!
//! Moved here from `nexo_core::agent::admin_rpc::audit` so the
//! ts-types-codegen pipeline (Phase 83.12.ts-types-codegen)
//! picks them up — `nexo-tool-meta` is the canonical home for
//! types that cross the Rust↔TS boundary, and the
//! `#[ts(export)]` derive only applies to types in this crate.
//! The original module re-exports from here, so no consumer
//! sees a broken import path.

use serde::{Deserialize, Serialize};

/// Outcome of an admin RPC dispatch — written to the audit table
/// alongside every call.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminAuditResult {
    /// Handler returned a `result` payload.
    Ok,
    /// Handler returned an error other than capability denial
    /// (validation failure, internal error, method-not-found).
    Error,
    /// Capability gate refused the call before dispatch.
    Denied,
}

impl AdminAuditResult {
    /// Stable wire string used by the SQLite writer (82.10.g) and
    /// the CLI tail formatter.
    pub fn as_str(self) -> &'static str {
        match self {
            AdminAuditResult::Ok => "ok",
            AdminAuditResult::Error => "error",
            AdminAuditResult::Denied => "denied",
        }
    }

    /// Inverse of [`Self::as_str`]. Unknown strings (e.g.
    /// forward-compat from a future writer variant) map to
    /// `Error` so the audit row is never silently misclassified
    /// as `Ok`.
    pub fn from_str(s: &str) -> Self {
        match s {
            "ok" => AdminAuditResult::Ok,
            "denied" => AdminAuditResult::Denied,
            _ => AdminAuditResult::Error,
        }
    }
}

/// One row of `microapp_admin_audit`. Stable wire shape — every
/// field is present in the JSON payload returned by
/// `nexo/admin/microapp_audit/tail`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminAuditRow {
    /// Microapp identity (extension id from `extensions.yaml`).
    pub microapp_id: String,
    /// Full JSON-RPC method (`nexo/admin/<domain>/<method>`).
    pub method: String,
    /// Required capability (e.g. `agents_crud`). When the call
    /// was denied, this is the capability that was missing.
    pub capability: String,
    /// SHA-256 of canonicalised params (sorted keys). Lets
    /// operators detect repeated identical calls without storing
    /// PII payloads.
    pub args_hash: String,
    /// Epoch milliseconds when dispatch started.
    pub started_at_ms: u64,
    /// `"ok"` | `"error"` | `"denied"`.
    pub result: AdminAuditResult,
    /// Wall-clock duration of the dispatch.
    pub duration_ms: u64,
    /// Phase 83.8.12.7 — tenant scope for the call. Sniffed from
    /// `params.tenant_id` (string) when present; legacy rows /
    /// non-tenant-scoped calls (echo, pairing, credentials)
    /// leave it `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// Filter passed to `nexo/admin/microapp_audit/tail`. Every
/// field optional; combine to narrow the result set.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AuditTailFilter {
    /// Restrict to a single microapp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microapp_id: Option<String>,
    /// Restrict to one JSON-RPC method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Restrict to one outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<AdminAuditResult>,
    /// Restrict to one tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Lower-bound timestamp (epoch ms). Use
    /// `now - duration_ms` for human-friendly windows
    /// ("last 24h", "last 7d", …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// Max rows to return. Server clamps to `[1, 500]`. `0` →
    /// default 50.
    #[serde(default)]
    pub limit: usize,
    /// Pagination offset. `0` for the first page; subsequent
    /// pages set this to the previous response's `next_offset`.
    #[serde(default)]
    pub offset: usize,
}

/// Paginated response for `nexo/admin/microapp_audit/tail`.
/// Frontend stores `entries`, appends on `loadMore` until
/// `has_more = false`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AuditTailPage {
    /// Rows in newest-first order, capped by `filter.limit`.
    pub entries: Vec<AdminAuditRow>,
    /// Total matching rows (without `limit`/`offset`). Useful
    /// for "showing N of M" UI labels.
    pub total: u64,
    /// `true` when `entries.len()` was capped by `limit` and
    /// more rows match the filter.
    pub has_more: bool,
    /// Offset to pass to the next call's `filter.offset`.
    /// `None` when `has_more = false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}
