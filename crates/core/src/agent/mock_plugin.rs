use super::plugin::{Command, Plugin, Response};
use async_trait::async_trait;
use nexo_broker::AnyBroker;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
pub struct MockPlugin {
    pub plugin_name: String,
    pub received: Arc<Mutex<Vec<Command>>>,
    pub response: Response,
    pub started: Arc<AtomicBool>,
    pub stopped: Arc<AtomicBool>,
}
impl MockPlugin {
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_response(name, Response::Ok)
    }
    pub fn with_response(name: impl Into<String>, response: Response) -> Self {
        Self {
            plugin_name: name.into(),
            received: Arc::new(Mutex::new(Vec::new())),
            response,
            started: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }
}
#[async_trait]
impl Plugin for MockPlugin {
    fn name(&self) -> &str {
        &self.plugin_name
    }
    async fn start(&self, _broker: AnyBroker) -> anyhow::Result<()> {
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&self) -> anyhow::Result<()> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn send_command(&self, cmd: Command) -> anyhow::Result<Response> {
        self.received.lock().unwrap().push(cmd);
        Ok(self.response.clone())
    }
}
