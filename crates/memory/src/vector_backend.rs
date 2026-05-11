//! `VectorBackend` trait + wire shapes.
//!
//! Subprocess plugins declaring
//! `[plugin.extends].memory_backends = [...]` register
//! implementations of this trait into the daemon's
//! `VectorBackendRegistry`. Operator-side consumers
//! (`LongTermMemory.recall_vector` lookup) wire to the registry
//! separately.
//!
//! Vector-only scope: short-term + long-term memory keep their
//! existing SQLite implementation. Plugins replace ONLY the
//! vector index. The primary use case is Pinecone / Qdrant /
//! Weaviate / pgvector, where operators want managed-vector-DB
//! features (filtering, sharding, hybrid retrieval) the
//! sqlite-vec extension doesn't ship.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One vector record. `embedding` is the pre-computed dense
/// vector (host-side embedder or LLM provider produces it);
/// `metadata` is opaque JSON the backend may filter against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorRecord {
    pub id: String,
    pub content: String,
    pub embedding: Vec<f32>,
    /// Backend-specific metadata. Pinecone / Qdrant / Weaviate
    /// each accept different shapes; the host passes whatever
    /// the operator's caller provides as opaque JSON.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

/// Vector search request. `filter` is an opaque JSON value the
/// backend interprets per its own convention (Pinecone metadata
/// filter, Qdrant filter expression, etc.) — the host does NOT
/// validate or rewrite it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorQuery {
    pub embedding: Vec<f32>,
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<serde_json::Value>,
}

/// One match returned by `VectorBackend::search`. `score` is on
/// the backend's native scale (cosine vs dot-product vs distance);
/// each backend documents its convention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorMatch {
    pub id: String,
    pub content: String,
    pub score: f32,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UpsertAck {
    pub count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeleteAck {
    pub count: u32,
}

/// Pluggable vector store. v1 covers `upsert / search / delete`;
/// hybrid retrieval (vector + tag filter) stays in
/// `LongTermMemory` and orchestrates this trait when wired.
#[async_trait]
pub trait VectorBackend: Send + Sync + 'static {
    /// Stable backend identifier matching the operator's
    /// `agents.yaml.<id>.vector_backend = "<name>"` selector.
    fn name(&self) -> &str;

    async fn upsert(
        &self,
        collection: &str,
        records: Vec<VectorRecord>,
    ) -> anyhow::Result<UpsertAck>;

    async fn search(
        &self,
        collection: &str,
        query: VectorQuery,
    ) -> anyhow::Result<Vec<VectorMatch>>;

    async fn delete(&self, collection: &str, ids: Vec<String>) -> anyhow::Result<DeleteAck>;
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_record_round_trips() {
        let r = VectorRecord {
            id: "r1".into(),
            content: "hello".into(),
            embedding: vec![0.1, 0.2, 0.3],
            metadata: serde_json::json!({"source": "kb"}),
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: VectorRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn vector_query_round_trips() {
        let q = VectorQuery {
            embedding: vec![0.4, 0.5],
            limit: 10,
            filter: Some(serde_json::json!({"namespace": "tenant-1"})),
        };
        let s = serde_json::to_string(&q).unwrap();
        let back: VectorQuery = serde_json::from_str(&s).unwrap();
        assert_eq!(back, q);

        // Filter omitted when None.
        let q_no_filter = VectorQuery {
            embedding: vec![1.0],
            limit: 5,
            filter: None,
        };
        let s = serde_json::to_string(&q_no_filter).unwrap();
        assert!(!s.contains("filter"));
    }

    #[test]
    fn vector_match_round_trips() {
        let m = VectorMatch {
            id: "r1".into(),
            content: "hello".into(),
            score: 0.97,
            metadata: serde_json::json!({"source": "kb"}),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: VectorMatch = serde_json::from_str(&s).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn upsert_ack_round_trips() {
        let a = UpsertAck { count: 42 };
        let s = serde_json::to_string(&a).unwrap();
        let back: UpsertAck = serde_json::from_str(&s).unwrap();
        assert_eq!(back, a);

        let d = DeleteAck { count: 7 };
        let s = serde_json::to_string(&d).unwrap();
        let back: DeleteAck = serde_json::from_str(&s).unwrap();
        assert_eq!(back, d);
    }
}
