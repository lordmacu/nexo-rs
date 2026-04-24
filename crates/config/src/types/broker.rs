use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerConfig {
    pub broker: BrokerInner,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerInner {
    #[serde(rename = "type")]
    pub kind: BrokerKind,
    pub url: String,
    #[serde(default)]
    pub auth: BrokerAuthConfig,
    #[serde(default)]
    pub persistence: BrokerPersistenceConfig,
    #[serde(default)]
    pub limits: BrokerLimitsConfig,
    #[serde(default)]
    pub fallback: BrokerFallbackConfig,
}

#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BrokerKind {
    #[default]
    Local,
    Nats,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrokerAuthConfig {
    #[serde(default)]
    pub enabled: bool,
    pub nkey_file: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrokerPersistenceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_queue_path")]
    pub path: String,
}

fn default_queue_path() -> String {
    "./data/queue".to_string()
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrokerLimitsConfig {
    #[serde(default = "default_max_payload")]
    pub max_payload: String,
    #[serde(default = "default_max_pending")]
    pub max_pending: usize,
}

fn default_max_payload() -> String {
    "4MB".to_string()
}
fn default_max_pending() -> usize {
    10_000
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrokerFallbackConfig {
    #[serde(default = "default_fallback_mode")]
    pub mode: String,
    #[serde(default = "default_drain_on_reconnect")]
    pub drain_on_reconnect: bool,
}

fn default_fallback_mode() -> String {
    "local_queue".to_string()
}
fn default_drain_on_reconnect() -> bool {
    true
}
