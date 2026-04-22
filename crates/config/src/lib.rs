mod env;
pub mod types;

pub use types::*;

use std::path::Path;
use anyhow::{Context, Result};

#[derive(Debug)]
pub struct AppConfig {
    pub agents: AgentsConfig,
    pub broker: BrokerConfig,
    pub llm: LlmConfig,
    pub memory: MemoryConfig,
    pub plugins: PluginsConfig,
}

impl AppConfig {
    pub fn load(dir: &Path) -> Result<Self> {
        let agents = load_required::<AgentsConfig>(dir, "agents.yaml")?;
        let broker = load_required::<BrokerConfig>(dir, "broker.yaml")?;
        let llm = load_required::<LlmConfig>(dir, "llm.yaml")?;
        let memory = load_required::<MemoryConfig>(dir, "memory.yaml")?;
        let plugins = load_plugins(dir)?;
        Ok(AppConfig { agents, broker, llm, memory, plugins })
    }
}

fn load_required<T: serde::de::DeserializeOwned>(dir: &Path, filename: &str) -> Result<T> {
    let path = dir.join(filename);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let resolved = env::resolve_placeholders(&raw, filename)?;
    serde_yaml::from_str(&resolved)
        .with_context(|| format!("invalid config in {}", path.display()))
}

fn load_optional<T: serde::de::DeserializeOwned>(dir: &Path, filename: &str) -> Result<Option<T>> {
    let path = dir.join(filename);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let resolved = env::resolve_placeholders(&raw, filename)?;
    let value = serde_yaml::from_str(&resolved)
        .with_context(|| format!("invalid config in {}", path.display()))?;
    Ok(Some(value))
}

fn load_plugins(dir: &Path) -> Result<PluginsConfig> {
    let plugins_dir = dir.join("plugins");
    let whatsapp = load_optional::<WhatsappPluginConfigFile>(&plugins_dir, "whatsapp.yaml")?
        .map(|f| f.whatsapp);
    let telegram = load_optional::<TelegramPluginConfigFile>(&plugins_dir, "telegram.yaml")?
        .map(|f| f.telegram);
    let email = load_optional::<EmailPluginConfigFile>(&plugins_dir, "email.yaml")?
        .map(|f| f.email);
    Ok(PluginsConfig { whatsapp, telegram, email })
}
