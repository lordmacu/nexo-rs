/// Manual smoke test for the browser CDP plugin.
/// Launches Chromium, navigates to a URL, takes a screenshot, saves PNG.
/// Run with:  cargo run --bin browser-test -- https://example.com
use std::sync::Arc;
use std::path::PathBuf;

use anyhow::Context;
use agent_config::BrowserConfig;
use agent_plugin_browser::{ChromeLauncher, CdpClient, CdpSession};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let url = std::env::args().nth(1)
        .unwrap_or_else(|| "https://example.com".to_string());

    let config = BrowserConfig {
        headless: true,
        executable: String::new(),
        cdp_url: String::new(),
        user_data_dir: "./data/browser/test-profile".to_string(),
        window_width: 1280,
        window_height: 800,
        connect_timeout_ms: 15_000,
        command_timeout_ms: 15_000,
    };

    tracing::info!("launching Chromium...");
    let chrome = ChromeLauncher::launch(&config).await
        .context("failed to launch Chrome")?;
    tracing::info!(pid = chrome.pid, ws_url = %chrome.ws_url, "Chrome ready");

    let client = Arc::new(
        CdpClient::connect(&chrome.ws_url).await
            .context("CDP connect failed")?
    );

    // GET /json/list to find the first page target
    let http_base = chrome.ws_url
        .replace("ws://", "http://")
        .split("/devtools/")
        .next()
        .unwrap_or("http://127.0.0.1:9222")
        .to_string();

    let targets_raw = reqwest::Client::new()
        .get(format!("{http_base}/json/list"))
        .send().await?.text().await?;
    let targets: Vec<serde_json::Value> = serde_json::from_str(&targets_raw)?;

    let target_id = targets.iter()
        .find(|t| t["type"] == "page")
        .and_then(|t| t["id"].as_str())
        .context("no page target found")?
        .to_string();

    tracing::info!(target_id = %target_id, "attaching to page");

    let mut session = CdpSession::new(Arc::clone(&client), &target_id, 15_000).await
        .context("session attach failed")?;

    tracing::info!(url = %url, "navigating...");
    session.navigate(&url).await?;
    tracing::info!("navigation done");

    // Snapshot
    let snapshot = session.snapshot().await?;
    tracing::info!("snapshot:\n{}", &snapshot[..snapshot.len().min(800)]);

    // Screenshot
    tracing::info!("taking screenshot...");
    let png_bytes = session.screenshot().await?;

    let out_path = PathBuf::from("screenshot.png");
    std::fs::write(&out_path, &png_bytes)?;
    tracing::info!(
        path = %out_path.display(),
        bytes = png_bytes.len(),
        "screenshot saved"
    );

    // Evaluate
    let title = session.evaluate("document.title").await?;
    tracing::info!(title = %title, "page title");

    println!("\n✅  Test passed");
    println!("   URL:        {url}");
    println!("   Title:      {title}");
    println!("   Screenshot: {} bytes → {}", png_bytes.len(), out_path.display());
    println!("   Snapshot refs: {} lines", snapshot.lines().count());

    Ok(())
}

