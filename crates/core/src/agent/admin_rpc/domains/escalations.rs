//! Phase 82.14 — agent escalation list + resolve handlers.
//!
//! Backed by an `EscalationStore` trait so the dispatcher
//! crate stays cycle-free vs whoever holds the persistent
//! impl. v0 production wires
//! `nexo_setup::admin_adapters::InMemoryEscalationStore`
//! (DashMap, mirrors the in-memory pairing + processing
//! stores).
//!
//! The `escalate_to_human` built-in tool that PRODUCES the
//! `Pending` rows is deferred to 82.14.b alongside the
//! BindingContext→scope derivation. v0 ships the read +
//! resolve surface so operator UIs can poll today.

use async_trait::async_trait;
use nexo_tool_meta::admin::escalations::{
    EscalationEntry, EscalationState, EscalationsListFilter, EscalationsListParams,
    EscalationsListResponse, EscalationsResolveParams, EscalationsResolveResponse, ResolvedBy,
};
use nexo_tool_meta::admin::processing::ProcessingScope;
use serde_json::Value;
use uuid::Uuid;

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Default `list` cap when the caller leaves it unset.
pub const DEFAULT_LIST_LIMIT: usize = 100;
const MAX_LIST_LIMIT: usize = 1000;

/// Storage abstraction for escalations. v0 production wires
/// the in-memory adapter; durable SQLite variant is a
/// 82.14.b follow-up alongside the
/// `escalate_to_human` tool.
#[async_trait]
pub trait EscalationStore: Send + Sync + std::fmt::Debug {
    /// List entries matching `filter`, newest first.
    async fn list(
        &self,
        filter: &EscalationsListParams,
    ) -> anyhow::Result<Vec<EscalationEntry>>;
    /// Read the current state for one scope.
    async fn get(&self, scope: &ProcessingScope) -> anyhow::Result<EscalationState>;
    /// Atomic resolve transition: `Pending` → `Resolved` with
    /// the supplied [`ResolvedBy`]. Returns `false` when the
    /// scope wasn't pending (idempotent retry).
    async fn resolve(
        &self,
        scope: &ProcessingScope,
        by: ResolvedBy,
        resolved_at_ms: u64,
    ) -> anyhow::Result<bool>;
    /// Set a fresh `Pending` row (used by the future
    /// `escalate_to_human` tool — v0 exposes it for tests
    /// + the eventual emit site).
    async fn upsert_pending(
        &self,
        agent_id: String,
        state: EscalationState,
    ) -> anyhow::Result<bool>;
}

/// `nexo/admin/escalations/list` — read-only paginated query.
pub async fn list(
    store: &dyn EscalationStore,
    params: Value,
) -> AdminRpcResult {
    let mut p: EscalationsListParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.limit == 0 {
        p.limit = DEFAULT_LIST_LIMIT;
    }
    p.limit = p.limit.min(MAX_LIST_LIMIT);
    let entries = match store.list(&p).await {
        Ok(rows) => rows,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "escalations.list: {e}"
            )))
        }
    };
    AdminRpcResult::ok(
        serde_json::to_value(EscalationsListResponse { entries })
            .unwrap_or(Value::Null),
    )
}

/// `nexo/admin/escalations/resolve` — flip Pending → Resolved.
pub async fn resolve(
    store: &dyn EscalationStore,
    params: Value,
) -> AdminRpcResult {
    let p: EscalationsResolveParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    let by = match p.by.as_str() {
        "dismissed" => match p.dismiss_reason {
            Some(r) if !r.trim().is_empty() => ResolvedBy::OperatorDismissed { reason: r },
            _ => {
                return AdminRpcResult::err(AdminRpcError::InvalidParams(
                    "dismiss_reason is required when by=dismissed".into(),
                ))
            }
        },
        "takeover" => ResolvedBy::OperatorTakeover,
        other => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "by must be `dismissed` or `takeover`, got `{other}`"
            )))
        }
    };
    let changed = match store.resolve(&p.scope, by, now_epoch_ms()).await {
        Ok(c) => c,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "escalations.resolve: {e}"
            )))
        }
    };
    AdminRpcResult::ok(
        serde_json::to_value(EscalationsResolveResponse {
            changed,
            correlation_id: Uuid::new_v4(),
        })
        .unwrap_or(Value::Null),
    )
}

/// Phase 82.14 cross-cut into 82.13: when `processing/pause`
/// fires on a scope that has a Pending escalation, auto-flip
/// the escalation to `Resolved { OperatorTakeover }`. Boot
/// wires this by calling [`auto_resolve_on_pause`] from the
/// processing pause handler when both stores are configured.
/// Idempotent — returns `Ok(true)` when a row was flipped,
/// `Ok(false)` otherwise.
pub async fn auto_resolve_on_pause(
    store: &dyn EscalationStore,
    scope: &ProcessingScope,
) -> anyhow::Result<bool> {
    store
        .resolve(scope, ResolvedBy::OperatorTakeover, now_epoch_ms())
        .await
}

/// Convenience: filter helper used by in-memory store impls.
/// Production callers that need a different ordering can
/// override list directly.
pub fn filter_matches(
    entry: &EscalationEntry,
    params: &EscalationsListParams,
) -> bool {
    if let Some(want_agent) = &params.agent_id {
        if &entry.agent_id != want_agent {
            return false;
        }
    }
    if let Some(want_kind) = &params.scope_kind {
        let actual_kind = scope_discriminator(&entry.scope);
        if actual_kind != want_kind {
            return false;
        }
    }
    match (&params.filter, &entry.state) {
        (EscalationsListFilter::Pending, EscalationState::Pending { .. }) => true,
        (EscalationsListFilter::Resolved, EscalationState::Resolved { .. }) => true,
        (EscalationsListFilter::All, EscalationState::Pending { .. })
        | (EscalationsListFilter::All, EscalationState::Resolved { .. }) => true,
        _ => false,
    }
}

/// Mirror the snake_case `kind` discriminator from
/// `ProcessingScope` — tests + filters compare against this
/// string.
pub fn scope_discriminator(scope: &ProcessingScope) -> &'static str {
    match scope {
        ProcessingScope::Conversation { .. } => "conversation",
        ProcessingScope::AgentBinding { .. } => "agent_binding",
        ProcessingScope::Agent { .. } => "agent",
        ProcessingScope::EventStream { .. } => "event_stream",
        ProcessingScope::BatchQueue { .. } => "batch_queue",
        ProcessingScope::Custom { .. } => "custom",
        // Future variants — be lenient.
        _ => "unknown",
    }
}

fn now_epoch_ms() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use nexo_tool_meta::admin::escalations::{EscalationReason, EscalationUrgency};

    #[derive(Debug, Default)]
    struct MockStore {
        rows: Mutex<std::collections::HashMap<ProcessingScope, EscalationEntry>>,
    }

    #[async_trait]
    impl EscalationStore for MockStore {
        async fn list(
            &self,
            filter: &EscalationsListParams,
        ) -> anyhow::Result<Vec<EscalationEntry>> {
            let rows = self.rows.lock().unwrap();
            let mut out: Vec<EscalationEntry> = rows
                .values()
                .filter(|e| filter_matches(e, filter))
                .cloned()
                .collect();
            out.truncate(filter.limit);
            Ok(out)
        }
        async fn get(
            &self,
            scope: &ProcessingScope,
        ) -> anyhow::Result<EscalationState> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .get(scope)
                .map(|e| e.state.clone())
                .unwrap_or(EscalationState::None))
        }
        async fn resolve(
            &self,
            scope: &ProcessingScope,
            by: ResolvedBy,
            resolved_at_ms: u64,
        ) -> anyhow::Result<bool> {
            let mut rows = self.rows.lock().unwrap();
            let Some(entry) = rows.get_mut(scope) else {
                return Ok(false);
            };
            if !matches!(entry.state, EscalationState::Pending { .. }) {
                return Ok(false);
            }
            entry.state = EscalationState::Resolved {
                scope: scope.clone(),
                resolved_at_ms,
                by,
            };
            Ok(true)
        }
        async fn upsert_pending(
            &self,
            agent_id: String,
            state: EscalationState,
        ) -> anyhow::Result<bool> {
            let scope = match &state {
                EscalationState::Pending { scope, .. }
                | EscalationState::Resolved { scope, .. } => scope.clone(),
                _ => anyhow::bail!("upsert_pending requires Pending or Resolved state"),
            };
            let entry = EscalationEntry {
                agent_id,
                scope: scope.clone(),
                state,
            };
            let prev = self.rows.lock().unwrap().insert(scope, entry);
            Ok(prev.is_none())
        }
    }

    fn convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.55".into(),
            mcp_channel_source: None,
        }
    }

    fn pending_state() -> EscalationState {
        EscalationState::Pending {
            scope: convo(),
            summary: "needs human".into(),
            reason: EscalationReason::OutOfScope,
            urgency: EscalationUrgency::High,
            context: BTreeMap::new(),
            requested_at_ms: 1_700_000_000_000,
        }
    }

    #[tokio::test]
    async fn list_returns_pending_by_default() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let result = list(&store, serde_json::json!({})).await;
        let resp: EscalationsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(resp.entries.len(), 1);
    }

    #[tokio::test]
    async fn list_caps_limit() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let result = list(
            &store,
            serde_json::json!({ "filter": "all", "limit": 0 }),
        )
        .await;
        let resp: EscalationsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        // The mock returns at most `params.limit` rows; we
        // sent 0 → server clamps to DEFAULT_LIST_LIMIT, mock
        // returns up to that.
        assert_eq!(resp.entries.len(), 1);
    }

    #[tokio::test]
    async fn resolve_dismissed_requires_reason() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let r = resolve(
            &store,
            serde_json::json!({
                "scope": convo(),
                "by": "dismissed",
                "operator_token_hash": "h",
            }),
        )
        .await;
        assert!(matches!(
            r.error.expect("error"),
            AdminRpcError::InvalidParams(_)
        ));
    }

    #[tokio::test]
    async fn resolve_takeover_flips_state() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let r = resolve(
            &store,
            serde_json::json!({
                "scope": convo(),
                "by": "takeover",
                "operator_token_hash": "h",
            }),
        )
        .await;
        let resp: EscalationsResolveResponse =
            serde_json::from_value(r.result.unwrap()).unwrap();
        assert!(resp.changed);
        let state = store.get(&convo()).await.unwrap();
        assert!(matches!(state, EscalationState::Resolved { .. }));
    }

    #[tokio::test]
    async fn resolve_unknown_scope_returns_changed_false() {
        let store = MockStore::default();
        let r = resolve(
            &store,
            serde_json::json!({
                "scope": convo(),
                "by": "takeover",
                "operator_token_hash": "h",
            }),
        )
        .await;
        let resp: EscalationsResolveResponse =
            serde_json::from_value(r.result.unwrap()).unwrap();
        assert!(!resp.changed);
    }

    #[tokio::test]
    async fn auto_resolve_on_pause_flips_pending_to_takeover() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let changed = auto_resolve_on_pause(&store, &convo()).await.unwrap();
        assert!(changed);
        let state = store.get(&convo()).await.unwrap();
        match state {
            EscalationState::Resolved { by, .. } => {
                assert!(matches!(by, ResolvedBy::OperatorTakeover));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn scope_discriminator_matches_serde_tag() {
        assert_eq!(scope_discriminator(&convo()), "conversation");
        assert_eq!(
            scope_discriminator(&ProcessingScope::Agent {
                agent_id: "ana".into()
            }),
            "agent"
        );
    }
}
