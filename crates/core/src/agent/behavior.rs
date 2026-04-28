use super::context::AgentContext;
use super::types::InboundMessage;
use async_trait::async_trait;
use nexo_broker::Event;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTurnControl {
    Done,
    Sleep { duration_ms: u64, reason: String },
}

#[async_trait]
pub trait AgentBehavior: Send + Sync {
    async fn on_message(&self, _ctx: &AgentContext, _msg: InboundMessage) -> anyhow::Result<()> {
        Ok(())
    }
    async fn on_message_control(
        &self,
        ctx: &AgentContext,
        msg: InboundMessage,
    ) -> anyhow::Result<AgentTurnControl> {
        self.on_message(ctx, msg).await?;
        Ok(AgentTurnControl::Done)
    }
    async fn on_event(&self, _ctx: &AgentContext, _event: Event) -> anyhow::Result<()> {
        Ok(())
    }
    async fn on_heartbeat(&self, _ctx: &AgentContext) -> anyhow::Result<()> {
        Ok(())
    }
    /// Stub for Phase 3 LLM reasoning. Returns empty string by default.
    async fn decide(&self, _ctx: &AgentContext, _msg: &InboundMessage) -> anyhow::Result<String> {
        Ok(String::new())
    }
}
