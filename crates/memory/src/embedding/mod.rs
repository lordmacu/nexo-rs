//! Phase 5.4 — embedding provider abstraction.
//!
//! The trait is the public surface; `HttpEmbeddingProvider` is the one
//! implementation we ship in 5.4 (OpenAI-compatible `/embeddings`). Local
//! providers (fastembed / candle) are follow-ups.

mod http;

use async_trait::async_trait;

pub use http::HttpEmbeddingProvider;

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Size of every vector produced by `embed`. Validated at schema init.
    fn dimension(&self) -> usize;

    /// Embed a batch of texts. The output vec has the same length as the
    /// input and preserves order.
    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;
}
