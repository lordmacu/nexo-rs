use std::sync::Arc;
use dashmap::DashMap;
use agent_broker::AnyBroker;
use super::plugin::Plugin;
#[derive(Default, Clone)]
pub struct PluginRegistry {
    plugins: Arc<DashMap<String, Arc<dyn Plugin>>>,
}
impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&self, plugin: impl Plugin + 'static) {
        self.plugins
            .insert(plugin.name().to_string(), Arc::new(plugin));
    }
    pub fn get(&self, name: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins.get(name).map(|e| Arc::clone(e.value()))
    }
    pub fn names(&self) -> Vec<String> {
        self.plugins.iter().map(|e| e.key().clone()).collect()
    }
    pub async fn start_all(&self, broker: AnyBroker) -> anyhow::Result<()> {
        for entry in self.plugins.iter() {
            entry.value().start(broker.clone()).await?;
        }
        Ok(())
    }
    /// Stops all plugins. Logs errors and continues — always returns Ok.
    pub async fn stop_all(&self) -> anyhow::Result<()> {
        for entry in self.plugins.iter() {
            if let Err(e) = entry.value().stop().await {
                tracing::error!(plugin = %entry.key(), error = %e, "plugin stop failed");
            }
        }
        Ok(())
    }
}
