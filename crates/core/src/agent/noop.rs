use async_trait::async_trait;
use super::behavior::AgentBehavior;
use super::context::AgentContext;
use super::types::InboundMessage;
pub struct NoOpAgent;
#[async_trait]
impl AgentBehavior for NoOpAgent {
    async fn on_message(&self, _ctx: &AgentContext, msg: InboundMessage) -> anyhow::Result<()> {
        tracing::info!(
            agent_id = %msg.agent_id,
            session_id = %msg.session_id,
            trigger = ?msg.trigger,
            text = %msg.text,
            "noop agent received message"
        );
        Ok(())
    }
}
