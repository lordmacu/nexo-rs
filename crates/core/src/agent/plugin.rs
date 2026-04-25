use nexo_broker::AnyBroker;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    SendMessage {
        to: String,
        text: String,
    },
    SendMedia {
        to: String,
        url: String,
        caption: Option<String>,
    },
    Custom {
        name: String,
        payload: Value,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    MessageSent { message_id: String },
    Error { message: String },
    Custom { payload: Value },
}
#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    /// Subscribe to `plugin.outbound.{name}` and start publishing to `plugin.inbound.{name}`.
    async fn start(&self, broker: AnyBroker) -> anyhow::Result<()>;
    /// Release resources and stop publishing events.
    async fn stop(&self) -> anyhow::Result<()>;
    /// Send a command directly and await a response.
    async fn send_command(&self, cmd: Command) -> anyhow::Result<Response>;
}
