//! `MockEmbedder` — deterministic embedder for tests. Hashes the
//! input text into 8 floats so equal strings produce identical
//! vectors and distinct strings produce different ones.
//!
//! NOT exposed in the public surface. Available to integration tests
//! via the test-only re-export in `mod.rs`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use async_trait::async_trait;
use nexo_memory::EmbeddingProvider;

pub struct MockEmbedder {
    dim: usize,
}

impl MockEmbedder {
    pub fn new() -> Self {
        Self { dim: 8 }
    }
}

impl Default for MockEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbedder {
    fn dimension(&self) -> usize {
        self.dim
    }

    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vector_for(t, self.dim)).collect())
    }
}

fn vector_for(text: &str, dim: usize) -> Vec<f32> {
    // Mix the input text into a u64 seed, then derive `dim` floats by
    // splat-hashing the seed combined with each index. Stable across
    // runs and platform byte-order.
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    let seed = h.finish();
    (0..dim)
        .map(|i| {
            let mut h2 = DefaultHasher::new();
            (seed, i as u64).hash(&mut h2);
            // Map u64 → [-1.0, 1.0] roughly. Stable, deterministic.
            let n = h2.finish();
            ((n as i64) as f32) / (i64::MAX as f32)
        })
        .collect()
}
