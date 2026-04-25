//! End-to-end: `LlmDecider` records every decision into the memory
//! store. Uses `MockLlmClient` (always returns Allow) +
//! `SqliteVecDecisionMemory` in-memory.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_driver_loop::memory::mock::MockEmbedder;
use nexo_driver_loop::{LlmDecider, SqliteVecDecisionMemory};
use nexo_driver_permission::{PermissionDecider, PermissionRequest};
use nexo_driver_types::GoalId;
use nexo_llm::{ChatRequest, ChatResponse, FinishReason, LlmClient, ResponseContent, TokenUsage};

struct MockLlmClient;

#[async_trait]
impl LlmClient for MockLlmClient {
    fn model_id(&self) -> &str {
        "mock"
    }
    async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            content: ResponseContent::Text(
                r#"{"outcome":"allow_once","rationale":"safe"}"#.to_string(),
            ),
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Stop,
            cache_usage: None,
        })
    }
}

#[tokio::test]
async fn llm_decider_records_after_decide() {
    let mem = Arc::new(
        SqliteVecDecisionMemory::open_memory(Arc::new(MockEmbedder::new()))
            .await
            .unwrap(),
    );
    let llm: Arc<dyn LlmClient> = Arc::new(MockLlmClient);
    let decider = LlmDecider::builder()
        .llm(llm)
        .model("mock")
        .max_tokens(64)
        .memory(mem.clone())
        .build()
        .unwrap();

    let req = PermissionRequest {
        goal_id: GoalId::new(),
        tool_use_id: "tu_1".into(),
        tool_name: "Edit".into(),
        input: serde_json::json!({"file": "src/lib.rs"}),
        metadata: serde_json::Map::new(),
    };
    let _ = decider.decide(req).await.unwrap();
    assert_eq!(mem.count().await.unwrap(), 1);
}
