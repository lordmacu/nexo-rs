pub mod env;
pub mod types;

pub use types::*;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

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
    /// Phase 18 — runtime-level knobs (hot-reload). Always populated;
    /// an absent `runtime.yaml` yields [`RuntimeConfig::default`].
    pub runtime: RuntimeConfig,
    /// Phase 19 — generic poller subsystem. `None` when
    /// `pollers.yaml` is absent (subsystem off).
    pub pollers: Option<PollersConfig>,
    /// TaskFlow runtime knobs. Always populated; absent file → defaults.
    pub taskflow: TaskflowConfig,
    /// Transcripts subsystem (FTS index + redaction). Always populated;
    /// absent file → defaults (FTS on, redaction off).
    pub transcripts: TranscriptsConfig,
    /// FOLLOWUPS PR-6 — optional pairing config overrides
    /// (`config/pairing.yaml`). `None` keeps the legacy hardcoded
    /// paths (`<memory_dir>/pairing.db`,
    /// `~/.nexo/secret/pairing.key`); each field overrides
    /// selectively when the file is present.
    pub pairing: Option<PairingInner>,
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
        let mut agents = load_required::<AgentsConfig>(dir, "agents.yaml")?;
        merge_agents_drop_in(dir, &mut agents)?;
        resolve_relative_paths(dir, &mut agents);
        let broker = load_required::<BrokerConfig>(dir, "broker.yaml")?;
        let llm = load_required::<LlmConfig>(dir, "llm.yaml")?;
        let memory = load_required::<MemoryConfig>(dir, "memory.yaml")?;
        let plugins = load_plugins(dir)?;
        let extensions =
            load_optional::<ExtensionsConfigFile>(dir, "extensions.yaml")?.map(|f| f.extensions);
        let mcp = load_optional::<McpConfigFile>(dir, "mcp.yaml")?.map(|f| f.mcp);
        if let Some(m) = &mcp {
            m.validate().map_err(|e| anyhow::anyhow!(e))?;
        }
        let mcp_server =
            load_optional::<McpServerConfigFile>(dir, "mcp_server.yaml")?.map(|f| f.mcp_server);
        let runtime = load_optional::<RuntimeConfig>(dir, "runtime.yaml")?.unwrap_or_default();
        let pollers = load_optional::<PollersConfigFile>(dir, "pollers.yaml")?.map(|f| f.pollers);
        let taskflow = load_optional::<TaskflowConfig>(dir, "taskflow.yaml")?.unwrap_or_default();
        let transcripts =
            load_optional::<TranscriptsConfig>(dir, "transcripts.yaml")?.unwrap_or_default();
        let pairing =
            load_optional::<PairingConfig>(dir, "pairing.yaml")?.map(|f| f.pairing);
        Ok(AppConfig {
            agents,
            broker,
            llm,
            memory,
            plugins,
            extensions,
            mcp,
            mcp_server,
            runtime,
            pollers,
            taskflow,
            transcripts,
            pairing,
        })
    }

    /// Phase 12.6 — load only what `agent mcp-server` needs. Tolerant of
    /// missing `llm.yaml` / `broker.yaml` / `memory.yaml` env vars so the
    /// subcommand runs on hosts that don't have a full runtime configured.
    pub fn load_for_mcp_server(dir: &Path) -> Result<McpServerBootConfig> {
        let mut agents = load_required::<AgentsConfig>(dir, "agents.yaml")?;
        merge_agents_drop_in(dir, &mut agents)?;
        resolve_relative_paths(dir, &mut agents);
        let mcp_server =
            load_optional::<McpServerConfigFile>(dir, "mcp_server.yaml")?.map(|f| f.mcp_server);
        Ok(McpServerBootConfig { agents, mcp_server })
    }
}

/// Merge private agent definitions from `config/agents.d/*.yaml` into the
/// top-level agents list. Use-case: keep `config/agents.yaml` public-safe
/// in version control while stashing per-customer or business-sensitive
/// agents (full prompts, pricing tables, internal contact lists) in a
/// gitignored drop-in directory that the runtime still loads at boot.
///
/// Each drop-in file has the same shape as `agents.yaml`
/// (`agents: [ ... ]`) so operators can move entries freely between the
/// base file and the directory without restructuring.
/// Resolve agent-level filesystem paths (`skills_dir`, `workspace`,
/// `transcripts_dir`, `extra_docs`) against the config directory when
/// they are relative. Makes boot independent of the process cwd — a
/// config loaded from `/etc/agent/` still points `./skills` at
/// `/etc/agent/skills` instead of whatever the shell happened to
/// launch from. Absolute paths and empty strings pass through
/// unchanged.
fn resolve_relative_paths(dir: &Path, agents: &mut AgentsConfig) {
    let resolve = |p: &str| -> String {
        if p.is_empty() {
            return p.to_string();
        }
        let candidate = Path::new(p);
        if candidate.is_absolute() {
            return p.to_string();
        }
        // Skip leading `./` components so the rendered path looks clean
        // (`/etc/agent/skills` instead of `/etc/agent/./skills`) and
        // matches what downstream consumers expect in log messages and
        // assertions.
        let mut joined = dir.to_path_buf();
        for comp in candidate.components() {
            match comp {
                std::path::Component::CurDir => {}
                c => joined.push(c.as_os_str()),
            }
        }
        joined.to_string_lossy().into_owned()
    };
    for a in &mut agents.agents {
        a.skills_dir = resolve(&a.skills_dir);
        a.workspace = resolve(&a.workspace);
        a.transcripts_dir = resolve(&a.transcripts_dir);
        for d in &mut a.extra_docs {
            *d = resolve(d);
        }
    }
}

fn merge_agents_drop_in(dir: &Path, base: &mut AgentsConfig) -> Result<()> {
    let drop_dir = dir.join("agents.d");
    if !drop_dir.exists() {
        return Ok(());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&drop_dir)
        .with_context(|| format!("read_dir {}", drop_dir.display()))?
        .filter_map(|r| r.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yaml"))
        .collect();
    // Deterministic load order — operators can prefix with `00-`, `10-`, etc.
    files.sort();
    for path in files {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("agents.d entry");
        let resolved = env::resolve_placeholders(&raw, label)?;
        let extra: AgentsConfig = serde_yaml::from_str(&resolved)
            .with_context(|| format!("invalid config in {}", path.display()))?;
        base.agents.extend(extra.agents);
    }
    Ok(())
}

/// Load a required YAML file, resolving `${ENV_VAR}` placeholders.
pub fn load_required<T: serde::de::DeserializeOwned>(dir: &Path, filename: &str) -> Result<T> {
    let path = dir.join(filename);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let resolved = env::resolve_placeholders(&raw, filename)?;
    serde_yaml::from_str(&resolved).with_context(|| format!("invalid config in {}", path.display()))
}

/// Load an optional YAML file, resolving `${ENV_VAR}` placeholders. Returns
/// `Ok(None)` when the file is absent.
pub fn load_optional<T: serde::de::DeserializeOwned>(
    dir: &Path,
    filename: &str,
) -> Result<Option<T>> {
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
    let email =
        load_optional::<EmailPluginConfigFile>(&plugins_dir, "email.yaml")?.map(|f| f.email);
    let browser =
        load_optional::<BrowserConfigFile>(&plugins_dir, "browser.yaml")?.map(|f| f.browser);
    Ok(PluginsConfig {
        whatsapp,
        telegram,
        email,
        browser,
    })
}
