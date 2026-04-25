use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nexo_llm::ToolDef;
use nexo_memory::LongTermMemory;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
pub struct HeartbeatTool {
    memory: Arc<LongTermMemory>,
}
impl HeartbeatTool {
    pub fn new(memory: Arc<LongTermMemory>) -> Self {
        Self { memory }
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "schedule_reminder".to_string(),
            description: "Schedule a reminder message for later in the current conversation. Use this when the user asks to be reminded at a time or after a delay.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "at": {
                        "type": "string",
                        "description": "When the reminder is due. Accepts RFC3339 timestamps like 2026-04-22T18:30:00Z or relative durations like 10m, 2h, 1d."
                    },
                    "message": {
                        "type": "string",
                        "description": "Reminder text to send when due."
                    }
                },
                "required": ["at", "message"]
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for HeartbeatTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let at = args["at"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule_reminder requires `at`"))?;
        let message = args["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule_reminder requires `message`"))?;
        let session_id = args["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule_reminder missing runtime `session_id`"))?;
        let source_plugin = args["source_plugin"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule_reminder missing runtime `source_plugin`"))?;
        let recipient = args["recipient"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule_reminder missing runtime `recipient`"))?;
        let session_id = Uuid::parse_str(session_id)
            .map_err(|_| anyhow::anyhow!("invalid runtime session_id: {session_id}"))?;
        let due_at = parse_due_at(at)?;
        let id = self
            .memory
            .schedule_reminder(
                &ctx.agent_id,
                session_id,
                source_plugin,
                recipient,
                message,
                due_at,
            )
            .await?;
        Ok(json!({
            "ok": true,
            "id": id.to_string(),
            "due_at": due_at.to_rfc3339(),
        }))
    }
}
fn parse_due_at(input: &str) -> anyhow::Result<DateTime<Utc>> {
    if let Ok(ts) = DateTime::parse_from_rfc3339(input) {
        return Ok(ts.with_timezone(&Utc));
    }
    let duration = humantime::parse_duration(input)
        .map_err(|e| anyhow::anyhow!("invalid reminder time `{input}`: {e}"))?;
    let due_at = chrono::Duration::from_std(duration)
        .map_err(|_| anyhow::anyhow!("reminder duration out of range: {input}"))?;
    Ok(Utc::now() + due_at)
}
