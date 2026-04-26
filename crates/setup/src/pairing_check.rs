//! Phase 70.6 — pairing-store audit run by `agent setup doctor`.
//!
//! Operators wire `pairing.auto_challenge: true` on a binding and
//! then forget that the `pairing_allow_from` table is empty on first
//! boot. The result is a silent gate: every inbound message is
//! treated as an unknown sender and either challenged or dropped.
//! This audit walks the loaded `AppConfig`, identifies bindings with
//! the gate enabled, and reports the (channel, account_id) tuples
//! that have no allowlisted senders. The fix it suggests is the
//! direct `nexo pair seed` command rather than the QR flow, since the
//! latter needs a non-loopback gateway URL the dev box rarely has.
//!
//! Best-effort: the function returns an empty `Vec` on any I/O or
//! parse failure so the doctor pass keeps moving. Operators that
//! want the verbose error path can run `nexo pair list --all` and
//! see the table for themselves.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct EmptyPairing {
    pub agent_id: String,
    pub channel: String,
    pub account_id: String,
}

/// Walk every binding with `auto_challenge: true` and report those
/// whose `(channel, account_id)` tuple has zero active rows in
/// `pairing_allow_from`. The audit is read-only and never mutates
/// the store. Async because the underlying store uses sqlx; the
/// caller (`run_doctor`) lives inside the `#[tokio::main]` runtime
/// so awaiting here is free.
pub async fn audit(config_dir: &Path) -> Vec<EmptyPairing> {
    let Ok(cfg) = nexo_config::AppConfig::load(config_dir) else {
        return Vec::new();
    };
    let store_path = resolve_store_path(config_dir, &cfg);
    let store = match nexo_pairing::PairingStore::open(
        store_path.to_str().unwrap_or("pairing.db"),
    )
    .await
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut findings: Vec<EmptyPairing> = Vec::new();
    for agent in &cfg.agents.agents {
        for binding in &agent.inbound_bindings {
            let policy_value = if !binding.pairing_policy.is_null() {
                &binding.pairing_policy
            } else {
                &agent.pairing_policy
            };
            if !auto_challenge_enabled(policy_value) {
                continue;
            }
            let channel = binding.plugin.as_str();
            let account = binding.instance.as_deref().unwrap_or("default");
            let rows = match store.list_allow(Some(channel), false).await {
                Ok(rs) => rs,
                Err(_) => continue,
            };
            let active = rows.iter().any(|r| r.account_id == account);
            if !active {
                findings.push(EmptyPairing {
                    agent_id: agent.id.clone(),
                    channel: channel.to_string(),
                    account_id: account.to_string(),
                });
            }
        }
    }
    findings
}

pub fn print(findings: &[EmptyPairing]) {
    if findings.is_empty() {
        return;
    }
    println!();
    println!("── Pairing store (Phase 70.6) ──────────────────");
    println!(
        "  ⚠  {} binding(s) have `pairing.auto_challenge: true` but no allowlisted senders.",
        findings.len()
    );
    println!("  Until you seed at least one sender, every inbound message will be challenged or dropped.");
    println!();
    for f in findings {
        println!(
            "    • agent={} channel={} account={}",
            f.agent_id, f.channel, f.account_id
        );
        println!(
            "      fix:  nexo pair seed {} {} <YOUR_SENDER_ID>",
            f.channel, f.account_id
        );
    }
    println!();
    println!("  Verify after seeding with:  nexo pair list --all");
}

fn resolve_store_path(config_dir: &Path, cfg: &nexo_config::AppConfig) -> PathBuf {
    if let Some(p) = cfg.pairing.as_ref().and_then(|p| p.storage.path.clone()) {
        return PathBuf::from(p);
    }
    let memory_dir: PathBuf = cfg
        .memory
        .long_term
        .sqlite
        .as_ref()
        .map(|s| {
            Path::new(&s.path)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."))
        })
        .unwrap_or_else(|| config_dir.parent().unwrap_or(Path::new(".")).to_path_buf());
    memory_dir.join("pairing.db")
}

fn auto_challenge_enabled(value: &serde_json::Value) -> bool {
    if value.is_null() {
        return false;
    }
    value
        .get("auto_challenge")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auto_challenge_flag_round_trip() {
        assert!(!auto_challenge_enabled(&serde_json::Value::Null));
        assert!(!auto_challenge_enabled(&json!({})));
        assert!(!auto_challenge_enabled(&json!({"auto_challenge": false})));
        assert!(auto_challenge_enabled(&json!({"auto_challenge": true})));
    }
}
