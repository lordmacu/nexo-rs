use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    pub agents: Vec<AgentConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub id: String,
    pub model: ModelConfig,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    #[serde(default)]
    pub config: AgentRuntimeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_heartbeat_interval")]
    pub interval: String,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        HeartbeatConfig { enabled: false, interval: default_heartbeat_interval() }
    }
}

fn default_heartbeat_interval() -> String {
    "5m".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimeConfig {
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_queue_cap")]
    pub queue_cap: usize,
}

impl Default for AgentRuntimeConfig {
    fn default() -> Self {
        AgentRuntimeConfig { debounce_ms: default_debounce_ms(), queue_cap: default_queue_cap() }
    }
}

fn default_debounce_ms() -> u64 { 2000 }
fn default_queue_cap() -> usize { 5 }
