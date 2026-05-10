//! Phase 90.x.memory — `nexo/admin/memory/*` handlers.
//!
//! Surface (Phase 90.x.memory + 90.x.memory-snapshot.create-restore):
//!
//! - `query` — text search over long-term memory store (`recall`).
//! - `list_snapshots` / `delete_snapshot` — bundle inventory +
//!   idempotent removal (Phase 90.x.memory-snapshot list/delete).
//! - `create_snapshot` / `restore_snapshot` — capture + restore
//!   bundles (Phase 90.x.memory-snapshot.create-restore). Defaults
//!   forced server-side: `redact_secrets=true`,
//!   `auto_pre_snapshot=true`, `created_by="admin-ui"`. Encryption
//!   recipient resolved from `memory.snapshot.encryption.recipients`
//!   YAML; UI surfaces availability via the `encryption_available`
//!   bit on list responses.

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::memory::{
    MemoryEntryWire, MemoryQueryParams, MemoryQueryResponse,
    MemorySnapshotsCreateParams, MemorySnapshotsCreateResponse,
    MemorySnapshotsDeleteParams, MemorySnapshotsDeleteResponse,
    MemorySnapshotsListParams, MemorySnapshotsListResponse,
    MemorySnapshotsRestoreParams, MemorySnapshotsRestoreResponse,
    RestoreReportWire, SnapshotMetaWire,
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
///
/// NAMING NOTE (Phase 90.x.memory-snapshot.create-restore): the
/// trait is named `…Reader` but now also exposes `create()` and
/// `restore()` — write operations. The mismatch is intentional
/// debt rather than a rename: renaming would break `nexo-core`
/// 0.1.x consumers that already pin
/// `Arc<dyn MemorySnapshotReader>`. Slated for renaming at the
/// next major bump (logged in `proyecto/FOLLOWUPS.md`).
#[async_trait]
pub trait MemorySnapshotReader: Send + Sync + std::fmt::Debug {
    /// Enumerate snapshots for `agent_id` under `tenant` plus
    /// the daemon's encryption availability flag (so the SPA can
    /// disable the create-modal encrypt toggle when no
    /// recipients are configured).
    ///
    /// `MemorySnapshotsListResponse.snapshots` is ordered
    /// newest-first (`created_at_ms` descending). Empty result =
    /// no snapshots OR snapshot subsystem disabled at boot.
    async fn list(
        &self,
        agent_id: &str,
        tenant: &str,
    ) -> anyhow::Result<MemorySnapshotsListResponse>;

    /// Remove one bundle. Idempotent — missing ids return Ok(()).
    async fn delete(
        &self,
        agent_id: &str,
        tenant: &str,
        snapshot_id: &str,
    ) -> anyhow::Result<()>;

    /// Capture a fresh bundle. `encrypt=true` requires server-side
    /// recipients; adapters MUST reject with an error containing
    /// "encryption requested but no recipients configured" when
    /// the flag is set without recipients (the handler maps that
    /// substring back to `InvalidParams` so callers can branch).
    /// Defaults forced server-side by adapters:
    /// `redact_secrets=true`, `created_by="admin-ui"`.
    async fn create(
        &self,
        agent_id: &str,
        tenant: &str,
        label: Option<&str>,
        encrypt: bool,
    ) -> anyhow::Result<SnapshotMetaWire>;

    /// Restore from an existing snapshot id. Adapter contract:
    ///
    ///   1. `list()` lookup → match by `snapshot_id`; missing id
    ///      = error with "snapshot {id} not found" so the
    ///      handler can map to `InvalidParams` (stale list, not
    ///      a bug).
    ///   2. Validate `meta.tenant == tenant_param`; mismatch =
    ///      error with both tenants quoted so the handler maps
    ///      to `InvalidParams`.
    ///   3. Resolve `DecryptionIdentity` from operator config
    ///      when the meta says `encrypted=true`; missing
    ///      `identity_path` = `Internal` ("encrypted but no
    ///      identity_path configured; restore via CLI").
    ///   4. Construct `RestoreRequest { dry_run, auto_pre_snapshot:
    ///      true, ... }` and call the underlying snapshotter.
    ///   5. Project the `RestoreReport` → `RestoreReportWire`.
    async fn restore(
        &self,
        agent_id: &str,
        tenant: &str,
        snapshot_id: &str,
        dry_run: bool,
    ) -> anyhow::Result<RestoreReportWire>;
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
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
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

/// `nexo/admin/memory/create_snapshot` — capture fresh bundle.
///
/// Adapter is responsible for forcing `redact_secrets=true`,
/// `created_by="admin-ui"`, and resolving the encryption recipient
/// from operator config when `encrypt=true`. The handler maps the
/// recognised "encryption requested but no recipients configured"
/// substring back to `InvalidParams` so the SPA can surface a
/// targeted error instead of a generic Internal.
pub async fn create_snapshot(
    reader: &dyn MemorySnapshotReader,
    params: Value,
) -> AdminRpcResult {
    let p: MemorySnapshotsCreateParams = match serde_json::from_value(params) {
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
    let label = p.label.as_deref().filter(|s| !s.trim().is_empty());
    match reader
        .create(&p.agent_id, tenant, label, p.encrypt)
        .await
    {
        Ok(snapshot) => {
            let resp = MemorySnapshotsCreateResponse { snapshot };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => {
            let msg = e.to_string();
            // The adapter's encryption-without-recipients path is
            // user-recoverable (toggle off encrypt, or configure
            // recipients) — surface as InvalidParams so the SPA
            // doesn't show a generic 500.
            if msg.contains("encryption requested but no recipients configured") {
                AdminRpcResult::err(AdminRpcError::InvalidParams(msg))
            } else {
                AdminRpcResult::err(AdminRpcError::Internal(format!(
                    "memory.create_snapshot: {msg}"
                )))
            }
        }
    }
}

/// `nexo/admin/memory/restore_snapshot` — restore from snapshot id.
///
/// `tenant` is REQUIRED (defensive intent) — the adapter rejects
/// when it doesn't match the bundle manifest's recorded tenant.
/// `dry_run=true` returns the would-be diff without mutating live
/// state. The adapter forces `auto_pre_snapshot=true` for
/// destructive runs so every restore is reversible. The
/// "snapshot {id} not found" + "belongs to tenant" + "specified"
/// substrings are mapped back to `InvalidParams` so SPA branches
/// on user-recoverable errors versus Internal.
pub async fn restore_snapshot(
    reader: &dyn MemorySnapshotReader,
    params: Value,
) -> AdminRpcResult {
    let p: MemorySnapshotsRestoreParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if p.agent_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "agent_id is empty".into(),
        ));
    }
    if p.tenant.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "tenant is empty (required for restore — guards against \
             accidental cross-tenant restore)"
                .into(),
        ));
    }
    if p.snapshot_id.trim().is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "snapshot_id is empty".into(),
        ));
    }
    match reader
        .restore(&p.agent_id, &p.tenant, &p.snapshot_id, p.dry_run)
        .await
    {
        Ok(report) => {
            let resp = MemorySnapshotsRestoreResponse { report };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => {
            let msg = e.to_string();
            // User-recoverable: stale snapshot id or tenant typo.
            if msg.contains("not found") || msg.contains("belongs to tenant") {
                AdminRpcResult::err(AdminRpcError::InvalidParams(msg))
            } else {
                AdminRpcResult::err(AdminRpcError::Internal(format!(
                    "memory.restore_snapshot: {msg}"
                )))
            }
        }
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
        encryption_available: bool,
        deleted: std::sync::Mutex<Vec<(String, String, String)>>,
        // (agent_id, tenant, label, encrypt) captured for create.
        created: std::sync::Mutex<Vec<(String, String, Option<String>, bool)>>,
        // (agent_id, tenant, snapshot_id, dry_run) captured for restore.
        restored: std::sync::Mutex<Vec<(String, String, String, bool)>>,
        // Optional preset error injected on next create() — when
        // Some, returns Err(this); else returns synthesized
        // SnapshotMetaWire mirroring inputs.
        create_err: std::sync::Mutex<Option<String>>,
        // Optional preset error for restore() — same pattern.
        restore_err: std::sync::Mutex<Option<String>>,
    }

    impl StubSnapshotReader {
        fn empty() -> Self {
            Self {
                list_returns: vec![],
                encryption_available: false,
                deleted: std::sync::Mutex::new(Vec::new()),
                created: std::sync::Mutex::new(Vec::new()),
                restored: std::sync::Mutex::new(Vec::new()),
                create_err: std::sync::Mutex::new(None),
                restore_err: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl MemorySnapshotReader for StubSnapshotReader {
        async fn list(
            &self,
            _agent_id: &str,
            _tenant: &str,
        ) -> anyhow::Result<MemorySnapshotsListResponse> {
            Ok(MemorySnapshotsListResponse {
                snapshots: self.list_returns.clone(),
                encryption_available: self.encryption_available,
            })
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
        async fn create(
            &self,
            agent_id: &str,
            tenant: &str,
            label: Option<&str>,
            encrypt: bool,
        ) -> anyhow::Result<SnapshotMetaWire> {
            self.created.lock().unwrap().push((
                agent_id.to_string(),
                tenant.to_string(),
                label.map(|s| s.to_string()),
                encrypt,
            ));
            if let Some(msg) = self.create_err.lock().unwrap().take() {
                anyhow::bail!(msg);
            }
            Ok(SnapshotMetaWire {
                id: "stub-created".into(),
                agent_id: agent_id.into(),
                tenant: tenant.into(),
                label: label.map(|s| s.to_string()),
                created_at_ms: 1_700_000_000_000,
                bundle_path: format!("/tmp/{agent_id}-stub.tar.zst"),
                bundle_size_bytes: 2048,
                bundle_sha256: "00".repeat(32),
                git_oid: None,
                encrypted: encrypt,
                redactions_applied: true,
            })
        }
        async fn restore(
            &self,
            agent_id: &str,
            tenant: &str,
            snapshot_id: &str,
            dry_run: bool,
        ) -> anyhow::Result<RestoreReportWire> {
            self.restored.lock().unwrap().push((
                agent_id.to_string(),
                tenant.to_string(),
                snapshot_id.to_string(),
                dry_run,
            ));
            if let Some(msg) = self.restore_err.lock().unwrap().take() {
                anyhow::bail!(msg);
            }
            Ok(RestoreReportWire {
                agent_id: agent_id.into(),
                from_snapshot_id: snapshot_id.into(),
                pre_snapshot_id: if dry_run { None } else { Some("stub-pre".into()) },
                git_reset_oid: None,
                sqlite_restored_dbs: vec!["long_term.db".into()],
                state_files_restored: vec!["extract_cursor".into()],
                workers_restarted: !dry_run,
                dry_run,
            })
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
            ..StubSnapshotReader::empty()
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
        let reader = StubSnapshotReader::empty();
        let res = list_snapshots(
            &reader,
            serde_json::json!({"agent_id": ""}),
        )
        .await;
        assert!(res.error.is_some());
    }

    #[tokio::test]
    async fn list_snapshots_defaults_empty_tenant_to_default() {
        let reader = StubSnapshotReader::empty();
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
        let reader = StubSnapshotReader::empty();
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
        let reader = StubSnapshotReader::empty();
        let res = delete_snapshot(
            &reader,
            serde_json::json!({"agent_id": "ana", "id": ""}),
        )
        .await;
        assert!(res.error.is_some());
    }

    // ── Phase 90.x.memory-snapshot.create-restore tests ──

    #[tokio::test]
    async fn create_snapshot_records_call() {
        let reader = StubSnapshotReader::empty();
        let res = create_snapshot(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "default",
                "label": "pre-deploy",
                "encrypt": true
            }),
        )
        .await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["snapshot"]["agent_id"], "ana");
        assert_eq!(payload["snapshot"]["label"], "pre-deploy");
        assert_eq!(payload["snapshot"]["encrypted"], true);
        let recorded = reader.created.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0],
            (
                "ana".into(),
                "default".into(),
                Some("pre-deploy".into()),
                true
            )
        );
    }

    #[tokio::test]
    async fn create_snapshot_rejects_empty_agent_id() {
        let reader = StubSnapshotReader::empty();
        let res = create_snapshot(
            &reader,
            serde_json::json!({"agent_id": ""}),
        )
        .await;
        assert!(res.error.is_some());
        // Reader was never called.
        assert!(reader.created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_snapshot_maps_no_recipients_error_to_invalid_params() {
        let reader = StubSnapshotReader::empty();
        *reader.create_err.lock().unwrap() =
            Some("encryption requested but no recipients configured".into());
        let res = create_snapshot(
            &reader,
            serde_json::json!({"agent_id": "ana", "encrypt": true}),
        )
        .await;
        let err = res.error.expect("err");
        // -32602 = JSON-RPC invalid params (matches AdminRpcError::InvalidParams).
        assert_eq!(err.code(), -32602);
        let msg = err.to_string();
        assert!(
            msg.contains("no recipients configured"),
            "actual: {msg}"
        );
    }

    #[tokio::test]
    async fn restore_snapshot_records_call_with_dry_run() {
        let reader = StubSnapshotReader::empty();
        let res = restore_snapshot(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "default",
                "snapshot_id": "abc12345",
                "dry_run": true
            }),
        )
        .await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["report"]["dry_run"], true);
        assert_eq!(payload["report"]["from_snapshot_id"], "abc12345");
        // dry_run never captures pre-snapshot.
        assert!(payload["report"].get("pre_snapshot_id").is_none());
        let recorded = reader.restored.lock().unwrap().clone();
        assert_eq!(recorded[0].3, true);
    }

    #[tokio::test]
    async fn restore_snapshot_rejects_empty_tenant() {
        let reader = StubSnapshotReader::empty();
        let res = restore_snapshot(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "",
                "snapshot_id": "abc"
            }),
        )
        .await;
        let err = res.error.expect("err");
        let msg = err.to_string();
        assert!(msg.contains("tenant is empty"), "actual: {msg}");
        assert!(reader.restored.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn restore_snapshot_rejects_empty_snapshot_id() {
        let reader = StubSnapshotReader::empty();
        let res = restore_snapshot(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "default",
                "snapshot_id": ""
            }),
        )
        .await;
        let err = res.error.expect("err");
        let msg = err.to_string();
        assert!(msg.contains("snapshot_id is empty"), "actual: {msg}");
    }

    #[tokio::test]
    async fn restore_snapshot_maps_not_found_to_invalid_params() {
        let reader = StubSnapshotReader::empty();
        *reader.restore_err.lock().unwrap() =
            Some("snapshot abc not found".into());
        let res = restore_snapshot(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "default",
                "snapshot_id": "abc"
            }),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn restore_snapshot_maps_tenant_mismatch_to_invalid_params() {
        let reader = StubSnapshotReader::empty();
        *reader.restore_err.lock().unwrap() = Some(
            "snapshot abc belongs to tenant `staging`, request specified `prod`".into(),
        );
        let res = restore_snapshot(
            &reader,
            serde_json::json!({
                "agent_id": "ana",
                "tenant": "prod",
                "snapshot_id": "abc"
            }),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("belongs to tenant"));
    }

    #[tokio::test]
    async fn list_response_carries_encryption_available_flag() {
        let reader = StubSnapshotReader {
            list_returns: vec![],
            encryption_available: true,
            ..StubSnapshotReader::empty()
        };
        let res = list_snapshots(
            &reader,
            serde_json::json!({"agent_id": "ana"}),
        )
        .await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["encryption_available"], true);
        assert!(payload["snapshots"].is_array());
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
