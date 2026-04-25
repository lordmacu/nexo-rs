//! YAML config for the project tracker subsystem.
//!
//! The config covers three areas that future steps will consume:
//!
//! - `tracker:` — file roots + cache TTL + watcher toggle (used now).
//! - `program_phase:` — dispatch knobs (consumed in 67.E.x).
//! - `agent_registry:` — multi-agent registry persistence (consumed
//!   in 67.B.x).
//!
//! Loading them all in one struct keeps the operator surface as a
//! single file even though different sub-phases wire different
//! pieces.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct ProjectTrackerConfig {
    #[serde(default)]
    pub tracker: TrackerConfig,
    #[serde(default)]
    pub program_phase: ProgramPhaseConfig,
    #[serde(default)]
    pub agent_registry: AgentRegistryConfig,
}

impl Default for ProjectTrackerConfig {
    fn default() -> Self {
        Self {
            tracker: TrackerConfig::default(),
            program_phase: ProgramPhaseConfig::default(),
            agent_registry: AgentRegistryConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct TrackerConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Workspace root that contains `PHASES.md` and optionally
    /// `FOLLOWUPS.md`. Empty string means "use cwd".
    #[serde(default)]
    pub root: PathBuf,
    #[serde(default = "default_watch")]
    pub watch: bool,
    #[serde(with = "humantime_serde", default = "default_ttl")]
    pub ttl: Duration,
    #[serde(default)]
    pub git_log: GitLogConfig,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            root: PathBuf::new(),
            watch: true,
            ttl: default_ttl(),
            git_log: GitLogConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct GitLogConfig {
    #[serde(default = "default_git_enabled")]
    pub enabled: bool,
    #[serde(with = "humantime_serde", default = "default_git_timeout")]
    pub timeout: Duration,
    #[serde(default = "default_max_commits")]
    pub max_commits: usize,
}

impl Default for GitLogConfig {
    fn default() -> Self {
        Self {
            enabled: default_git_enabled(),
            timeout: default_git_timeout(),
            max_commits: default_max_commits(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProgramPhaseConfig {
    /// Dispatch tools opt-in. Default `false` — operators must turn
    /// this on explicitly. Read tools are independent.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_require_trusted")]
    pub require_trusted: bool,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_agents: u32,
    #[serde(default = "default_max_per_sender")]
    pub max_concurrent_per_sender: u32,
    #[serde(default = "default_progress_every")]
    pub progress_every_turns: u32,
    /// Shell hooks run arbitrary commands inside the daemon process.
    /// Default off; honours the `PROGRAM_PHASE_ALLOW_SHELL_HOOKS` env
    /// override exposed via `agent doctor capabilities`.
    #[serde(default)]
    pub allow_shell_hooks: bool,
    #[serde(with = "humantime_serde", default = "default_hook_timeout")]
    pub hook_timeout: Duration,
    #[serde(default = "default_summary_cap")]
    pub summary_byte_cap: usize,
    #[serde(default = "default_chain_depth")]
    pub max_chain_depth: u32,
}

impl Default for ProgramPhaseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_trusted: default_require_trusted(),
            max_concurrent_agents: default_max_concurrent(),
            max_concurrent_per_sender: default_max_per_sender(),
            progress_every_turns: default_progress_every(),
            allow_shell_hooks: false,
            hook_timeout: default_hook_timeout(),
            summary_byte_cap: default_summary_cap(),
            max_chain_depth: default_chain_depth(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentRegistryConfig {
    /// SQLite path. Empty → fall back to in-memory only (state lost
    /// on daemon restart — acceptable for dev).
    #[serde(default)]
    pub store: PathBuf,
    #[serde(default = "default_reattach")]
    pub reattach_on_boot: bool,
    #[serde(default = "default_log_buffer_lines")]
    pub log_buffer_lines: usize,
}

impl Default for AgentRegistryConfig {
    fn default() -> Self {
        Self {
            store: PathBuf::new(),
            reattach_on_boot: default_reattach(),
            log_buffer_lines: default_log_buffer_lines(),
        }
    }
}

fn default_enabled() -> bool {
    true
}
fn default_watch() -> bool {
    true
}
fn default_ttl() -> Duration {
    Duration::from_secs(60)
}
fn default_git_enabled() -> bool {
    true
}
fn default_git_timeout() -> Duration {
    Duration::from_secs(5)
}
fn default_max_commits() -> usize {
    5
}
fn default_require_trusted() -> bool {
    true
}
fn default_max_concurrent() -> u32 {
    4
}
fn default_max_per_sender() -> u32 {
    2
}
fn default_progress_every() -> u32 {
    5
}
fn default_hook_timeout() -> Duration {
    Duration::from_secs(30)
}
fn default_summary_cap() -> usize {
    3500
}
fn default_chain_depth() -> u32 {
    5
}
fn default_reattach() -> bool {
    true
}
fn default_log_buffer_lines() -> usize {
    200
}

impl ProjectTrackerConfig {
    pub fn from_yaml_str(yaml: &str) -> Result<Self, String> {
        serde_yaml::from_str(yaml).map_err(|e| e.to_string())
    }

    pub fn from_yaml_file(path: &std::path::Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::from_yaml_str(&raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_yaml_uses_defaults() {
        let cfg = ProjectTrackerConfig::from_yaml_str("{}").unwrap();
        assert!(cfg.tracker.enabled);
        assert!(cfg.tracker.watch);
        assert_eq!(cfg.tracker.ttl, Duration::from_secs(60));
        assert!(!cfg.program_phase.enabled);
        assert!(cfg.program_phase.require_trusted);
        assert_eq!(cfg.program_phase.max_concurrent_agents, 4);
        assert!(cfg.agent_registry.reattach_on_boot);
    }

    #[test]
    fn full_yaml_round_trips() {
        let yaml = "\
tracker:
  enabled: true
  root: /tmp/p
  watch: false
  ttl: 30s
  git_log:
    enabled: true
    timeout: 3s
    max_commits: 10
program_phase:
  enabled: true
  require_trusted: true
  max_concurrent_agents: 8
  max_concurrent_per_sender: 3
  progress_every_turns: 10
  allow_shell_hooks: false
  hook_timeout: 1m
  summary_byte_cap: 2000
  max_chain_depth: 3
agent_registry:
  store: /var/lib/nexo-rs/agents.db
  reattach_on_boot: true
  log_buffer_lines: 500
";
        let cfg = ProjectTrackerConfig::from_yaml_str(yaml).unwrap();
        assert_eq!(cfg.tracker.root, PathBuf::from("/tmp/p"));
        assert!(!cfg.tracker.watch);
        assert_eq!(cfg.tracker.ttl, Duration::from_secs(30));
        assert_eq!(cfg.tracker.git_log.max_commits, 10);
        assert!(cfg.program_phase.enabled);
        assert_eq!(cfg.program_phase.max_concurrent_agents, 8);
        assert_eq!(cfg.program_phase.hook_timeout, Duration::from_secs(60));
        assert_eq!(
            cfg.agent_registry.store,
            PathBuf::from("/var/lib/nexo-rs/agents.db")
        );
        assert_eq!(cfg.agent_registry.log_buffer_lines, 500);
    }

    #[test]
    fn allow_shell_hooks_default_off() {
        let cfg = ProjectTrackerConfig::from_yaml_str("program_phase: { enabled: true }").unwrap();
        assert!(!cfg.program_phase.allow_shell_hooks);
    }
}
