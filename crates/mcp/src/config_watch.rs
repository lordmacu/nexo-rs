//! Phase 12.8 — file watcher for `mcp.yaml`.
//!
//! Observes `{config_dir}/mcp.yaml` and calls
//! `McpRuntimeManager::update_config` on stable content changes. Opt-in
//! via `mcp.watch.enabled: true`. Invalid YAML and missing env vars are
//! logged at `warn` and skipped — the last good config keeps running.
//!
//! Also observes `{config_dir}/extensions.yaml` and re-computes
//! extension-declared MCP servers so `agent ext enable/disable` can hot
//! reload the MCP runtime without a process restart.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use agent_config::env::resolve_placeholders;
use agent_config::types::{ExtensionsConfig, ExtensionsConfigFile, McpConfig, McpConfigFile};

use crate::manager::McpRuntimeManager;
use crate::runtime_config::{ExtensionServerDecl, McpRuntimeConfig};

pub const MCP_YAML_FILENAME: &str = "mcp.yaml";
pub const EXTENSIONS_YAML_FILENAME: &str = "extensions.yaml";

/// Spawn a background task that watches `config_dir/mcp.yaml` and calls
/// `manager.update_config` on stable changes. Returns immediately. The
/// task stops when `shutdown` is cancelled.
pub fn spawn_mcp_config_watcher(
    config_dir: PathBuf,
    manager: Arc<McpRuntimeManager>,
    ext_servers: Vec<ExtensionServerDecl>,
    extension_defaults: Option<ExtensionsConfig>,
    debounce: Duration,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        if let Err(e) = run_watcher(
            config_dir,
            manager,
            ext_servers,
            extension_defaults,
            debounce,
            shutdown,
        )
        .await
        {
            tracing::warn!(error = %e, "mcp config watcher terminated");
        }
    });
}

async fn run_watcher(
    config_dir: PathBuf,
    manager: Arc<McpRuntimeManager>,
    ext_servers: Vec<ExtensionServerDecl>,
    extension_defaults: Option<ExtensionsConfig>,
    debounce: Duration,
    shutdown: CancellationToken,
) -> Result<()> {
    let target_path = config_dir.join(MCP_YAML_FILENAME);
    let ext_cfg_path = config_dir.join(EXTENSIONS_YAML_FILENAME);
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();
    let target_for_handler = target_path.clone();
    let ext_for_handler = ext_cfg_path.clone();

    // `new_debouncer` spawns its own OS thread with a blocking callback.
    // We relay a coarse `()` signal per stable batch to the async side.
    let mut debouncer = new_debouncer(debounce, None, move |res: DebounceEventResult| {
        let events = match res {
            Ok(events) => events,
            Err(errs) => {
                for e in errs {
                    tracing::warn!(error = %e, "mcp config watcher error");
                }
                return;
            }
        };
        let relevant = events.iter().any(|e| {
            e.paths
                .iter()
                .any(|p| p == &target_for_handler || p == &ext_for_handler)
        });
        if relevant {
            let _ = tx.send(());
        }
    })
    .context("failed to construct debouncer")?;

    // Watch the parent directory non-recursively so we catch atomic renames
    // (editor temp-file → rename to `mcp.yaml`) that a direct file watch
    // would miss once the inode changes.
    debouncer
        .watcher()
        .watch(&config_dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watch({})", config_dir.display()))?;

    tracing::info!(path = %target_path.display(), "mcp config watcher started");
    tracing::info!(
        path = %ext_cfg_path.display(),
        "extensions config watcher piggyback enabled"
    );

    let mut ext_servers_current = ext_servers;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            maybe = rx.recv() => {
                if maybe.is_none() {
                    break;
                }
                match collect_extension_servers(&config_dir, extension_defaults.as_ref()) {
                    Ok(next) => {
                        ext_servers_current = next;
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %ext_cfg_path.display(),
                            error = %e,
                            "extensions config reload skipped; keeping previous extension server set"
                        );
                    }
                }
                match read_and_parse(&target_path) {
                    Ok(cfg) => {
                        let rt_cfg = McpRuntimeConfig::from_yaml_with_extensions(
                            &cfg, &ext_servers_current,
                        );
                        manager.update_config(rt_cfg).await;
                        tracing::info!(
                            path = %target_path.display(),
                            servers = cfg.servers.len(),
                            extension_servers = ext_servers_current.len(),
                            "mcp config reloaded"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %target_path.display(),
                            error = %e,
                            "mcp config reload skipped"
                        );
                    }
                }
            }
        }
    }
    tracing::info!("mcp config watcher stopped");
    Ok(())
}

fn collect_extension_servers(
    config_dir: &Path,
    fallback: Option<&ExtensionsConfig>,
) -> Result<Vec<ExtensionServerDecl>> {
    let cfg = read_extensions_config(config_dir, fallback)?;
    if !cfg.enabled {
        return Ok(Vec::new());
    }
    let search_paths: Vec<PathBuf> = cfg.search_paths.iter().map(PathBuf::from).collect();
    let report = agent_extensions::ExtensionDiscovery::new(
        search_paths,
        cfg.ignore_dirs.clone(),
        cfg.disabled.clone(),
        cfg.allowlist.clone(),
        cfg.max_depth,
    )
    .with_follow_links(cfg.follow_links)
    .discover();

    for d in &report.diagnostics {
        match d.level {
            agent_extensions::DiagnosticLevel::Warn => tracing::warn!(
                path = %d.path.display(),
                message = %d.message,
                "extension discovery during mcp reload"
            ),
            agent_extensions::DiagnosticLevel::Error => tracing::error!(
                path = %d.path.display(),
                message = %d.message,
                "extension discovery during mcp reload"
            ),
        }
    }

    let out = agent_extensions::collect_mcp_declarations(&report, &cfg.disabled)
        .into_iter()
        .map(|d| ExtensionServerDecl {
            ext_id: d.ext_id,
            ext_version: d.ext_version,
            ext_root: d.ext_root,
            servers: d.servers,
        })
        .collect();
    Ok(out)
}

fn read_extensions_config(
    config_dir: &Path,
    fallback: Option<&ExtensionsConfig>,
) -> Result<ExtensionsConfig> {
    let on_disk =
        agent_config::load_optional::<ExtensionsConfigFile>(config_dir, EXTENSIONS_YAML_FILENAME)?
            .map(|f| f.extensions);
    Ok(on_disk.or_else(|| fallback.cloned()).unwrap_or_default())
}

/// Read `mcp.yaml`, resolve `${ENV_VAR}` placeholders, parse into
/// `McpConfig`. Surfaces any step's error as `anyhow`.
fn read_and_parse(path: &Path) -> Result<McpConfig> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let resolved = resolve_placeholders(&raw, MCP_YAML_FILENAME)
        .with_context(|| format!("env resolve {}", path.display()))?;
    let file: McpConfigFile =
        serde_yaml::from_str(&resolved).with_context(|| format!("parse {}", path.display()))?;
    Ok(file.mcp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_and_parse_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MCP_YAML_FILENAME);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "mcp:\n  enabled: true\n  servers: {{}}").unwrap();
        let cfg = read_and_parse(&path).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn read_and_parse_missing_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MCP_YAML_FILENAME);
        let err = read_and_parse(&path).unwrap_err();
        assert!(err.to_string().contains("read"));
    }

    #[test]
    fn read_and_parse_invalid_yaml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MCP_YAML_FILENAME);
        std::fs::write(&path, "mcp: [not, a, map").unwrap();
        let err = read_and_parse(&path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("parse") || err.root_cause().to_string().contains("yaml"));
    }
}
