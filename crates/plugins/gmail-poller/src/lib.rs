//! Gmail-poller plugin — ticks Gmail on a fixed interval, extracts
//! structured fields from matching emails via regex, and routes them
//! to any channel plugin. Zero LLM involvement in the hot path.
//!
//! Typical use case: forward sales lead emails to a WhatsApp number
//! the moment they arrive. See `docs/layering.md` for where this sits
//! in the plugin taxonomy.

pub mod config;
pub mod poll;

use std::sync::Arc;

use agent_broker::AnyBroker;
use agent_plugin_google::{GoogleAuthClient, GoogleAuthConfig};
use anyhow::{Context, Result};

pub use config::{GmailPollerConfig, JobConfig};

/// Spawn one tokio task per configured job. Each task loops forever
/// until the runtime shuts down. Errors inside a tick log at warn and
/// do NOT crash the poller — transient Gmail / network problems are
/// absorbed, the next tick just retries.
pub async fn spawn(
    cfg: GmailPollerConfig,
    broker: AnyBroker,
) -> Result<()> {
    if !cfg.enabled {
        tracing::info!("gmail-poller: disabled (enabled=false)");
        return Ok(());
    }

    // Build a standalone GoogleAuthClient pointed at the configured
    // token file. We intentionally DON'T share the client with the
    // agent-level google plugin — the poller has its own copy so a
    // concurrent refresh on either side can't race on the mutex.
    let google_cfg = GoogleAuthConfig {
        client_id: std::env::var("GOOGLE_CLIENT_ID")
            .ok()
            .unwrap_or_else(|| read_secret_file("google_client_id.txt")),
        client_secret: std::env::var("GOOGLE_CLIENT_SECRET")
            .ok()
            .unwrap_or_else(|| read_secret_file("google_client_secret.txt")),
        scopes: Vec::new(),
        token_file: cfg.token_path.clone(),
        redirect_port: 0,
    };
    let workspace = std::path::Path::new("/"); // token_file is absolute, workspace unused
    let google = GoogleAuthClient::new(google_cfg, workspace);
    google
        .load_from_disk()
        .await
        .context("gmail-poller: load_from_disk failed")?;

    let default_interval = cfg.interval_secs;
    for job in cfg.jobs {
        let compiled = poll::CompiledJob::new(job)
            .context("gmail-poller: job compile failed")?;
        let interval = compiled.cfg.interval_secs.unwrap_or(default_interval);
        let broker = broker.clone();
        let google = Arc::clone(&google);
        let name = compiled.cfg.name.clone();
        tokio::spawn(async move {
            tracing::info!(job = %name, interval_secs = interval, "gmail-poller: job started");
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval));
            // Skip the immediate fire at t=0 — let the agent finish
            // booting first. Tick fires at t=interval instead.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match poll::run_once(&compiled, &google, &broker).await {
                    Ok(n) if n > 0 => {
                        tracing::info!(job = %name, dispatched = n, "gmail-poller: tick ok");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(job = %name, error = %e, "gmail-poller: tick failed");
                    }
                }
            }
        });
    }
    Ok(())
}

/// Best-effort read of a relative secret file from `./secrets/`. Used
/// only as a fallback when the env var isn't set — keeps the plugin
/// functional in host runs that don't export GOOGLE_CLIENT_ID.
fn read_secret_file(name: &str) -> String {
    std::fs::read_to_string(std::path::Path::new("./secrets").join(name))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}
