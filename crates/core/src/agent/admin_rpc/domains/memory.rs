//! Phase 90.x.memory — `nexo/admin/memory/*` handlers.
//!
//! v1 wraps text search over the long-term memory store
//! (`recall`). Snapshot / restore deferred — the
//! `agent memory snapshot` CLI still owns those flows.

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::memory::{
    MemoryEntryWire, MemoryQueryParams, MemoryQueryResponse,
    MemorySnapshotsDeleteParams, MemorySnapshotsDeleteResponse,
    MemorySnapshotsListParams, MemorySnapshotsListResponse, SnapshotMetaWire,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Server-side limit clamp. 0 / out-of-range coerces to 20.
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

/// Reader abstraction over the daemon's long-term memory store.
/// Production adapter wraps `nexo_memory::LongTermMemory`; tests
/// inject in-memory fakes.
#[async_trait]
pub trait MemoryReader: Send + Sync + std::fmt::Debug {
    /// Recall up to `limit` entries for `agent_id` matching
    /// `query`. Empty query returns recent entries.
    async fn query(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntryWire>>;
}

/// Reader abstraction over the snapshot bundle store. Production
/// adapter wraps `nexo_memory_snapshot::MemorySnapshotter`; tests
/// inject in-memory fakes.
#[async_trait]
pub trait MemorySnapshotReader: Send + Sync + std::fmt::Debug {
    /// Enumerate snapshots for `agent_id` under `tenant`.
    /// Newest-first (`created_at_ms` descending). Empty result
    /// = no snapshots OR snapshot subsystem disabled at boot.
    async fn list(
        &self,
        agent_id: &str,
        tenant: &str,
    ) -> anyhow::Result<Vec<SnapshotMetaWire>>;

    /// Remove one bundle. Idempotent — missing ids return Ok(()).
    async fn delete(
        &self,
        agent_id: &str,
        tenant: &str,
        snapshot_id: &str,
    ) -> anyhow::Result<()>;
}

/// `nexo/admin/memory/query` — recall.
pub async fn query(reader: &dyn MemoryReader, params: Value) -> AdminRpcResult {
    let p: MemoryQueryParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.agent_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "agent_id is empty".into(),
        ));
    }
    let limit = clamp_limit(p.limit);
    match reader.query(&p.agent_id, &p.query, limit).await {
        Ok(entries) => {
            let resp = MemoryQueryResponse { entries };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "memory.query: {e}"
        ))),
    }
}

/// `nexo/admin/memory/list_snapshots` — snapshot inventory.
pub async fn list_snapshots(
    reader: &dyn MemorySnapshotReader,
    params: Value,
) -> AdminRpcResult {
    let p: MemorySnapshotsListParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.agent_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "agent_id is empty".into(),
        ));
    }
    let tenant = if p.tenant.trim().is_empty() {
        "default"
    } else {
        p.tenant.as_str()
    };
    match reader.list(&p.agent_id, tenant).await {
        Ok(snapshots) => {
            let resp = MemorySnapshotsListResponse { snapshots };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "memory.list_snapshots: {e}"
        ))),
    }
}

/// `nexo/admin/memory/delete_snapshot` — idempotent removal.
pub async fn delete_snapshot(
    reader: &dyn MemorySnapshotReader,
    params: Value,
) -> AdminRpcResult {
    let p: MemorySnapshotsDeleteParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.agent_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "agent_id is empty".into(),
        ));
    }
    if p.id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "id is empty".into(),
        ));
    }
    let tenant = if p.tenant.trim().is_empty() {
        "default"
    } else {
        p.tenant.as_str()
    };
    match reader.delete(&p.agent_id, tenant, &p.id).await {
        Ok(()) => {
            let resp = MemorySnapshotsDeleteResponse { removed: true };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "memory.delete_snapshot: {e}"
        ))),
    }
}

fn clamp_limit(raw: usize) -> usize {
    if raw == 0 {
        DEFAULT_LIMIT
    } else if raw > MAX_LIMIT {
        MAX_LIMIT
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct StubReader {
        rows: Vec<MemoryEntryWire>,
    }

    #[async_trait]
    impl MemoryReader for StubReader {
        async fn query(
            &self,
            _agent_id: &str,
            _query: &str,
            limit: usize,
        ) -> anyhow::Result<Vec<MemoryEntryWire>> {
            Ok(self.rows.iter().take(limit).cloned().collect())
        }
    }

    #[derive(Debug)]
    struct ErrReader;

    #[async_trait]
    impl MemoryReader for ErrReader {
        async fn query(
            &self,
            _agent_id: &str,
            _query: &str,
            _limit: usize,
        ) -> anyhow::Result<Vec<MemoryEntryWire>> {
            anyhow::bail!("simulated db error")
        }
    }

    fn entry(id: &str) -> MemoryEntryWire {
        MemoryEntryWire {
            id: id.into(),
            agent_id: "ana".into(),
            content: format!("entry {id}"),
            tags: vec![],
            concept_tags: vec![],
            created_at: "2026-05-10T00:00:00Z".into(),
            memory_type: None,
        }
    }

    #[derive(Debug)]
    struct StubSnapshotReader {
        list_returns: Vec<SnapshotMetaWire>,
        deleted: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    #[async_trait]
    impl MemorySnapshotReader for StubSnapshotReader {
        async fn list(
            &self,
            _agent_id: &str,
            _tenant: &str,
        ) -> anyhow::Result<Vec<SnapshotMetaWire>> {
            Ok(self.list_returns.clone())
        }
        async fn delete(
            &self,
            agent_id: &str,
            tenant: &str,
            snapshot_id: &str,
        ) -> anyhow::Result<()> {
            self.deleted.lock().unwrap().push((
                agent_id.to_string(),
                tenant.to_string(),
                snapshot_id.to_string(),
            ));
            Ok(())
        }
    }

    fn snap_meta(id: &str) -> SnapshotMetaWire {
        SnapshotMetaWire {
            id: id.into(),
            agent_id: "ana".into(),
            tenant: "default".into(),
            label: None,
            created_at_ms: 1_000_000,
            bundle_path: format!("/snap/{id}.tar.zst"),
            bundle_size_bytes: 1024,
            bundle_sha256: "deadbeef".into(),
            git_oid: None,
            encrypted: false,
            redactions_applied: false,
        }
    }

    #[tokio::test]
    async fn list_snapshots_happy() {
        let reader = StubSnapshotReader {
            list_returns: vec![snap_meta("a"), snap_meta("b")],
            deleted: std::sync::Mutex::new(Vec::new()),
        };
        let res = list_snapshots(
            &reader,
            serde_json::json!({"agent_id": "ana"}),
        )
        .await;
        let payload = res.result.expect("ok");
        let snaps = payload["snapshots"].as_array().unwrap();
        assert_eq!(snaps.len(), 2);
    }

    #[tokio::test]
    async fn list_snapshots_rejects_empty_agent_id() {
        let reader = StubSnapshotReader {
            list_returns: vec![],
            deleted: std::sync::Mutex::new(Vec::new()),
        };
        let res = list_snapshots(
            &reader,
            serde_json::json!({"agent_id": ""}),
        )
        .await;
        assert!(res.error.is_some());
    }

    #[tokio::test]
    async fn list_snapshots_defaults_empty_tenant_to_default() {
        let reader = StubSnapshotReader {
            list_returns: vec![],
            deleted: std::sync::Mutex::new(Vec::new()),
        };
        // No tenant in params + non-empty agent_id reaches the
        // adapter; the test stub doesn't capture tenant on list,
        // so we just verify the call shape doesn't error.
        let res = list_snapshots(
            &reader,
            serde_json::json!({"agent_id": "ana"}),
        )
        .await;
        assert!(res.error.is_none());
    }

    #[tokio::test]
    async fn delete_snapshot_records_call() {
        let reader = StubSnapshotReader {
            list_returns: vec![],
            deleted: std::sync::Mutex::new(Vec::new()),
        };
        let res = delete_snapshot(
            &reader,
            serde_json::json!({"agent_id": "ana", "id": "abc"}),
        )
        .await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["removed"], true);
        let recorded = reader.deleted.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0], ("ana".into(), "default".into(), "abc".into()));
    }

    #[tokio::test]
    async fn delete_snapshot_rejects_empty_id() {
        let reader = StubSnapshotReader {
            list_returns: vec![],
            deleted: std::sync::Mutex::new(Vec::new()),
        };
        let res = delete_snapshot(
            &reader,
            serde_json::json!({"agent_id": "ana", "id": ""}),
        )
        .await;
        assert!(res.error.is_some());
    }

    #[tokio::test]
    async fn query_with_default_limit_clamps_to_20() {
        let reader = StubReader {
            rows: (0..50).map(|i| entry(&i.to_string())).collect(),
        };
        let res = query(
            &reader,
            serde_json::json!({"agent_id": "ana", "query": "", "limit": 0}),
        )
        .await;
        let payload = res.result.expect("ok");
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 20);
    }

    #[tokio::test]
    async fn query_clamps_above_max() {
        let reader = StubReader {
            rows: (0..200).map(|i| entry(&i.to_string())).collect(),
        };
        let res = query(
            &reader,
            serde_json::json!({"agent_id": "ana", "limit": 9999}),
        )
        .await;
        let payload = res.result.expect("ok");
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 100);
    }

    #[tokio::test]
    async fn query_rejects_empty_agent_id() {
        let reader = StubReader { rows: vec![] };
        let res = query(
            &reader,
            serde_json::json!({"agent_id": "", "query": "x"}),
        )
        .await;
        assert!(res.error.is_some());
    }

    #[tokio::test]
    async fn query_surfaces_internal_error() {
        let res = query(
            &ErrReader,
            serde_json::json!({"agent_id": "ana", "query": "x"}),
        )
        .await;
        assert!(res.error.is_some());
    }

    #[test]
    fn clamp_limit_zero_returns_default() {
        assert_eq!(clamp_limit(0), DEFAULT_LIMIT);
    }

    #[test]
    fn clamp_limit_huge_returns_max() {
        assert_eq!(clamp_limit(usize::MAX), MAX_LIMIT);
    }

    #[test]
    fn clamp_limit_passes_through_in_range() {
        assert_eq!(clamp_limit(50), 50);
    }
}
