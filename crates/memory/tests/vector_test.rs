//! Phase 5.4 — integration tests for `open_with_vector`, `remember`,
//! `recall_vector`, `recall_hybrid` with a mock embedding provider.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nexo_memory::{EmbeddingProvider, LongTermMemory};
use async_trait::async_trait;
use tempfile::TempDir;

struct MockProvider {
    dim: usize,
    mapping: Mutex<HashMap<String, Vec<f32>>>,
}

impl MockProvider {
    fn new(dim: usize) -> Self {
        Self {
            dim,
            mapping: Mutex::new(HashMap::new()),
        }
    }

    /// Deterministic embedding: hash text into the first N positions.
    /// Different texts produce different vectors; identical texts collide.
    fn synth(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; self.dim];
        for (i, byte) in text.bytes().enumerate() {
            v[i % self.dim] += byte as f32;
        }
        // Normalize for nicer cosine-ish behaviour.
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        for x in &mut v {
            *x /= norm;
        }
        v
    }
}

#[async_trait]
impl EmbeddingProvider for MockProvider {
    fn dimension(&self) -> usize {
        self.dim
    }
    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        let mut guard = self.mapping.lock().unwrap();
        for t in texts {
            let v = guard
                .entry((*t).to_string())
                .or_insert_with(|| self.synth(t))
                .clone();
            out.push(v);
        }
        Ok(out)
    }
}

fn mock(dim: usize) -> Arc<dyn EmbeddingProvider> {
    Arc::new(MockProvider::new(dim))
}

#[tokio::test]
async fn open_without_provider_has_no_vec_table() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("memory.db");
    let mem = LongTermMemory::open(path.to_str().unwrap()).await.unwrap();
    assert!(mem.embedding_provider().is_none());
}

#[tokio::test]
async fn open_with_provider_creates_vec_table() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("memory.db");
    let mem = LongTermMemory::open_with_vector(path.to_str().unwrap(), Some(mock(4)))
        .await
        .unwrap();
    assert!(mem.embedding_provider().is_some());
    // recall_vector over empty memories returns empty vec.
    let out = mem.recall_vector("kate", "anything", 5).await.unwrap();
    assert!(out.is_empty());
}

#[tokio::test]
async fn remember_then_recall_vector_returns_closest() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("memory.db");
    let mem = LongTermMemory::open_with_vector(path.to_str().unwrap(), Some(mock(8)))
        .await
        .unwrap();

    mem.remember("kate", "cristian enjoys terse output", &["user", "style"])
        .await
        .unwrap();
    mem.remember("kate", "user is named cristian garcia", &["user"])
        .await
        .unwrap();
    mem.remember("kate", "project uses rust async for agent framework", &[])
        .await
        .unwrap();

    // Query that aligns with the first two; mock produces close vectors
    // for texts sharing many bytes.
    let out = mem
        .recall_vector("kate", "cristian likes brief replies", 2)
        .await
        .unwrap();
    assert!(out.len() <= 2);
    assert!(!out.is_empty(), "expected at least one nearest neighbor");
    let joined = out
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(joined.contains("cristian"), "got: {joined}");
}

#[tokio::test]
async fn recall_vector_filters_by_agent_id() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("memory.db");
    let mem = LongTermMemory::open_with_vector(path.to_str().unwrap(), Some(mock(8)))
        .await
        .unwrap();
    mem.remember("kate", "shared text about things", &[])
        .await
        .unwrap();
    mem.remember("bob", "shared text about things", &[])
        .await
        .unwrap();
    let kate_hits = mem
        .recall_vector("kate", "shared text about things", 5)
        .await
        .unwrap();
    assert_eq!(kate_hits.len(), 1);
    assert_eq!(kate_hits[0].agent_id, "kate");
}

#[tokio::test]
async fn recall_hybrid_without_provider_falls_back_to_fts() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("memory.db");
    let mem = LongTermMemory::open(path.to_str().unwrap()).await.unwrap();
    mem.remember("kate", "hello world", &[]).await.unwrap();
    let out = mem.recall_hybrid("kate", "hello", 3).await.unwrap();
    assert_eq!(out.len(), 1);
}

#[tokio::test]
async fn recall_hybrid_merges_fts_and_vector() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("memory.db");
    let mem = LongTermMemory::open_with_vector(path.to_str().unwrap(), Some(mock(8)))
        .await
        .unwrap();
    // FTS hit on "rust"
    mem.remember("kate", "rust is a systems language", &[])
        .await
        .unwrap();
    // Semantic-only candidate (shares bytes with query)
    mem.remember("kate", "cristian enjoys terse output", &[])
        .await
        .unwrap();
    // Non-matching baseline
    mem.remember("kate", "unrelated musings about coffee", &[])
        .await
        .unwrap();

    let out = mem.recall_hybrid("kate", "rust", 3).await.unwrap();
    assert!(!out.is_empty());
    // At least the FTS match must be present.
    assert!(
        out.iter().any(|m| m.content.contains("rust")),
        "missing FTS hit: {out:?}"
    );
}

#[tokio::test]
async fn dimension_mismatch_on_reopen_errors() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("memory.db");
    // Create with dim=4 and insert a row.
    {
        let mem = LongTermMemory::open_with_vector(path.to_str().unwrap(), Some(mock(4)))
            .await
            .unwrap();
        mem.remember("kate", "anchor content", &[]).await.unwrap();
    }
    // Re-open with dim=8 — mismatch must error.
    let result = LongTermMemory::open_with_vector(path.to_str().unwrap(), Some(mock(8))).await;
    match result {
        Ok(_) => panic!("expected dimension mismatch"),
        Err(e) => assert!(e.to_string().contains("dimension mismatch"), "got: {e}"),
    }
}
