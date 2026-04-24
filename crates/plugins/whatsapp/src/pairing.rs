//! Pairing state + snapshot API.
//!
//! The plugin publishes QR + lifecycle events to the broker (see
//! `session.rs` / `lifecycle.rs`), but an external UI — web page,
//! Telegram admin bot, CLI — also needs a pull endpoint that can be
//! polled without subscribing. This module holds that live cache.
//!
//! Layout mirrors OpenClaw's `startWebLoginWithQr` / `waitForWebLogin`
//! pair (`research/extensions/whatsapp/src/login-qr.ts`): a single
//! `current_qr` snapshot that gets replaced every time WhatsApp rotates
//! a pairing ref, plus a `connected` flag flipped by lifecycle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize)]
pub struct QrSnapshot {
    pub ascii: String,
    pub png_b64: String,
    pub expires_at: i64,
    /// Unix seconds. Helps UIs compute "expires in N seconds".
    pub captured_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub state: &'static str, // waiting_qr | connected | disconnected
    pub our_jid: Option<String>,
    pub last_reconnect_attempt: Option<u32>,
    pub has_qr: bool,
}

#[derive(Default)]
pub struct PairingState {
    current_qr: RwLock<Option<QrSnapshot>>,
    connected: AtomicBool,
    our_jid: RwLock<Option<String>>,
    last_reconnect_attempt: RwLock<Option<u32>>,
}

pub type SharedPairingState = Arc<PairingState>;

impl PairingState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn set_qr(&self, snap: QrSnapshot) {
        let mut w = self.current_qr.write().await;
        *w = Some(snap);
    }

    pub async fn clear_qr(&self) {
        let mut w = self.current_qr.write().await;
        *w = None;
    }

    pub async fn get_qr(&self) -> Option<QrSnapshot> {
        self.current_qr.read().await.clone()
    }

    pub fn set_connected(&self, yes: bool) {
        self.connected.store(yes, Ordering::SeqCst);
    }

    pub async fn set_our_jid(&self, jid: Option<String>) {
        let mut w = self.our_jid.write().await;
        *w = jid;
    }

    pub async fn set_reconnect_attempt(&self, attempt: Option<u32>) {
        let mut w = self.last_reconnect_attempt.write().await;
        *w = attempt;
    }

    pub async fn status(&self) -> StatusSnapshot {
        let our_jid = self.our_jid.read().await.clone();
        let has_qr = self.current_qr.read().await.is_some();
        let last_reconnect_attempt = *self.last_reconnect_attempt.read().await;
        let state = if self.connected.load(Ordering::SeqCst) {
            "connected"
        } else if has_qr {
            "waiting_qr"
        } else {
            "disconnected"
        };
        StatusSnapshot {
            state,
            our_jid,
            last_reconnect_attempt,
            has_qr,
        }
    }
}

/// Outcome of dispatching a `/whatsapp/...` route. The HTTP layer owns
/// how to write the response — this enum encodes *what* to write so
/// the routing rules stay pure + testable.
pub enum WhatsappRoute {
    /// Serve the static HTML pairing page. JS inside derives the
    /// per-instance QR/status URLs from `window.location.pathname`.
    Html,
    /// Return the JSON QR payload for this pairing state.
    Qr(SharedPairingState),
    /// Return the JSON status payload for this pairing state.
    Status(SharedPairingState),
    /// Pre-rendered JSON body (used by `/whatsapp/instances`).
    Json(String),
    /// No WhatsApp plugin is registered in this process.
    Disabled,
    /// Route named an instance that doesn't exist.
    NotFound,
}

/// Dispatch the `/whatsapp/...` subtree. `rest` is the path AFTER the
/// `/whatsapp/` prefix has been stripped by the caller. Returns `None`
/// when the path doesn't match any known route so the caller can fall
/// through to its own 404.
///
/// Paths handled:
///
/// * `pair`, `pair/qr`, `pair/status` → first instance (back-compat)
/// * `<id>/pair`, `<id>/pair/qr`, `<id>/pair/status` → named instance
/// * `instances` → JSON array of registered instance labels
pub fn dispatch_route(
    rest: &str,
    pairings: &std::collections::BTreeMap<String, SharedPairingState>,
) -> Option<WhatsappRoute> {
    if rest == "instances" {
        let names: Vec<&str> = pairings.keys().map(|s| s.as_str()).collect();
        let body = serde_json::to_string(&names).unwrap_or_else(|_| "[]".into());
        return Some(WhatsappRoute::Json(body));
    }
    if rest == "pair" {
        return if pairings.is_empty() {
            Some(WhatsappRoute::Disabled)
        } else {
            Some(WhatsappRoute::Html)
        };
    }
    if rest == "pair/qr" {
        return match pairings.values().next() {
            Some(p) => Some(WhatsappRoute::Qr(p.clone())),
            None => Some(WhatsappRoute::Disabled),
        };
    }
    if rest == "pair/status" {
        return match pairings.values().next() {
            Some(p) => Some(WhatsappRoute::Status(p.clone())),
            None => Some(WhatsappRoute::Disabled),
        };
    }
    let (instance, tail) = rest.split_once('/')?;
    match tail {
        "pair" => match pairings.get(instance) {
            Some(_) => Some(WhatsappRoute::Html),
            None => Some(WhatsappRoute::NotFound),
        },
        "pair/qr" => Some(
            pairings
                .get(instance)
                .map(|p| WhatsappRoute::Qr(p.clone()))
                .unwrap_or(WhatsappRoute::NotFound),
        ),
        "pair/status" => Some(
            pairings
                .get(instance)
                .map(|p| WhatsappRoute::Status(p.clone()))
                .unwrap_or(WhatsappRoute::NotFound),
        ),
        _ => None,
    }
}

/// Small HTML page you can open in a browser during pairing. Polls
/// `/whatsapp/pair/qr` and `/whatsapp/pair/status` every 2s and
/// swaps the QR image in place.
pub const PAIR_PAGE_HTML: &str = r#"<!DOCTYPE html>
<html lang="es">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Kate · WhatsApp pair</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 520px; margin: 40px auto; padding: 0 16px; color: #111; }
  h1 { font-size: 1.2rem; margin: 0 0 8px; }
  .card { border: 1px solid #ddd; border-radius: 12px; padding: 20px; text-align: center; }
  .state { font-size: 0.85rem; color: #666; margin: 8px 0 16px; }
  .state.ok { color: #0a7; }
  .state.err { color: #c22; }
  img.qr { width: 320px; height: 320px; image-rendering: pixelated; }
  .jid { font-family: ui-monospace, monospace; font-size: 0.9rem; color: #222; }
  .hint { font-size: 0.85rem; color: #666; margin-top: 16px; line-height: 1.5; }
  .empty { color: #888; padding: 80px 0; }
</style>
</head>
<body>
  <h1>Vincular WhatsApp</h1>
  <div class="state" id="state">…</div>
  <div class="card" id="card"><div class="empty">Esperando QR…</div></div>
  <div class="hint">
    1. Abre WhatsApp en tu teléfono.<br>
    2. Ajustes → Dispositivos vinculados → Vincular dispositivo.<br>
    3. Escanea este código.
  </div>
<script>
// Derive QR/status endpoints from the page URL so the same HTML works
// for both the legacy `/whatsapp/pair` (single-account) and per-instance
// `/whatsapp/<instance>/pair` multi-account paths.
const BASE = window.location.pathname.replace(/\/pair\/?$/, '/pair');
async function tick() {
  try {
    const s = await fetch(BASE + '/status').then(r => r.json());
    const stateEl = document.getElementById('state');
    const card = document.getElementById('card');
    if (s.state === 'connected') {
      stateEl.textContent = `✓ Conectado · ${s.our_jid || ''}`;
      stateEl.className = 'state ok';
      card.innerHTML = `<div class="jid">${s.our_jid || 'ok'}</div>`;
      return;
    }
    if (s.state === 'disconnected' && !s.has_qr) {
      stateEl.textContent = '· desconectado · esperando socket';
      stateEl.className = 'state err';
    } else {
      stateEl.textContent = '· esperando que escanees ·';
      stateEl.className = 'state';
    }
    const q = await fetch(BASE + '/qr').then(r => r.json()).catch(() => null);
    if (q && q.png_b64) {
      card.innerHTML = `<img class="qr" src="data:image/png;base64,${q.png_b64}" alt="QR">`;
    } else if (!card.querySelector('img')) {
      card.innerHTML = `<div class="empty">Esperando QR…</div>`;
    }
  } catch (e) {
    document.getElementById('state').textContent = `error: ${e}`;
  } finally {
    setTimeout(tick, 2000);
  }
}
tick();
</script>
</body>
</html>
"#;

#[cfg(test)]
mod route_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn empty() -> BTreeMap<String, SharedPairingState> {
        BTreeMap::new()
    }

    fn with(ids: &[&str]) -> BTreeMap<String, SharedPairingState> {
        let mut m = BTreeMap::new();
        for id in ids {
            m.insert((*id).to_string(), PairingState::new());
        }
        m
    }

    #[test]
    fn instances_returns_sorted_json_array() {
        let m = with(&["support", "biz", "vip"]);
        let r = dispatch_route("instances", &m).expect("handled");
        match r {
            WhatsappRoute::Json(body) => {
                // BTreeMap → keys in sorted order.
                assert_eq!(body, r#"["biz","support","vip"]"#);
            }
            _ => panic!("expected Json"),
        }
    }

    #[test]
    fn legacy_pair_routes_hit_first_instance() {
        let m = with(&["biz", "support"]);
        match dispatch_route("pair", &m).unwrap() {
            WhatsappRoute::Html => {}
            _ => panic!("expected Html"),
        }
        assert!(matches!(
            dispatch_route("pair/qr", &m).unwrap(),
            WhatsappRoute::Qr(_)
        ));
        assert!(matches!(
            dispatch_route("pair/status", &m).unwrap(),
            WhatsappRoute::Status(_)
        ));
    }

    #[test]
    fn empty_map_returns_disabled_on_legacy_routes() {
        let m = empty();
        assert!(matches!(
            dispatch_route("pair", &m).unwrap(),
            WhatsappRoute::Disabled
        ));
        assert!(matches!(
            dispatch_route("pair/qr", &m).unwrap(),
            WhatsappRoute::Disabled
        ));
        assert!(matches!(
            dispatch_route("pair/status", &m).unwrap(),
            WhatsappRoute::Disabled
        ));
    }

    #[test]
    fn per_instance_routes_match_or_404() {
        let m = with(&["biz", "support"]);
        assert!(matches!(
            dispatch_route("biz/pair", &m).unwrap(),
            WhatsappRoute::Html
        ));
        assert!(matches!(
            dispatch_route("biz/pair/qr", &m).unwrap(),
            WhatsappRoute::Qr(_)
        ));
        assert!(matches!(
            dispatch_route("biz/pair/status", &m).unwrap(),
            WhatsappRoute::Status(_)
        ));
        // Unknown instance.
        assert!(matches!(
            dispatch_route("nonexistent/pair", &m).unwrap(),
            WhatsappRoute::NotFound
        ));
        assert!(matches!(
            dispatch_route("nonexistent/pair/qr", &m).unwrap(),
            WhatsappRoute::NotFound
        ));
    }

    #[test]
    fn unrelated_path_returns_none_for_fallthrough() {
        let m = with(&["biz"]);
        assert!(dispatch_route("something-else", &m).is_none());
        assert!(dispatch_route("biz/other", &m).is_none());
    }
}
