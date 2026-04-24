pub mod env;
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
    /// Phase 11 — optional extension system config. `None` when the file is
    /// absent; consumers should fall back to `ExtensionsConfig::default()`.
    pub extensions: Option<ExtensionsConfig>,
    /// Phase 12.4 — optional MCP runtime config. `None` when `mcp.yaml` is
    /// absent; in that case the MCP subsystem is simply not started.
    pub mcp: Option<McpConfig>,
    /// Phase 12.6 — optional server-side config (expose this agent as an
    /// MCP server). `None` when `mcp_server.yaml` is absent.
    pub mcp_server: Option<McpServerConfig>,
}

/// Minimal config bundle for the `agent mcp-server` subcommand.
///
/// Phase 12.6 bootstrap only needs identity (pick the first agent) and the
/// server config itself. Skipping `llm.yaml` / `broker.yaml` /
/// `memory.yaml` lets the subcommand run on hosts that aren't configured
/// as a full agent runtime — the operator just wants to expose tools.
#[derive(Debug)]
pub struct McpServerBootConfig {
    pub agents: AgentsConfig,
    pub mcp_server: Option<McpServerConfig>,
}

impl AppConfig {
    pub fn load(dir: &Path) -> Result<Self> {
        let agents = load_required::<AgentsConfig>(dir, "agents.yaml")?;
        let broker = load_required::<BrokerConfig>(dir, "broker.yaml")?;
        let llm = load_required::<LlmConfig>(dir, "llm.yaml")?;
        let memory = load_required::<MemoryConfig>(dir, "memory.yaml")?;
        let plugins = load_plugins(dir)?;
        let extensions = load_optional::<ExtensionsConfigFile>(dir, "extensions.yaml")?
            .map(|f| f.extensions);
        let mcp = load_optional::<McpConfigFile>(dir, "mcp.yaml")?.map(|f| f.mcp);
        if let Some(m) = &mcp {
            m.validate().map_err(|e| anyhow::anyhow!(e))?;
        }
        let mcp_server = load_optional::<McpServerConfigFile>(dir, "mcp_server.yaml")?
            .map(|f| f.mcp_server);
        Ok(AppConfig {
            agents,
            broker,
            llm,
            memory,
            plugins,
            extensions,
            mcp,
            mcp_server,
        })
    }

    /// Phase 12.6 — load only what `agent mcp-server` needs. Tolerant of
    /// missing `llm.yaml` / `broker.yaml` / `memory.yaml` env vars so the
    /// subcommand runs on hosts that don't have a full runtime configured.
    pub fn load_for_mcp_server(dir: &Path) -> Result<McpServerBootConfig> {
        let agents = load_required::<AgentsConfig>(dir, "agents.yaml")?;
        let mcp_server = load_optional::<McpServerConfigFile>(dir, "mcp_server.yaml")?
            .map(|f| f.mcp_server);
        Ok(McpServerBootConfig { agents, mcp_server })
    }
}

/// Load a required YAML file, resolving `${ENV_VAR}` placeholders.
pub fn load_required<T: serde::de::DeserializeOwned>(dir: &Path, filename: &str) -> Result<T> {
    let path = dir.join(filename);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let resolved = env::resolve_placeholders(&raw, filename)?;
    serde_yaml::from_str(&resolved)
        .with_context(|| format!("invalid config in {}", path.display()))
}

/// Load an optional YAML file, resolving `${ENV_VAR}` placeholders. Returns
/// `Ok(None)` when the file is absent.
pub fn load_optional<T: serde::de::DeserializeOwned>(dir: &Path, filename: &str) -> Result<Option<T>> {
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
        .map(|f| f.whatsapp.into_vec())
        .unwrap_or_default();
    let telegram = load_optional::<TelegramPluginConfigFile>(&plugins_dir, "telegram.yaml")?
        .map(|f| f.telegram.into_vec())
        .unwrap_or_default();
    let email = load_optional::<EmailPluginConfigFile>(&plugins_dir, "email.yaml")?
        .map(|f| f.email);
    let browser = load_optional::<BrowserConfigFile>(&plugins_dir, "browser.yaml")?
        .map(|f| f.browser);
    Ok(PluginsConfig { whatsapp, telegram, email, browser })
}
