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
use nexo_tool_meta::admin::agent_events::AgentEventKind;
use nexo_tool_meta::admin::escalations::{
    EscalationEntry, EscalationState, EscalationsListFilter, EscalationsListParams,
    EscalationsListResponse, EscalationsResolveParams, EscalationsResolveResponse, ResolvedBy,
};
use nexo_tool_meta::admin::processing::ProcessingScope;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};
use crate::agent::agent_events::AgentEventEmitter;

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
///
/// Phase 83.8.12.4.b — `patcher` is optional. When `Some` AND
/// `params.tenant_id.is_some()`, the handler filters returned
/// rows by joining each `EscalationEntry.agent_id` against
/// `agents.yaml.<id>.tenant_id`. Defense-in-depth: agents
/// without a `tenant_id` field are excluded from any non-`None`
/// tenant query (matches the `agents/list` behavior). When
/// `patcher` is `None` (test paths), the filter is a pass-through
/// — preserving prior behavior.
pub async fn list(
    store: &dyn EscalationStore,
    patcher: Option<&dyn super::agents::YamlPatcher>,
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
    let entries = match (&p.tenant_id, patcher) {
        (Some(want), Some(patcher)) => entries
            .into_iter()
            .filter(|e| {
                super::agents::agent_tenant_id(patcher, &e.agent_id).as_deref()
                    == Some(want.as_str())
            })
            .collect(),
        _ => entries,
    };
    AdminRpcResult::ok(
        serde_json::to_value(EscalationsListResponse { entries })
            .unwrap_or(Value::Null),
    )
}

/// `nexo/admin/escalations/resolve` — flip Pending → Resolved.
///
/// Phase 82.14.b — when `emitter` is `Some` AND the resolve
/// transition was a real flip (`changed = true`), fires
/// `AgentEventKind::EscalationResolved` on the firehose so
/// operator UIs subscribed to `nexo/notify/agent_event` clear
/// the badge in real time. Without an emitter wired the
/// transition still happens; subscribers fall back to polling
/// `escalations/list`.
pub async fn resolve(
    store: &dyn EscalationStore,
    emitter: Option<&Arc<dyn AgentEventEmitter>>,
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
    let resolved_at_ms = now_epoch_ms();
    let by_for_emit = by.clone();
    let changed = match store.resolve(&p.scope, by, resolved_at_ms).await {
        Ok(c) => c,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "escalations.resolve: {e}"
            )))
        }
    };
    if changed {
        if let Some(em) = emitter {
            em.emit(AgentEventKind::EscalationResolved {
                agent_id: p.scope.agent_id().to_string(),
                scope: p.scope.clone(),
                resolved_at_ms,
                by: by_for_emit,
                tenant_id: None,
            })
            .await;
        }
    }
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
///
/// Phase 82.14.b — when `emitter` is `Some` AND the row
/// flipped, fires `AgentEventKind::EscalationResolved` on the
/// firehose. Same shape as the operator-driven `resolve`
/// handler emit so subscribers can't tell the two paths
/// apart on the wire.
pub async fn auto_resolve_on_pause(
    store: &dyn EscalationStore,
    emitter: Option<&Arc<dyn AgentEventEmitter>>,
    scope: &ProcessingScope,
) -> anyhow::Result<bool> {
    let resolved_at_ms = now_epoch_ms();
    let changed = store
        .resolve(scope, ResolvedBy::OperatorTakeover, resolved_at_ms)
        .await?;
    if changed {
        if let Some(em) = emitter {
            em.emit(AgentEventKind::EscalationResolved {
                agent_id: scope.agent_id().to_string(),
                scope: scope.clone(),
                resolved_at_ms,
                by: ResolvedBy::OperatorTakeover,
                tenant_id: None,
            })
            .await;
        }
    }
    Ok(changed)
}

/// Phase 82.14.b — sliding-window escalation throttle.
///
/// Caps how many `escalate_to_human` calls a single scope can
/// emit within a rolling time window. Defends against agent
/// loops that flood the operator UI with thousands of identical
/// escalations on each token-budget cycle. The future built-in
/// tool consults `try_acquire(scope)` before calling
/// `store.upsert_pending`; on `Err(ThrottleDenied)` the agent
/// receives the denial and the operator UI never sees the
/// noise.
///
/// Defaults: 3 escalations per scope per hour. Mirrors the
/// 82.14 follow-up spec ("max 3 escalations per scope per hour
/// to prevent agent loops"). Per-scope (NOT per-agent) so an
/// agent that genuinely needs to flag two distinct
/// conversations within an hour still passes.
#[derive(Debug)]
pub struct EscalationThrottle {
    window_ms: u64,
    cap: u32,
    history:
        std::sync::Arc<dashmap::DashMap<ProcessingScope, std::collections::VecDeque<u64>>>,
}

/// Failure shape returned by [`EscalationThrottle::try_acquire`]
/// when the per-scope cap is exhausted. Caller surfaces this to
/// the agent so the agent can wait + retry rather than spinning
/// on the same scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThrottleDenied {
    /// Window cap that was hit (e.g. `3`).
    pub cap: u32,
    /// Window length in ms (e.g. 3_600_000 for an hour).
    pub window_ms: u64,
    /// Milliseconds the caller should wait before retrying.
    /// Computed from the oldest captured timestamp.
    pub retry_after_ms: u64,
}

impl Default for EscalationThrottle {
    fn default() -> Self {
        // 3 per scope per rolling hour — operator-facing default
        // tuned in the 82.14 spec, conservative on purpose.
        Self::new(3_600_000, 3)
    }
}

impl EscalationThrottle {
    /// Build with a custom window + cap. `window_ms = 60_000` and
    /// `cap = 1` for tests that want a tight gate; production
    /// uses [`Default`] (3/h).
    pub fn new(window_ms: u64, cap: u32) -> Self {
        Self {
            window_ms,
            cap,
            history: std::sync::Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Try to record one escalation at `now_ms`. Returns
    /// `Ok(remaining_after)` when admitted, where
    /// `remaining_after` is how many escalations the scope still
    /// has in this window (0 means the next call will deny).
    /// Returns `Err(ThrottleDenied)` when the rolling window
    /// already has `cap` entries — caller surfaces the
    /// `retry_after_ms` to the agent.
    pub fn try_acquire(
        &self,
        scope: &ProcessingScope,
        now_ms: u64,
    ) -> Result<u32, ThrottleDenied> {
        let mut entry = self
            .history
            .entry(scope.clone())
            .or_insert_with(std::collections::VecDeque::new);
        // Prune stamps that fell out of the window.
        let cutoff = now_ms.saturating_sub(self.window_ms);
        while let Some(&front) = entry.front() {
            if front < cutoff {
                entry.pop_front();
            } else {
                break;
            }
        }
        if (entry.len() as u32) >= self.cap {
            // Oldest in-window stamp + window = first moment a
            // slot frees. Saturating arithmetic keeps the math
            // safe across clock edge cases.
            let oldest = entry.front().copied().unwrap_or(now_ms);
            let retry_after_ms = oldest
                .saturating_add(self.window_ms)
                .saturating_sub(now_ms);
            return Err(ThrottleDenied {
                cap: self.cap,
                window_ms: self.window_ms,
                retry_after_ms,
            });
        }
        entry.push_back(now_ms);
        Ok(self.cap.saturating_sub(entry.len() as u32))
    }

    /// Drop the per-scope window after a successful resolve so
    /// the next legitimate escalation on the same scope starts
    /// fresh. Production wires this from the resolve handler;
    /// tests call directly.
    pub fn forget(&self, scope: &ProcessingScope) {
        self.history.remove(scope);
    }

    /// Active scope count — observability only (boot diagnostics
    /// + tests).
    pub fn tracked_scopes(&self) -> usize {
        self.history.len()
    }
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
        let result = list(&store, None, serde_json::json!({})).await;
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
            None,
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
            None,
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
            None,
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
            None,
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

    /// Phase 83.8.12.4.b — minimal `YamlPatcher` mock used to feed
    /// `agent_tenant_id` lookups during the tenant-filter tests.
    /// Maps `agent_id → tenant_id` via a static table; only the
    /// `tenant_id` dotted field is honoured.
    #[derive(Debug, Default)]
    struct MockTenantPatcher {
        tenants: std::collections::HashMap<String, Option<String>>,
    }

    impl super::super::agents::YamlPatcher for MockTenantPatcher {
        fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.tenants.keys().cloned().collect())
        }
        fn read_agent_field(
            &self,
            agent_id: &str,
            dotted: &str,
        ) -> anyhow::Result<Option<Value>> {
            if dotted == "tenant_id" {
                if let Some(t) = self.tenants.get(agent_id) {
                    return Ok(t.as_ref().map(|s| Value::String(s.clone())));
                }
            }
            Ok(None)
        }
        fn upsert_agent_field(
            &self,
            _: &str,
            _: &str,
            _: Value,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("upsert not used in escalations tests"))
        }
        fn remove_agent(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn pending_state_for(agent_id: &str) -> EscalationState {
        EscalationState::Pending {
            scope: ProcessingScope::Conversation {
                agent_id: agent_id.into(),
                channel: "whatsapp".into(),
                account_id: "acc".into(),
                contact_id: "wa.55".into(),
                mcp_channel_source: None,
            },
            summary: format!("{agent_id} needs human"),
            reason: EscalationReason::OutOfScope,
            urgency: EscalationUrgency::High,
            context: BTreeMap::new(),
            requested_at_ms: 1_700_000_000_000,
        }
    }

    #[tokio::test]
    async fn list_filters_by_tenant_when_patcher_wired() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state_for("ana"))
            .await
            .unwrap();
        store
            .upsert_pending("bob".into(), pending_state_for("bob"))
            .await
            .unwrap();
        let mut patcher = MockTenantPatcher::default();
        patcher.tenants.insert("ana".into(), Some("acme".into()));
        patcher.tenants.insert("bob".into(), Some("globex".into()));

        let result = list(
            &store,
            Some(&patcher),
            serde_json::json!({ "tenant_id": "acme" }),
        )
        .await;
        let resp: EscalationsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(resp.entries.len(), 1, "only ana (tenant=acme) survives");
        assert_eq!(resp.entries[0].agent_id, "ana");
    }

    #[tokio::test]
    async fn list_filter_passes_through_when_patcher_absent() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        // Tenant filter requested but no patcher available → row
        // returned unfiltered (back-compat behavior; production
        // wires the patcher always).
        let result = list(
            &store,
            None,
            serde_json::json!({ "tenant_id": "acme" }),
        )
        .await;
        let resp: EscalationsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(resp.entries.len(), 1, "no patcher → no filter");
    }

    #[tokio::test]
    async fn list_filters_excludes_agents_without_tenant_id() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let mut patcher = MockTenantPatcher::default();
        patcher.tenants.insert("ana".into(), None); // agent has no tenant
        let result = list(
            &store,
            Some(&patcher),
            serde_json::json!({ "tenant_id": "acme" }),
        )
        .await;
        let resp: EscalationsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(
            resp.entries.is_empty(),
            "agents without tenant_id must filter out (defense-in-depth)"
        );
    }

    /// Phase 82.14.b — minimal `AgentEventEmitter` mock that
    /// captures emitted events into a Vec for the firehose
    /// emit assertions.
    #[derive(Debug, Default)]
    struct CaptureEmitter {
        events: std::sync::Mutex<Vec<nexo_tool_meta::admin::agent_events::AgentEventKind>>,
    }

    #[async_trait]
    impl crate::agent::agent_events::AgentEventEmitter for CaptureEmitter {
        async fn emit(
            &self,
            event: nexo_tool_meta::admin::agent_events::AgentEventKind,
        ) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn resolve_emits_escalation_resolved_when_changed() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let emitter: std::sync::Arc<dyn crate::agent::agent_events::AgentEventEmitter> =
            std::sync::Arc::new(CaptureEmitter::default());
        let _r = resolve(
            &store,
            Some(&emitter),
            serde_json::json!({
                "scope": convo(),
                "by": "takeover",
                "operator_token_hash": "h",
            }),
        )
        .await;
        // Reach into the concrete capture buffer via downcast
        // through Arc::strong_count guard? Simpler: rebuild a
        // local emitter, bypass the trait object dance.
        let cap = std::sync::Arc::new(CaptureEmitter::default());
        let cap_dyn: std::sync::Arc<dyn crate::agent::agent_events::AgentEventEmitter> =
            cap.clone();
        let store2 = MockStore::default();
        store2
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let _ = resolve(
            &store2,
            Some(&cap_dyn),
            serde_json::json!({
                "scope": convo(),
                "by": "takeover",
                "operator_token_hash": "h",
            }),
        )
        .await;
        let events = cap.events.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "exactly one EscalationResolved fired");
        match &events[0] {
            nexo_tool_meta::admin::agent_events::AgentEventKind::EscalationResolved {
                agent_id,
                by,
                ..
            } => {
                assert_eq!(agent_id, "ana");
                assert!(matches!(by, ResolvedBy::OperatorTakeover));
            }
            other => panic!("unexpected emit: {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_skips_emit_when_no_change() {
        // Resolve a scope that has nothing pending → store
        // returns false, emitter stays untouched.
        let store = MockStore::default();
        let cap = std::sync::Arc::new(CaptureEmitter::default());
        let cap_dyn: std::sync::Arc<dyn crate::agent::agent_events::AgentEventEmitter> =
            cap.clone();
        let _ = resolve(
            &store,
            Some(&cap_dyn),
            serde_json::json!({
                "scope": convo(),
                "by": "takeover",
                "operator_token_hash": "h",
            }),
        )
        .await;
        assert!(
            cap.events.lock().unwrap().is_empty(),
            "no-op resolve must not emit (subscribers would see phantom events)"
        );
    }

    #[tokio::test]
    async fn auto_resolve_on_pause_emits_when_pending_existed() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let cap = std::sync::Arc::new(CaptureEmitter::default());
        let cap_dyn: std::sync::Arc<dyn crate::agent::agent_events::AgentEventEmitter> =
            cap.clone();
        let changed = auto_resolve_on_pause(&store, Some(&cap_dyn), &convo())
            .await
            .unwrap();
        assert!(changed);
        let events = cap.events.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        match &events[0] {
            nexo_tool_meta::admin::agent_events::AgentEventKind::EscalationResolved {
                by, ..
            } => {
                assert!(matches!(by, ResolvedBy::OperatorTakeover));
            }
            other => panic!("unexpected emit: {other:?}"),
        }
    }

    #[tokio::test]
    async fn auto_resolve_on_pause_flips_pending_to_takeover() {
        let store = MockStore::default();
        store
            .upsert_pending("ana".into(), pending_state())
            .await
            .unwrap();
        let changed = auto_resolve_on_pause(&store, None, &convo()).await.unwrap();
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

    // ── Phase 82.14.b — escalation throttle primitive ──────────

    fn other_convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.99".into(),
            mcp_channel_source: None,
        }
    }

    #[test]
    fn throttle_default_admits_first_three_then_denies_fourth() {
        let throttle = EscalationThrottle::default();
        let scope = convo();
        // Cap = 3 within the rolling hour. Every call shares
        // the same now_ms so the window window doesn't help
        // the 4th call.
        let now = 1_700_000_000_000u64;
        assert_eq!(throttle.try_acquire(&scope, now).unwrap(), 2);
        assert_eq!(throttle.try_acquire(&scope, now).unwrap(), 1);
        assert_eq!(throttle.try_acquire(&scope, now).unwrap(), 0);
        let denied = throttle.try_acquire(&scope, now).unwrap_err();
        assert_eq!(denied.cap, 3);
        assert_eq!(denied.window_ms, 3_600_000);
        // Oldest entry was at `now`; window slides at
        // `now + 3_600_000` → retry_after_ms == window_ms.
        assert_eq!(denied.retry_after_ms, 3_600_000);
    }

    #[test]
    fn throttle_window_slides_so_old_entries_dont_count() {
        // 60s window, cap 2.
        let throttle = EscalationThrottle::new(60_000, 2);
        let scope = convo();
        // Two acquires at t=0, fully occupying the window.
        throttle.try_acquire(&scope, 0).unwrap();
        throttle.try_acquire(&scope, 1_000).unwrap();
        // 4th acquire at t=70_000 — well past the window for
        // the first entry, just past for the second. The
        // second entry (at t=1_000) is now > 60s old, so the
        // window is empty and we admit.
        assert_eq!(throttle.try_acquire(&scope, 70_000).unwrap(), 1);
    }

    #[test]
    fn throttle_per_scope_independent() {
        let throttle = EscalationThrottle::new(60_000, 1);
        let now = 1_000u64;
        // First scope eats its only slot.
        throttle.try_acquire(&convo(), now).unwrap();
        assert!(throttle.try_acquire(&convo(), now).is_err());
        // Other scope still has its own slot.
        assert_eq!(
            throttle.try_acquire(&other_convo(), now).unwrap(),
            0,
            "other scope's window must NOT inherit the first's count"
        );
    }

    #[test]
    fn throttle_retry_after_is_first_moment_a_slot_frees() {
        let throttle = EscalationThrottle::new(60_000, 2);
        let scope = convo();
        throttle.try_acquire(&scope, 0).unwrap();
        throttle.try_acquire(&scope, 10_000).unwrap();
        // 3rd at t=15_000: oldest is at 0, window=60_000, so
        // first slot frees at t=60_000 → retry_after = 45_000.
        let denied = throttle.try_acquire(&scope, 15_000).unwrap_err();
        assert_eq!(denied.retry_after_ms, 45_000);
    }

    #[test]
    fn throttle_forget_resets_scope_history() {
        let throttle = EscalationThrottle::new(60_000, 1);
        let scope = convo();
        throttle.try_acquire(&scope, 0).unwrap();
        assert!(throttle.try_acquire(&scope, 1).is_err());
        throttle.forget(&scope);
        // Reset → next acquire admits.
        assert_eq!(throttle.try_acquire(&scope, 2).unwrap(), 0);
    }

    #[test]
    fn throttle_zero_cap_denies_every_call() {
        let throttle = EscalationThrottle::new(60_000, 0);
        let denied = throttle.try_acquire(&convo(), 0).unwrap_err();
        assert_eq!(denied.cap, 0);
    }

    #[test]
    fn throttle_tracked_scopes_increments_per_distinct_scope() {
        let throttle = EscalationThrottle::default();
        assert_eq!(throttle.tracked_scopes(), 0);
        throttle.try_acquire(&convo(), 0).unwrap();
        assert_eq!(throttle.tracked_scopes(), 1);
        throttle.try_acquire(&other_convo(), 0).unwrap();
        assert_eq!(throttle.tracked_scopes(), 2);
        // Same scope again: no new entry.
        throttle.try_acquire(&convo(), 1).unwrap();
        assert_eq!(throttle.tracked_scopes(), 2);
    }
}
