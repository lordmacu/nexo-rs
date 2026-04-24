//! Gmail-poller plugin — ticks Gmail on a fixed interval, extracts
//! structured fields from matching emails via regex, and routes them
//! to any channel plugin. Zero LLM involvement in the hot path.
//!
//! Supports per-agent OAuth accounts: each entry in `accounts:` owns
//! its own credentials + token file, and each job names the account
//! it wants to poll. Back-compat shim synthesizes a `"default"`
//! account from the top-level `token_path` / `client_id_path` /
//! `client_secret_path` fields when `accounts` is absent.

pub mod config;
pub mod poll;

use std::collections::HashMap;
use std::sync::Arc;

use agent_broker::AnyBroker;
use agent_plugin_google::{GoogleAuthClient, GoogleAuthConfig};
use anyhow::{Context, Result};

pub use config::{AccountConfig, GmailPollerConfig, JobConfig};

/// Resolve the account list from config, expanding the back-compat
/// single-account shorthand when needed. Errors if neither form
/// yields at least one account.
fn resolve_accounts(cfg: &GmailPollerConfig) -> Result<Vec<AccountConfig>> {
    if !cfg.accounts.is_empty() {
        return Ok(cfg.accounts.clone());
    }
    let Some(token_path) = cfg.token_path.as_deref() else {
        anyhow::bail!(
            "gmail-poller: no `accounts` list and no top-level `token_path` — nothing to poll"
        );
    };
    let client_id_path = cfg
        .client_id_path
        .clone()
        .unwrap_or_else(|| "./secrets/google_client_id.txt".to_string());
    let client_secret_path = cfg
        .client_secret_path
        .clone()
        .unwrap_or_else(|| "./secrets/google_client_secret.txt".to_string());
    Ok(vec![AccountConfig {
        id: "default".to_string(),
        token_path: token_path.to_string(),
        client_id_path,
        client_secret_path,
        agent_id: None,
    }])
}

/// Build one `GoogleAuthClient` per configured account. The account's
/// `token_path` is absolute; we pass `"/"` as the workspace base so
/// `new()` uses the absolute path directly.
async fn build_clients(
    accounts: &[AccountConfig],
) -> Result<HashMap<String, Arc<GoogleAuthClient>>> {
    let mut out = HashMap::new();
    for acc in accounts {
        let client_id = read_trimmed(&acc.client_id_path)
            .with_context(|| format!("read {}", acc.client_id_path))?;
        let client_secret = read_trimmed(&acc.client_secret_path)
            .with_context(|| format!("read {}", acc.client_secret_path))?;
        let cfg = GoogleAuthConfig {
            client_id,
            client_secret,
            scopes: Vec::new(),
            token_file: acc.token_path.clone(),
            redirect_port: 0,
        };
        let client = GoogleAuthClient::new(cfg, std::path::Path::new("/"));
        client
            .load_from_disk()
            .await
            .with_context(|| format!("load_from_disk for account `{}`", acc.id))?;
        out.insert(acc.id.clone(), client);
    }
    Ok(out)
}

fn read_trimmed(path: &str) -> Result<String> {
    Ok(std::fs::read_to_string(path)?.trim().to_string())
}

/// Spawn one tokio task per configured job, each using the right
/// `GoogleAuthClient` for its account. Errors inside a tick log at
/// warn and do NOT crash the poller — transient problems are
/// absorbed, the next tick just retries.
pub async fn spawn(cfg: GmailPollerConfig, broker: AnyBroker) -> Result<()> {
    if !cfg.enabled {
        tracing::info!("gmail-poller: disabled (enabled=false)");
        return Ok(());
    }

    let accounts = resolve_accounts(&cfg)?;
    let clients = build_clients(&accounts).await?;
    tracing::info!(
        accounts = accounts.len(),
        jobs = cfg.jobs.len(),
        "gmail-poller: initialized"
    );

    // Persistent dedup caches live next to each account's token file.
    // Keyed by account id so jobs sharing an account reuse the cache.
    let state_dirs: HashMap<String, std::path::PathBuf> = accounts
        .iter()
        .map(|a| {
            let dir = std::path::Path::new(&a.token_path)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("./data"));
            (a.id.clone(), dir)
        })
        .collect();

    let default_interval = cfg.interval_secs;
    for job in cfg.jobs {
        let google = clients.get(&job.account).cloned().with_context(|| {
            format!(
                "job `{}` references unknown account `{}` — available: [{}]",
                job.name,
                job.account,
                accounts
                    .iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let state_dir = state_dirs
            .get(&job.account)
            .cloned()
            .unwrap_or_else(|| std::path::PathBuf::from("./data"));
        let compiled = Arc::new(
            poll::CompiledJob::new(job, &state_dir)
                .context("gmail-poller: job compile failed")?,
        );
        let interval = compiled.cfg.interval_secs.unwrap_or(default_interval);
        let broker = broker.clone();
        let name = compiled.cfg.name.clone();
        let account = compiled.cfg.account.clone();
        tokio::spawn(async move {
            tracing::info!(
                job = %name,
                account = %account,
                interval_secs = interval,
                "gmail-poller: job started"
            );
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(interval));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            let mut consecutive_errors = 0u32;
            loop {
                ticker.tick().await;
                match poll::run_once(&compiled, &google, &broker).await {
                    Ok(n) => {
                        if n > 0 {
                            tracing::info!(
                                job = %name,
                                dispatched = n,
                                "gmail-poller: tick ok"
                            );
                        }
                        consecutive_errors = 0;
                    }
                    Err(e) => {
                        consecutive_errors += 1;
                        let extra = extra_backoff_secs(consecutive_errors);
                        tracing::warn!(
                            job = %name,
                            error = %e,
                            consecutive_errors,
                            extra_sleep_secs = extra,
                            "gmail-poller: tick failed"
                        );
                        if extra > 0 {
                            tokio::time::sleep(std::time::Duration::from_secs(extra))
                                .await;
                        }
                    }
                }
            }
        });
    }
    Ok(())
}

/// Back-off schedule for sustained errors. 0 for the first three
/// failures (the ticker interval alone is enough), then 30/60/120s
/// capped at 300s. Resets on first success.
fn extra_backoff_secs(errors: u32) -> u64 {
    match errors {
        0 | 1 | 2 | 3 => 0,
        4 => 30,
        5 => 60,
        6 => 120,
        _ => 300,
    }
}
