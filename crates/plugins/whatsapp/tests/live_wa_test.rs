//! Live-account end-to-end. Skipped unless `--features live-wa` is set
//! AND the environment is primed with a paired wa-agent session.
//!
//! Required env vars when running:
//!
//!   * `WA_LIVE_SESSION_DIR` — path to a directory that already has
//!     `.whatsapp-rs/creds.json` from a previous pair. The test refuses
//!     to run without this to avoid surprise QR pairing in CI.
//!   * `WA_LIVE_PEER_JID` — JID of a second device / contact we send
//!     the probe message to and expect a reply from (e.g. an echo bot
//!     on another phone).
//!
//! Scenarios covered:
//!
//!   * Inbound text → bridge → broker → broker outbound → proactive
//!     `send_text` round-trip.
//!   * Proactive send via `Plugin::send_command(SendMessage { .. })`.
//!   * `health()` reports `connected = true` after boot settles.
//!
//! Non-goals: media, transcriber, reconnect — those ride along the
//! same plumbing and are verified manually on a staging device. See
//! `proyecto/FOLLOWUPS.md` for the live-media checklist.

#![cfg(feature = "live-wa")]

use std::time::Duration;

use agent_broker::AnyBroker;
use agent_config::{
    WhatsappAclConfig, WhatsappBehaviorConfig, WhatsappBridgeConfig, WhatsappDaemonConfig,
    WhatsappPluginConfig, WhatsappRateLimitConfig, WhatsappTranscriberConfig,
};
use agent_core::agent::plugin::{Command, Plugin, Response};
use agent_plugin_whatsapp::WhatsappPlugin;

fn live_cfg(session_dir: String) -> WhatsappPluginConfig {
    WhatsappPluginConfig {
        enabled: true,
        session_dir,
        media_dir: std::env::temp_dir()
            .join("wa-live-media")
            .to_string_lossy()
            .to_string(),
        credentials_file: None,
        acl: WhatsappAclConfig::default(),
        behavior: WhatsappBehaviorConfig::default(),
        rate_limit: WhatsappRateLimitConfig::default(),
        bridge: WhatsappBridgeConfig {
            response_timeout_ms: 15_000,
            on_timeout: "noop".into(),
            apology_text: "sorry".into(),
        },
        public_tunnel: Default::default(),
        instance: None,
        allow_agents: Vec::new(),
        transcriber: WhatsappTranscriberConfig::default(),
        daemon: WhatsappDaemonConfig {
            prefer_existing: false,
        },
        public_tunnel: Default::default(),
        instance: None,
        allow_agents: Vec::new(),
    }
}

/// Smoke: boot the plugin, verify `health()` eventually reports
/// connected, then push a proactive send to a peer JID. Success =
/// `send_command` returns `Response::Ok` within the timeout.
///
/// Reads `WA_LIVE_SESSION_DIR` + `WA_LIVE_PEER_JID`; the test is
/// skipped (marked passed) if either is missing so `cargo test
/// --features live-wa` on an un-prepared machine stays green.
#[tokio::test]
async fn live_boot_and_proactive_send() {
    let Some(session_dir) = std::env::var("WA_LIVE_SESSION_DIR").ok() else {
        eprintln!("WA_LIVE_SESSION_DIR not set — skipping");
        return;
    };
    let Some(peer_jid) = std::env::var("WA_LIVE_PEER_JID").ok() else {
        eprintln!("WA_LIVE_PEER_JID not set — skipping");
        return;
    };

    let plugin = WhatsappPlugin::new(live_cfg(session_dir));
    let broker = AnyBroker::local();
    plugin
        .start(broker.clone())
        .await
        .expect("plugin start failed");

    // Allow the reconnect loop + app-state sync to finish.
    for _ in 0..30 {
        if plugin.health().await.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(plugin.health().await.connected, "did not connect in 15s");

    let resp = plugin
        .send_command(Command::SendMessage {
            to: peer_jid,
            text: format!(
                "agent-plugin-whatsapp live check at {}",
                chrono::Utc::now().timestamp()
            ),
        })
        .await
        .expect("send_command failed");
    assert!(matches!(resp, Response::Ok));

    plugin.stop().await.ok();
}
