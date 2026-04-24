//! Session bootstrap — directory layout, daemon-collision guard, and
//! the actual `Client::new().connect()` call.
//!
//! Directory layout rationale lives in `docs/wa-agent-integration.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_broker::{AnyBroker, BrokerHandle, Event};
use agent_config::WhatsappPluginConfig;
use anyhow::{Context, Result};

use crate::bridge::SOURCE;
use crate::events::InboundEvent;
use crate::pairing::{QrSnapshot, SharedPairingState};

/// Ensure `session_dir` and `media_dir` exist; return the resolved XDG
/// base we want `wa-agent` to use (i.e. `session_dir` itself — the crate
/// expects the state folder to be the parent of `.whatsapp-rs/`).
pub fn ensure_session_dirs(cfg: &WhatsappPluginConfig) -> Result<PathBuf> {
    let session_dir = PathBuf::from(&cfg.session_dir);
    std::fs::create_dir_all(&session_dir)
        .with_context(|| format!("cannot create session_dir {}", session_dir.display()))?;
    let media_dir = PathBuf::from(&cfg.media_dir);
    std::fs::create_dir_all(&media_dir)
        .with_context(|| format!("cannot create media_dir {}", media_dir.display()))?;
    Ok(session_dir)
}

/// Path where `wa-agent` advertises a running daemon (port + auth token).
/// We look relative to the plugin's `session_dir` first (honors our XDG
/// override), then fall back to the system default for safety.
pub fn daemon_handle_path(cfg: &WhatsappPluginConfig) -> PathBuf {
    PathBuf::from(&cfg.session_dir).join(".whatsapp-rs").join("daemon.json")
}

/// Returns an error when `daemon.prefer_existing` is true and a daemon
/// handle file is present — opening a second WebSocket against the same
/// WhatsApp account would invalidate the live one.
pub fn check_daemon_collision(cfg: &WhatsappPluginConfig) -> Result<()> {
    if !cfg.daemon.prefer_existing {
        return Ok(());
    }
    let handle = daemon_handle_path(cfg);
    if handle.exists() {
        anyhow::bail!(
            "wa-agent daemon handle detected at {} — refusing to open a \
             second WhatsApp socket. Stop the daemon \
             (`systemctl --user stop whatsapp-rs`) or set \
             `daemon.prefer_existing: false` in whatsapp.yaml.",
            handle.display()
        );
    }
    Ok(())
}

/// Deprecated — kept for callers that haven't migrated to the
/// `Client::new_in_dir` path. New code should NOT call this; it mutates
/// process-wide state and breaks multi-account setups (the last call
/// wins, previous accounts then read the wrong data dir).
#[deprecated(note = "Use Client::new_in_dir(session_dir) — this mutates XDG process-wide")]
pub fn apply_xdg_override(session_dir: &Path) {
    // SAFETY: set_var is process-wide; the plugin owns the XDG pointer
    // for the WhatsApp process regardless of the host's shell env.
    std::env::set_var("XDG_DATA_HOME", session_dir);
}

/// Full boot: mkdir, collision check, XDG override, connect. Called from
/// `Plugin::start` — not on plugin construction, because it opens a
/// WebSocket and blocks on QR if creds are missing.
///
/// The `broker` is handed in so the QR callback can publish
/// `InboundEvent::Qr` events while pairing, letting any UI subscribed
/// to `plugin.inbound.whatsapp` render the QR (ascii + PNG) without
/// touching stdout.
pub async fn connect_session(
    cfg: &WhatsappPluginConfig,
    broker: AnyBroker,
    pairing: SharedPairingState,
) -> Result<whatsapp_rs::Session> {
    let session_dir = ensure_session_dirs(cfg)?;
    check_daemon_collision(cfg)?;

    let broker_for_qr = Arc::new(broker);
    let qr_broker = broker_for_qr.clone();
    let qr_pairing = pairing.clone();
    let qr_inbound_topic = crate::bridge::inbound_topic_for(cfg.instance.as_deref());
    // Use `new_in_dir` so Signal state lands under our configured
    // `session_dir` without mutating `XDG_DATA_HOME` process-wide.
    // That's the change that unblocks multi-account in a single
    // process: every instance gets its own `FileStore` rooted at its
    // own `session_dir/.whatsapp-rs/...` with no shared global state.
    let client = whatsapp_rs::Client::new_in_dir(&session_dir)
        .context("wa-agent Client::new_in_dir failed")?
        .on_qr(move |qr_payload| {
            let payload = qr_payload.to_string();
            let broker = qr_broker.clone();
            let pairing = qr_pairing.clone();
            let inbound_topic = qr_inbound_topic.clone();
            tokio::spawn(async move {
                let ascii = whatsapp_rs::qr::ascii::render_qr(payload.as_bytes());
                eprintln!("\n{ascii}\nEscanea con WhatsApp → Dispositivos vinculados\n");
                let png_base64 = render_qr_png(&payload);
                let now = chrono::Utc::now().timestamp();
                let expires_at = now + 60;
                // Stash into pairing state for the HTTP API — this is
                // what `/whatsapp/pair/qr` serves.
                pairing
                    .set_qr(QrSnapshot {
                        ascii: ascii.clone(),
                        png_b64: png_base64.clone(),
                        expires_at,
                        captured_at: now,
                    })
                    .await;
                let ev = InboundEvent::Qr {
                    ascii,
                    png_base64,
                    expires_at,
                };
                let event = Event::new(&inbound_topic, SOURCE, ev.to_payload());
                if let Err(e) = broker.publish(&inbound_topic, event).await {
                    tracing::warn!(error = %e, "QR event publish failed");
                }
            });
        });
    let session = client.connect().await.context("wa-agent connect failed")?;
    Ok(session)
}

/// Render the pairing payload as a base64-encoded PNG for UIs. Fails
/// soft — returns an empty string when rendering fails so the ascii
/// QR is still usable.
fn render_qr_png(payload: &str) -> String {
    use base64::Engine;
    let code = match qrcode::QrCode::new(payload.as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "qrcode build failed");
            return String::new();
        }
    };
    let image = code
        .render::<image::Luma<u8>>()
        .min_dimensions(256, 256)
        .quiet_zone(true)
        .build();
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    if let Err(e) = image.write_to(
        &mut std::io::Cursor::new(&mut buf),
        image::ImageFormat::Png,
    ) {
        tracing::warn!(error = %e, "png encode failed");
        return String::new();
    }
    base64::engine::general_purpose::STANDARD.encode(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_qr_png_returns_valid_base64_png() {
        let out = render_qr_png("ref123,abc,def,ghi");
        assert!(!out.is_empty(), "png should render");
        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&out)
            .expect("valid base64");
        // PNG magic: 89 50 4E 47
        assert_eq!(&raw[..4], &[0x89, 0x50, 0x4E, 0x47]);
    }
}
