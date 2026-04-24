use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::stream::{default_stream_from_chat, StreamChunk};
use crate::types::{ChatRequest, ChatResponse};

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse>;
    fn model_id(&self) -> &str;
    /// Short identifier of the provider ("minimax", "openai", "stub", ...).
    /// Used as a Prometheus label; must be low-cardinality.
    fn provider(&self) -> &str {
        "unknown"
    }
    /// Compute dense vector embeddings for a batch of input texts.
    /// Default returns an error — providers that do not expose an embedding
    /// endpoint (or where it is not yet wired) opt out by leaving this default.
    async fn embed(&self, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Err(anyhow::anyhow!(
            "embed() not supported by provider '{}'",
            self.provider()
        ))
    }

    /// Incremental response stream. Default implementation delegates to
    /// `chat()` and emits a single `TextDelta` (or `ToolCall*` triples)
    /// followed by `Usage` + `End`. Providers that speak SSE override this
    /// to stream real token-level deltas.
    async fn stream<'a>(
        &'a self,
        req: ChatRequest,
    ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
        default_stream_from_chat(self, req).await
    }
}
