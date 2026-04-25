//! `agent pollers <subcommand>` HTTP client. Hits the loopback admin
//! endpoint at `127.0.0.1:9091`. The daemon must be running.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const ADMIN_BASE: &str = "http://127.0.0.1:9091";

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("reqwest client")
}

pub async fn list(json: bool) -> Result<()> {
    let body: Value = http()
        .get(format!("{ADMIN_BASE}/admin/pollers"))
        .send()
        .await
        .context("connect admin server (is the daemon running?)")?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let arr = body.as_array().ok_or_else(|| anyhow!("expected array"))?;
    if arr.is_empty() {
        println!("No pollers configured.");
        return Ok(());
    }
    println!(
        "{:<24} {:<14} {:<10} {:<10} {:<10} {:<8}",
        "ID", "KIND", "AGENT", "PAUSED", "STATUS", "ERR"
    );
    for j in arr {
        println!(
            "{:<24} {:<14} {:<10} {:<10} {:<10} {:<8}",
            j["id"].as_str().unwrap_or(""),
            j["kind"].as_str().unwrap_or(""),
            j["agent"].as_str().unwrap_or(""),
            j["paused"].as_bool().unwrap_or(false),
            j["last_status"].as_str().unwrap_or("-"),
            j["consecutive_errors"].as_i64().unwrap_or(0),
        );
    }
    Ok(())
}

pub async fn show(id: &str, json: bool) -> Result<()> {
    let body: Value = http()
        .get(format!("{ADMIN_BASE}/admin/pollers/{id}"))
        .send()
        .await
        .context("connect admin server")?
        .error_for_status()
        .with_context(|| format!("fetch '{id}'"))?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!("id:                   {}", body["id"].as_str().unwrap_or(""));
        println!("kind:                 {}", body["kind"].as_str().unwrap_or(""));
        println!("agent:                {}", body["agent"].as_str().unwrap_or(""));
        println!("paused:               {}", body["paused"].as_bool().unwrap_or(false));
        println!("last_status:          {}", body["last_status"].as_str().unwrap_or("-"));
        println!("consecutive_errors:   {}", body["consecutive_errors"].as_i64().unwrap_or(0));
        println!(
            "items_seen_total:     {}",
            body["items_seen_total"].as_i64().unwrap_or(0)
        );
        println!(
            "items_dispatched_total: {}",
            body["items_dispatched_total"].as_i64().unwrap_or(0)
        );
        if let Some(err) = body["last_error"].as_str() {
            println!("last_error:           {err}");
        }
    }
    Ok(())
}

pub async fn run(id: &str) -> Result<()> {
    let body: Value = http()
        .post(format!("{ADMIN_BASE}/admin/pollers/{id}/run"))
        .send()
        .await
        .context("connect admin server")?
        .json()
        .await?;
    if body["ok"].as_bool().unwrap_or(false) {
        println!(
            "✔ tick ok — items_seen={}, items_dispatched={}, deliveries={}",
            body["items_seen"].as_u64().unwrap_or(0),
            body["items_dispatched"].as_u64().unwrap_or(0),
            body["deliveries"].as_u64().unwrap_or(0),
        );
        Ok(())
    } else {
        Err(anyhow!(
            "tick failed: {}",
            body["error"].as_str().unwrap_or("unknown")
        ))
    }
}

pub async fn pause(id: &str) -> Result<()> {
    post_action(id, "pause").await
}

pub async fn resume(id: &str) -> Result<()> {
    post_action(id, "resume").await
}

pub async fn reset(id: &str, yes: bool) -> Result<()> {
    if !yes {
        return Err(anyhow!(
            "reset is destructive (clears cursor + paused + errors); pass --yes to confirm"
        ));
    }
    post_action(id, "reset").await
}

pub async fn reload() -> Result<()> {
    let body: Value = http()
        .post(format!("{ADMIN_BASE}/admin/pollers/reload"))
        .send()
        .await
        .context("connect admin server")?
        .json()
        .await?;
    if body["ok"].as_bool().unwrap_or(false) {
        let summary = |key: &str| -> String {
            body[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        println!("✔ reload applied");
        println!("  add:     [{}]", summary("add"));
        println!("  replace: [{}]", summary("replace"));
        println!("  remove:  [{}]", summary("remove"));
        println!("  keep:    [{}]", summary("keep"));
        Ok(())
    } else {
        Err(anyhow!(
            "reload failed: {}",
            body["error"].as_str().unwrap_or("unknown")
        ))
    }
}

async fn post_action(id: &str, action: &str) -> Result<()> {
    let body: Value = http()
        .post(format!("{ADMIN_BASE}/admin/pollers/{id}/{action}"))
        .send()
        .await
        .context("connect admin server")?
        .json()
        .await?;
    if body["ok"].as_bool().unwrap_or(false) {
        println!("✔ {action} ok");
        Ok(())
    } else {
        Err(anyhow!(
            "{action} failed: {}",
            body["error"].as_str().unwrap_or("unknown")
        ))
    }
}
