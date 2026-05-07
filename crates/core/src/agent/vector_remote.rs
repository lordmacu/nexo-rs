//! Phase 81.26 — `VectorBackend` impl backed by a subprocess
//! plugin's stdio bridge. One instance is registered per backend
//! name listed in `manifest.plugin.extends.memory_backends`.
//!
//! Wire methods (contract v1.8.0):
//!
//! - `memory.vector_upsert { backend, collection, records }` →
//!   `UpsertAck`
//! - `memory.vector_search { backend, collection, query }` →
//!   `{ matches: [VectorMatch] }`
//! - `memory.vector_delete { backend, collection, ids }` →
//!   `DeleteAck`
//!
//! The `backend` field on every payload allows a single
//! subprocess to advertise multiple backends
//! (`extends.memory_backends = ["pinecone", "qdrant"]`) and
//! dispatch by name.
//!
//! Custom error codes `-33301..=-33304` map onto operator-
//! greppable `anyhow::Error` messages (no typed
//! `MemoryBackendError` enum in v1; defer to 81.26.c).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_memory::{DeleteAck, UpsertAck, VectorBackend, VectorMatch, VectorQuery, VectorRecord};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

const DEFAULT_UPSERT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_DELETE_TIMEOUT: Duration = Duration::from_secs(30);

/// `VectorBackend` impl backed by a subprocess plugin.
pub struct RemoteVectorBackend {
    name: String,
    plugin_id: String,
    stdin_tx: mpsc::Sender<Value>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: Arc<AtomicU64>,
    upsert_timeout: Duration,
    search_timeout: Duration,
    delete_timeout: Duration,
}

impl RemoteVectorBackend {
    pub fn new(
        name: String,
        plugin_id: String,
        stdin_tx: mpsc::Sender<Value>,
        pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
        next_id: Arc<AtomicU64>,
    ) -> Self {
        let (upsert_timeout, search_timeout, delete_timeout) = Self::resolve_timeouts();
        Self {
            name,
            plugin_id,
            stdin_tx,
            pending,
            next_id,
            upsert_timeout,
            search_timeout,
            delete_timeout,
        }
    }

    fn resolve_timeouts() -> (Duration, Duration, Duration) {
        let env_override = std::env::var("NEXO_PLUGIN_MEMORY_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis);
        match env_override {
            Some(t) => (t, t, t),
            None => (
                DEFAULT_UPSERT_TIMEOUT,
                DEFAULT_SEARCH_TIMEOUT,
                DEFAULT_DELETE_TIMEOUT,
            ),
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    async fn send_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        if self.stdin_tx.send(frame).await.is_err() {
            self.pending.remove(&id);
            anyhow::bail!("backend {} stdin closed for {method}", self.name);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(err_str))) => Err(self.parse_error_string(&err_str, method)),
            Ok(Err(_)) => {
                self.pending.remove(&id);
                Err(anyhow::anyhow!(
                    "backend {} pending dropped (subprocess gone) for {method}",
                    self.name
                ))
            }
            Err(_) => {
                self.pending.remove(&id);
                Err(anyhow::anyhow!(
                    "backend {} {method} timed out after {}s",
                    self.name,
                    timeout.as_secs()
                ))
            }
        }
    }

    fn parse_error_string(&self, s: &str, method: &str) -> anyhow::Error {
        let parsed: Value = match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => {
                return anyhow::anyhow!("backend {} {method} error: {}", self.name, s);
            }
        };
        let code = parsed.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = parsed
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let data = parsed.get("data").cloned().unwrap_or(Value::Null);
        let backend = &self.name;

        match code {
            -32601 => anyhow::anyhow!("backend {backend} method not implemented: {message}"),
            -33301 => {
                let coll = data
                    .get("collection")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                anyhow::anyhow!("collection {coll} not found on backend {backend}")
            }
            -33302 => {
                let expected = data.get("expected").and_then(|v| v.as_u64()).unwrap_or(0);
                let got = data.get("got").and_then(|v| v.as_u64()).unwrap_or(0);
                anyhow::anyhow!("dimension mismatch: expected {expected}, got {got}")
            }
            -33303 => {
                let secs = data
                    .get("retry_after_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                anyhow::anyhow!("rate limited; retry after {secs}s")
            }
            -33304 => anyhow::anyhow!("write failed: {message}"),
            _ => anyhow::anyhow!("backend {backend} {method} error code {code}: {message}"),
        }
    }
}

#[async_trait]
impl VectorBackend for RemoteVectorBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn upsert(
        &self,
        collection: &str,
        records: Vec<VectorRecord>,
    ) -> anyhow::Result<UpsertAck> {
        let result = self
            .send_request(
                "memory.vector_upsert",
                serde_json::json!({
                    "backend": &self.name,
                    "collection": collection,
                    "records": records,
                }),
                self.upsert_timeout,
            )
            .await?;
        serde_json::from_value::<UpsertAck>(result)
            .map_err(|e| anyhow::anyhow!("decode UpsertAck: {e}"))
    }

    async fn search(
        &self,
        collection: &str,
        query: VectorQuery,
    ) -> anyhow::Result<Vec<VectorMatch>> {
        let result = self
            .send_request(
                "memory.vector_search",
                serde_json::json!({
                    "backend": &self.name,
                    "collection": collection,
                    "query": query,
                }),
                self.search_timeout,
            )
            .await?;
        // Wire wraps matches in `{ matches: [...] }` for forward
        // compat (future `total_count` / cursor fields).
        let matches = result
            .get("matches")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("memory.vector_search: missing `matches` field"))?;
        serde_json::from_value::<Vec<VectorMatch>>(matches)
            .map_err(|e| anyhow::anyhow!("decode Vec<VectorMatch>: {e}"))
    }

    async fn delete(&self, collection: &str, ids: Vec<String>) -> anyhow::Result<DeleteAck> {
        let result = self
            .send_request(
                "memory.vector_delete",
                serde_json::json!({
                    "backend": &self.name,
                    "collection": collection,
                    "ids": ids,
                }),
                self.delete_timeout,
            )
            .await?;
        serde_json::from_value::<DeleteAck>(result)
            .map_err(|e| anyhow::anyhow!("decode DeleteAck: {e}"))
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn build() -> (
        Arc<RemoteVectorBackend>,
        mpsc::Receiver<Value>,
        Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    ) {
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let backend = Arc::new(RemoteVectorBackend::new(
            "mock_backend".to_string(),
            "mock_plugin".to_string(),
            stdin_tx,
            pending.clone(),
            next_id,
        ));
        (backend, stdin_rx, pending)
    }

    fn resolve_with_result(
        pending: &DashMap<u64, oneshot::Sender<Result<Value, String>>>,
        id: u64,
        result: Value,
    ) {
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(Ok(result));
        }
    }

    fn resolve_with_error(
        pending: &DashMap<u64, oneshot::Sender<Result<Value, String>>>,
        id: u64,
        err_obj: Value,
    ) {
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(Err(err_obj.to_string()));
        }
    }

    fn fixture_record() -> VectorRecord {
        VectorRecord {
            id: "r1".into(),
            content: "hello".into(),
            embedding: vec![0.1, 0.2, 0.3],
            metadata: serde_json::json!({"source": "kb"}),
        }
    }

    #[test]
    fn name_returns_declared_backend() {
        let (backend, _, _) = build();
        assert_eq!(backend.name(), "mock_backend");
        assert_eq!(backend.plugin_id(), "mock_plugin");
    }

    #[tokio::test]
    async fn upsert_serializes_request_with_backend_collection_records() {
        let (backend, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let backend = backend.clone();
            async move { backend.upsert("kb", vec![fixture_record()]).await }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        assert_eq!(frame["method"], "memory.vector_upsert");
        assert_eq!(frame["params"]["backend"], "mock_backend");
        assert_eq!(frame["params"]["collection"], "kb");
        assert_eq!(frame["params"]["records"][0]["id"], "r1");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(&pending, id, serde_json::json!({"count": 1}));
        let ack = task.await.unwrap().unwrap();
        assert_eq!(ack.count, 1);
    }

    #[tokio::test]
    async fn upsert_deserializes_ack_count() {
        let (backend, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let backend = backend.clone();
            async move { backend.upsert("kb", vec![]).await }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(&pending, id, serde_json::json!({"count": 42}));
        let ack = task.await.unwrap().unwrap();
        assert_eq!(ack.count, 42);
    }

    #[tokio::test]
    async fn search_serializes_request_with_query_vector_and_limit() {
        let (backend, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let backend = backend.clone();
            async move {
                backend
                    .search(
                        "kb",
                        VectorQuery {
                            embedding: vec![0.5, 0.6],
                            limit: 7,
                            filter: None,
                        },
                    )
                    .await
            }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        assert_eq!(frame["method"], "memory.vector_search");
        assert_eq!(frame["params"]["query"]["limit"], 7);
        assert_eq!(frame["params"]["query"]["embedding"][0], 0.5);
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(&pending, id, serde_json::json!({"matches": []}));
        let matches = task.await.unwrap().unwrap();
        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn search_deserializes_match_array() {
        let (backend, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let backend = backend.clone();
            async move {
                backend
                    .search(
                        "kb",
                        VectorQuery {
                            embedding: vec![0.1],
                            limit: 1,
                            filter: None,
                        },
                    )
                    .await
            }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(
            &pending,
            id,
            serde_json::json!({
                "matches": [
                    {"id": "r1", "content": "hello", "score": 0.97, "metadata": {"source": "kb"}}
                ]
            }),
        );
        let matches = task.await.unwrap().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "r1");
        assert!((matches[0].score - 0.97).abs() < 1e-6);
    }

    #[tokio::test]
    async fn delete_serializes_ids_and_deserializes_ack() {
        let (backend, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let backend = backend.clone();
            async move { backend.delete("kb", vec!["r1".into(), "r2".into()]).await }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        assert_eq!(frame["method"], "memory.vector_delete");
        assert_eq!(frame["params"]["ids"][0], "r1");
        assert_eq!(frame["params"]["ids"][1], "r2");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_result(&pending, id, serde_json::json!({"count": 2}));
        let ack = task.await.unwrap().unwrap();
        assert_eq!(ack.count, 2);
    }

    #[tokio::test]
    async fn unsupported_method_returns_err_anyhow() {
        let (backend, mut stdin_rx, pending) = build();
        let task = tokio::spawn({
            let backend = backend.clone();
            async move { backend.upsert("kb", vec![]).await }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        resolve_with_error(
            &pending,
            id,
            serde_json::json!({"code": -32601, "message": "memory.vector_upsert"}),
        );
        let err = task.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("not implemented"));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn request_timeout_returns_err_anyhow() {
        let (stdin_tx, mut stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let backend = RemoteVectorBackend {
            name: "mock_backend".into(),
            plugin_id: "mock_plugin".into(),
            stdin_tx,
            pending,
            next_id,
            upsert_timeout: Duration::from_millis(50),
            search_timeout: Duration::from_millis(50),
            delete_timeout: Duration::from_millis(50),
        };
        let task = tokio::spawn(async move { backend.upsert("kb", vec![]).await });
        let _frame = stdin_rx.recv().await.expect("frame");
        tokio::time::advance(Duration::from_millis(200)).await;
        let err = task.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }
}
