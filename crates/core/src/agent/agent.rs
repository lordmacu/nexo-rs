use super::behavior::AgentBehavior;
use nexo_config::types::agents::AgentConfig;
use std::sync::Arc;
pub struct Agent {
    pub id: String,
    pub config: Arc<AgentConfig>,
    pub behavior: Arc<dyn AgentBehavior>,
}
impl Agent {
    pub fn new(config: AgentConfig, behavior: impl AgentBehavior + 'static) -> Self {
        let id = config.id.clone();
        Self {
            id,
            config: Arc::new(config),
            behavior: Arc::new(behavior),
        }
    }
}
