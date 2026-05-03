//! Phase 81.15.a / 81.17.c — out-of-tree subprocess plugin template.
//!
//! Echoes every inbound event back to the broker on the matching
//! `plugin.inbound.<kind>` topic. Replace the `on_broker_event`
//! handler with your channel's real outbound logic.
//!
//! Wire format spec: `nexo-plugin-contract.md` at the workspace
//! root. Reference SDK: `nexo_microapp_sdk::plugin::PluginAdapter`.

use nexo_broker::Event;
use nexo_microapp_sdk::plugin::{BrokerSender, PluginAdapter};

const MANIFEST: &str = include_str!("../nexo-plugin.toml");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Stderr logging — daemon's stdio→tracing bridge (Phase 81.23,
    // pending) will fold this into the operator's structured log
    // stream. Until then it's debug visibility for operators
    // running the binary directly.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    tracing::info!("template-plugin-rust starting");

    PluginAdapter::new(MANIFEST)?
        .on_broker_event(handle_event)
        .on_shutdown(|| async {
            tracing::info!("template-plugin-rust shutdown handler invoked");
            Ok(())
        })
        .run_stdio()
        .await?;
    Ok(())
}

/// Demo handler — echoes the inbound payload back as an outbound
/// event on `plugin.inbound.template_echo`. Real plugins replace
/// this with their channel's outbound logic (e.g. forward to
/// Slack API, then publish the API's reply back through the
/// broker so agents can observe it).
async fn handle_event(topic: String, event: Event, broker: BrokerSender) {
    tracing::info!(
        topic = %topic,
        source = %event.source,
        payload = %event.payload,
        "template-plugin-rust received broker.event"
    );

    // Build the echo event. `Event::new(topic, source, payload)`
    // produces a fresh UUID + current timestamp; the daemon's
    // bridge re-checks the topic against the allowlist before
    // forwarding to the broker.
    let echo = Event::new(
        "plugin.inbound.template_echo",
        "template_plugin_rust",
        serde_json::json!({
            "echoed_from": topic,
            "echoed_payload": event.payload,
        }),
    );

    if let Err(e) = broker.publish("plugin.inbound.template_echo", echo).await {
        tracing::warn!(error = %e, "template-plugin-rust echo publish failed");
    }
}
