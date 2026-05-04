//! Session bootstrap — directory layout, daemon-collision guard, and
//! the actual `Client::new().connect()` call.
//!
//! Directory layout rationale lives in `docs/wa-agent-integration.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use nexo_broker::AnyBroker;
use nexo_config::WhatsappPluginConfig;

use crate::pairing::SharedPairingState;

/// Phase 82.10.p — outcome returned by `pair_with_callback`.
/// Today carries no fields beyond the success signal; future
/// channels may extend with `device_jid` etc. once wa-agent
/// surfaces them post-connect.
#[derive(Debug, Default, Clone)]
pub struct PairingOutcome {}

/// Phase 82.10.p — one-shot pairing helper that surfaces every
/// QR rotation through `on_qr` callback. Used by:
///
/// - **Setup CLI** (`agent setup whatsapp`): passes a closure
///   that renders the QR to stdout (terminal scan). Wrapped
///   by [`pair_once`] for backwards compat.
/// - **Admin RPC pairing trigger** (Phase 82.10.p Step 5):
///   passes a closure that pushes
///   `(qr_png_base64, qr_ascii, expires_at_ms)` into the
///   challenge store + the SSE notifier so the operator UI
///   can render the QR live.
///
/// Returns when wa-agent's `connect()` resolves (creds persisted
/// to `<session_dir>/.whatsapp-rs/`). Caller is responsible for
/// observing cancel signals + dropping the future when the
/// operator aborts (the trigger task in
/// `WhatsappPairingTrigger` does this via `tokio::select!`).
pub async fn pair_with_callback<F>(session_dir: &Path, on_qr: F) -> Result<PairingOutcome>
where
    F: FnMut(String, String, u64) + Send + 'static,
{
    tokio::fs::create_dir_all(session_dir)
        .await
        .with_context(|| format!("mkdir {}", session_dir.display()))?;

    // wa-agent's on_qr handler is `Fn(&str)`, not async.
    // Wrap our `FnMut` in a Mutex so the synchronous callback
    // can mutably re-enter it on every QR rotation.
    use std::sync::Mutex;
    let on_qr_arc = Arc::new(Mutex::new(on_qr));
    let on_qr_for_handler = on_qr_arc.clone();

    let client = whatsapp_rs::Client::new_in_dir(session_dir)
        .context("wa-agent Client::new_in_dir failed")?
        .on_qr(move |qr_payload| {
            let payload = qr_payload.to_string();
            let (ascii, png_b64) = match qrcode::QrCode::new(payload.as_bytes()) {
                Ok(code) => {
                    // ASCII: inverted colors + quiet zone so the
                    // QR scans on both dark + light terminal themes.
                    let ascii = code
                        .clone()
                        .render::<qrcode::render::unicode::Dense1x2>()
                        .dark_color(qrcode::render::unicode::Dense1x2::Light)
                        .light_color(qrcode::render::unicode::Dense1x2::Dark)
                        .quiet_zone(true)
                        .build();
                    let png_b64 = render_qr_png(&payload);
                    (ascii, png_b64)
                }
                Err(e) => {
                    tracing::warn!(error = %e, payload = %payload, "QR render failed");
                    (String::new(), String::new())
                }
            };
            // WhatsApp QRs rotate roughly every 20 s — we hand
            // out the conservative end so UIs that count down
            // never overshoot. The next on_qr callback will
            // refresh the bound.
            let expires_at_ms = epoch_ms_now().saturating_add(20_000);
            if let Ok(mut cb) = on_qr_for_handler.lock() {
                cb(png_b64, ascii, expires_at_ms);
            }
        });

    let _session = client
        .connect()
        .await
        .context("wa-agent connect failed — ¿QR expirado? ¿conectividad?")?;
    Ok(PairingOutcome::default())
}

/// One-shot pairing helper used by the setup wizard. Spawns a
/// `wa-agent` Client with a terminal QR renderer, blocks on
/// `connect()` until the user scans. First successful connect
/// persists to `<session_dir>/.whatsapp-rs/` so later boots of the
/// main agent resume silently.
///
/// Phase 82.10.p — now a thin wrapper over [`pair_with_callback`]
/// that prints the QR to stdout. New callers (admin RPC trigger,
/// future channel UIs) should use `pair_with_callback` directly
/// so they can route QR data wherever they need it.
pub async fn pair_once(session_dir: &Path) -> Result<()> {
    pair_with_callback(session_dir, |_png_b64, ascii, _expires| {
        println!();
        println!("{ascii}");
        println!("Esperando scan…");
    })
    .await
    .map(|_outcome| ())
}

/// Epoch-ms helper used to stamp QR rotation deadlines.
fn epoch_ms_now() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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
    PathBuf::from(&cfg.session_dir)
        .join(".whatsapp-rs")
        .join("daemon.json")
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

    // Pairing is a setup-time operation only. If no credentials exist
    // we refuse to boot instead of silently launching the QR flow —
    // that keeps runtime lean and forces the operator through the
    // wizard, where allowlist / token / device name are collected
    // consistently.
    let creds_path = session_dir.join(".whatsapp-rs").join("creds.json");
    if !creds_path.exists() {
        anyhow::bail!(
            "WhatsApp session not found at {}. \
             Pair via `agent setup` (the runtime no longer emits QRs). \
             After pairing, restart the agent.",
            creds_path.display()
        );
    }

    let broker_for_qr = Arc::new(broker);
    let qr_broker = broker_for_qr.clone();
    let qr_pairing = pairing.clone();
    let qr_inbound_topic = crate::bridge::inbound_topic_for(cfg.instance.as_deref());
    // Use `new_in_dir` so Signal state lands under our configured
    // `session_dir` without mutating `XDG_DATA_HOME` process-wide.
    // That's the change that unblocks multi-account in a single
    // process: every instance gets its own `FileStore` rooted at its
    // own `session_dir/.whatsapp-rs/...` with no shared global state.
    // Intentionally no `on_qr` handler: pairing lives in the setup
    // wizard. If creds go stale server-side (401 loop) the operator
    // must re-pair via setup — the runtime never surfaces a QR.
    let _ = (qr_broker, qr_pairing, qr_inbound_topic); // silence unused
    let client = whatsapp_rs::Client::new_in_dir(&session_dir)
        .context("wa-agent Client::new_in_dir failed")?;
    let session = client.connect().await.context("wa-agent connect failed")?;
    Ok(session)
}

/// Render the pairing payload as a base64-encoded PNG for UIs.
/// Fails soft — returns an empty string when rendering fails so
/// the ascii QR is still usable. Phase 82.10.p — promoted from
/// test-only to production: `pair_with_callback` invokes it on
/// every QR rotation so admin RPC pairing can stream PNG data
/// to the operator UI.
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
    if let Err(e) = image.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png) {
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
