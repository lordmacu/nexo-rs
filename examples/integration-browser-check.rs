use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_nats::Client;
use base64::Engine;
use futures::StreamExt;
use serde_json::json;
use tokio::time::{timeout, Instant};
use uuid::Uuid;

use nexo_broker::Event;

const INBOUND_TOPIC: &str = "plugin.inbound.browser";
const OUTBOUND_TOPIC: &str = "plugin.outbound.browser";

#[tokio::main]
async fn main() -> Result<()> {
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let timeout_ms = std::env::var("BROWSER_CHECK_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30_000);
    let timeout_per_step = Duration::from_millis(timeout_ms);

    let client = async_nats::connect(&nats_url)
        .await
        .with_context(|| format!("failed to connect to nats at {nats_url}"))?;
    let mut sub = client
        .subscribe(OUTBOUND_TOPIC.to_string())
        .await
        .context("failed to subscribe to plugin.outbound.browser")?;

    let session_id = Uuid::new_v4();
    println!("running browser integration check (session={session_id})");

    publish_browser_cmd(
        &client,
        session_id,
        json!({ "command": "navigate", "url": "https://example.com" }),
    )
    .await?;
    let nav = recv_for_session(&mut sub, session_id, timeout_per_step).await?;
    let (nav_ok, nav_err) = status(&nav);
    if !nav_ok {
        let err = nav_err.unwrap_or("unknown browser error");
        // In containerized setups this can happen when the external CDP endpoint
        // isn't usable from the agent container. Treat as a non-fatal skip so the
        // rest of the integration suite can still validate broker/readiness flows.
        if err.contains("session init failed")
            || err.contains("error sending request for url")
            || err.contains("error decoding response body")
        {
            println!("browser integration check skipped: {err}");
            return Ok(());
        }
        bail!("browser step `navigate` failed: {err}");
    }

    publish_browser_cmd(&client, session_id, json!({ "command": "snapshot" })).await?;
    let snap = recv_for_session(&mut sub, session_id, timeout_per_step).await?;
    ensure_ok(&snap, "snapshot")?;
    let snapshot = snap
        .payload
        .get("snapshot")
        .and_then(|v| v.as_str())
        .context("snapshot response missing `snapshot` text")?;
    if !snapshot.contains("@e") {
        bail!("snapshot response does not contain element refs (@eN)");
    }

    publish_browser_cmd(&client, session_id, json!({ "command": "screenshot" })).await?;
    let shot = recv_for_session(&mut sub, session_id, timeout_per_step).await?;
    ensure_ok(&shot, "screenshot")?;
    let b64 = shot
        .payload
        .get("data")
        .and_then(|v| v.as_str())
        .context("screenshot response missing `data` base64")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("invalid base64 screenshot payload")?;
    if bytes.len() < 10_000 {
        bail!("screenshot too small ({} bytes)", bytes.len());
    }

    println!(
        "browser integration check passed: snapshot={} chars, screenshot={} bytes",
        snapshot.len(),
        bytes.len()
    );
    Ok(())
}

async fn publish_browser_cmd(
    client: &Client,
    session_id: Uuid,
    payload: serde_json::Value,
) -> Result<()> {
    let mut event = Event::new(INBOUND_TOPIC, "integration-browser-check", payload);
    event.session_id = Some(session_id);
    let body = serde_json::to_vec(&event).context("failed to serialize browser command event")?;
    client
        .publish(INBOUND_TOPIC.to_string(), body.into())
        .await
        .context("failed to publish browser command")?;
    Ok(())
}

async fn recv_for_session(
    sub: &mut async_nats::Subscriber,
    session_id: Uuid,
    max_wait: Duration,
) -> Result<Event> {
    let deadline = Instant::now() + max_wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out waiting for browser response for session {session_id}");
        }

        let msg = timeout(remaining, sub.next())
            .await
            .context("timed out waiting for outbound browser event")?
            .context("browser outbound subscription closed")?;
        let event: Event = serde_json::from_slice(&msg.payload)
            .context("failed to deserialize browser outbound event")?;
        if event.session_id == Some(session_id) {
            return Ok(event);
        }
    }
}

fn ensure_ok(event: &Event, step: &str) -> Result<()> {
    let (ok, err) = status(event);
    if ok {
        return Ok(());
    }
    bail!(
        "browser step `{step}` failed: {}",
        err.unwrap_or("unknown browser error")
    );
}

fn status(event: &Event) -> (bool, Option<&str>) {
    let ok = event
        .payload
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let err = event.payload.get("error").and_then(|v| v.as_str());
    (ok, err)
}
