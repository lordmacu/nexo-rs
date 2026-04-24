use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use agent_llm::ToolDef;
use agent_memory::LongTermMemory;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
pub struct MemoryTool {
    memory: Arc<LongTermMemory>,
    default_recall_mode: String,
}
impl MemoryTool {
    pub fn new(memory: Arc<LongTermMemory>) -> Self {
        Self::new_with_default_mode(memory, "keyword")
    }
    pub fn new_with_default_mode(memory: Arc<LongTermMemory>, mode: impl Into<String>) -> Self {
        Self {
            memory,
            default_recall_mode: normalize_recall_mode(mode.into()),
        }
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "memory".to_string(),
            description: "Store and retrieve memories. Actions: remember (save a fact), recall (search memories by keyword), forget (delete by id).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["remember", "recall", "forget"]
                    },
                    "content": { "type": "string", "description": "Fact to store (for remember)" },
                    "query":   { "type": "string", "description": "Search query (for recall)" },
                    "id":      { "type": "string", "description": "Memory UUID to delete (for forget)" },
                    "tags":    {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional tags to categorize the memory"
                    },
                    "limit":   { "type": "integer", "description": "Max results to return (for recall, default 5)" },
                    "mode":    {
                        "type": "string",
                        "enum": ["keyword", "vector", "hybrid"],
                        "description": "Recall mode. `keyword` (default) = FTS; `vector` = semantic nearest-neighbor; `hybrid` = RRF fusion of both. `vector` and `hybrid` require the memory.vector config."
                    }
                },
                "required": ["action"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for MemoryTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let action = args["action"].as_str().unwrap_or("");
        match action {
            "remember" => {
                let content = args["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("remember requires 'content'"))?;
                let tags: Vec<&str> = args["tags"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                let id = self.memory.remember(&ctx.agent_id, content, &tags).await?;
                Ok(json!({ "ok": true, "id": id.to_string() }))
            }
            "recall" => {
                let query = args["query"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("recall requires 'query'"))?;
                let limit = args["limit"].as_u64().unwrap_or(5) as usize;
                let mode_owned = args["mode"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| self.default_recall_mode.clone());
                let mode = mode_owned.as_str();
                let entries = match mode {
                    "vector" => {
                        self.memory
                            .recall_vector(&ctx.agent_id, query, limit)
                            .await?
                    }
                    "hybrid" => {
                        self.memory
                            .recall_hybrid(&ctx.agent_id, query, limit)
                            .await?
                    }
                    "keyword" | "" => self.memory.recall(&ctx.agent_id, query, limit).await?,
                    other => anyhow::bail!("unknown recall mode: {other}"),
                };
                // Log one recall event per hit for Phase 10.5 signal tracking.
                // Reciprocal rank as score — position 1 → 1.0, position 2 → 0.5, …
                for (idx, e) in entries.iter().enumerate() {
                    let score = 1.0 / (idx as f32 + 1.0);
                    if let Err(err) = self
                        .memory
                        .record_recall_event(&ctx.agent_id, e.id, query, score)
                        .await
                    {
                        tracing::warn!(
                            agent_id = %ctx.agent_id,
                            memory_id = %e.id,
                            error = %err,
                            "failed to record recall event"
                        );
                    }
                }
                let results: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        json!({
                            "id":      e.id.to_string(),
                            "content": e.content,
                            "tags":    e.tags,
                        })
                    })
                    .collect();
                Ok(json!({ "ok": true, "results": results }))
            }
            "forget" => {
                let id_str = args["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("forget requires 'id'"))?;
                let id = Uuid::parse_str(id_str)
                    .map_err(|_| anyhow::anyhow!("invalid UUID: {id_str}"))?;
                let deleted = self.memory.forget(id).await?;
                Ok(json!({ "ok": deleted }))
            }
            other => anyhow::bail!("unknown memory action: {other}"),
        }
    }
}

fn normalize_recall_mode(mode: String) -> String {
    match mode.trim() {
        "keyword" | "vector" | "hybrid" => mode.trim().to_string(),
        _ => "keyword".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_recall_mode;

    #[test]
    fn normalize_recall_mode_accepts_supported_values() {
        assert_eq!(normalize_recall_mode("keyword".to_string()), "keyword");
        assert_eq!(normalize_recall_mode("vector".to_string()), "vector");
        assert_eq!(normalize_recall_mode("hybrid".to_string()), "hybrid");
    }

    #[test]
    fn normalize_recall_mode_falls_back_to_keyword() {
        assert_eq!(normalize_recall_mode("".to_string()), "keyword");
        assert_eq!(normalize_recall_mode("auto".to_string()), "keyword");
        assert_eq!(normalize_recall_mode(" VECTOR ".to_string()), "keyword");
    }
}
