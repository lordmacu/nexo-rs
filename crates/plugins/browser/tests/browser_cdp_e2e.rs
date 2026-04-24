//! End-to-end integration test against a real Chrome DevTools endpoint.
//!
//! Gated by the `CDP_URL` env var — when unset, the test is skipped (prints a
//! notice and returns Ok). When set (e.g. `CDP_URL=http://127.0.0.1:9222`), it
//! exercises the full discover → createTarget → attach → navigate →
//! captureScreenshot → evaluate flow against a live Chrome.
//!
//! Typical invocation against the docker-compose stack:
//!     CDP_URL=http://127.0.0.1:9222 cargo test -p agent-plugin-browser --test browser_cdp_e2e -- --nocapture

use std::sync::Arc;

use agent_plugin_browser::{CdpClient, CdpSession};
use serde_json::json;

#[tokio::test]
async fn full_e2e_flow_against_real_chrome() -> anyhow::Result<()> {
    let Ok(cdp_url) = std::env::var("CDP_URL") else {
        eprintln!("CDP_URL not set — skipping browser E2E test");
        return Ok(());
    };

    let ws_url = CdpClient::discover_ws_url(&cdp_url, 5_000).await?;
    assert!(
        ws_url.starts_with("ws://") || ws_url.starts_with("wss://"),
        "expected ws(s):// URL, got {ws_url}"
    );
    // discover_ws_url must rewrite the authority back to the one we reached Chrome at —
    // otherwise Chrome's echoed Host would leave us with an unreachable ws://localhost/...
    let expected_authority = cdp_url
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    assert!(
        ws_url.contains(expected_authority),
        "ws_url {ws_url} missing expected authority {expected_authority}"
    );

    let client = Arc::new(CdpClient::connect(&ws_url).await?);

    let target = client
        .send("Target.createTarget", json!({ "url": "about:blank" }))
        .await?;
    let target_id = target["targetId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no targetId"))?
        .to_string();

    let mut session = CdpSession::new(Arc::clone(&client), &target_id, 15_000).await?;
    session.navigate("data:text/html,<h1>hello</h1>").await?;

    let png = session.screenshot().await?;
    assert!(
        png.len() > 100,
        "screenshot unexpectedly small: {} bytes",
        png.len()
    );
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");

    let title = session
        .evaluate("document.querySelector('h1').innerText")
        .await?;
    assert_eq!(title.as_str(), Some("hello"));

    // Clean up so repeated runs don't leak targets.
    let _ = client
        .send("Target.closeTarget", json!({ "targetId": target_id }))
        .await;

    Ok(())
}
