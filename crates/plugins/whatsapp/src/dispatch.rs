//! Outbound dispatcher — consumes `plugin.outbound.whatsapp` and either
//! resolves a waiting bridge oneshot (reactive reply) or sends directly
//! via the wa-agent `Session` (proactive path — heartbeat reminders,
//! A2A delegation outputs, etc.).
//!
//! Dispatch rules:
//!
//! 1. If `event.session_id` is `Some(sid)` AND `pending` holds a sender
//!    for `sid` → deliver the reply text through the oneshot so the
//!    bridge handler renders a native `Response::Text` (wa-agent keeps
//!    the typing indicator coherent with the in-flight message).
//! 2. Otherwise → call `Session::send_text(to, text)` directly. This is
//!    still outbox-backed so reliability is the same.
//!
//! Commands we understand:
//!   - `SendMessage { to, text }`                  → send_text
//!   - `SendMedia { to, url, caption }`            → deferred to 6.5
//!   - `Custom { "reply",  { to, msg_id, text } }` → send_reply
//!   - `Custom { "react",  { to, msg_id, emoji } }` → send_reaction

use std::sync::Arc;

use anyhow::Result;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::plugin::{AskReplyIndex, PendingMap};

pub const TOPIC_OUTBOUND: &str = "plugin.outbound.whatsapp";

/// Outbound URL downloads are capped so a rogue caller can't OOM the
/// process. 64 MiB matches WhatsApp's server-side media ceiling.
const MAX_DOWNLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Minimal shape we care about on the outbound payload. Core's
/// `llm_behavior` publishes `{to, text, session_id}`; the `Custom`
/// variants add `name` + `payload`. We accept both with serde defaults.
#[derive(Debug, Deserialize)]
pub(crate) struct OutboundPayload {
    #[serde(default)]
    pub(crate) to: Option<String>,
    #[serde(default)]
    pub(crate) text: Option<String>,
    /// Discriminator for `Custom` commands. Present only when core /
    /// tools publish `Command::Custom`; otherwise defaults to `"text"`.
    #[serde(default = "default_kind")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) msg_id: Option<String>,
    #[serde(default)]
    pub(crate) emoji: Option<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) caption: Option<String>,
    #[serde(default)]
    pub(crate) file_name: Option<String>,
    /// Base64-encoded audio bytes. Set when `kind == "voice_note"`
    /// (reply transform pipeline produced a voice reply).
    #[serde(default)]
    pub(crate) audio_bytes_b64: Option<String>,
    /// MIME type accompanying `audio_bytes_b64` (`audio/mpeg`,
    /// `audio/ogg`, …).
    #[serde(default)]
    pub(crate) mimetype: Option<String>,
}

pub(crate) fn default_kind() -> String {
    "text".to_string()
}

/// Spawn the dispatcher loop. Returns a `JoinHandle` so callers can
/// await it on shutdown — otherwise `cancel.cancel()` just signals the
/// task and the process can exit with work still mid-flight.
pub fn spawn(
    broker: AnyBroker,
    session: Arc<whatsapp_rs::Session>,
    pending: PendingMap,
    ask_reply_index: AskReplyIndex,
    cancel: CancellationToken,
    outbound_topic: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut sub = match broker.subscribe(&outbound_topic).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, outbound_topic, "outbound subscribe failed — dispatcher exiting");
                return;
            }
        };
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("whatsapp outbound dispatcher cancelled");
                    break;
                }
                ev = sub.next() => {
                    let Some(ev) = ev else { break };
                    if let Err(e) = dispatch_event(ev, &session, &pending, &ask_reply_index).await {
                        warn!(error = %e, "outbound dispatch failed");
                    }
                }
            }
        }
    })
}

async fn dispatch_event(
    ev: Event,
    session: &whatsapp_rs::Session,
    pending: &PendingMap,
    ask_reply_index: &AskReplyIndex,
) -> Result<()> {
    tracing::info!(
        session_id = ?ev.session_id,
        topic = %ev.topic,
        payload_len = ev.payload.to_string().len(),
        "DISPATCH_EVENT received"
    );
    let payload: OutboundPayload =
        serde_json::from_value(ev.payload.clone()).unwrap_or(OutboundPayload {
            to: None,
            text: None,
            kind: default_kind(),
            msg_id: None,
            emoji: None,
            url: None,
            caption: None,
            file_name: None,
            audio_bytes_b64: None,
            mimetype: None,
        });

    // Reactive path: resolve the oneshot and stop — the bridge handler
    // will produce the Response, wa-agent renders it with native typing.
    if payload.kind == "text" {
        if let (Some(sid), Some(text)) = (ev.session_id, payload.text.as_ref()) {
            if let Some((_, tx)) = pending.remove(&sid) {
                if tx.send(text.clone()).is_ok() {
                    return Ok(());
                }
                // Receiver already dropped (timeout) — fall through to
                // a direct send so the user still gets the message.
            }
        }
    } else if let Some(sid) = ev.session_id {
        // Non-text kinds (voice_note, image, …) bypass wa-agent's
        // reactive Response::Text path and send proactively below.
        // Drop the pending oneshot so `bridge_step` returns None
        // immediately and the bridge handler stops typing — without
        // this the typing indicator hangs until `response_timeout_ms`
        // elapses while we've already sent the audio.
        if let Some((_, tx)) = pending.remove(&sid) {
            drop(tx);
        }
    }

    // If we reach here with a session_id but no `to`, it means the
    // reactive oneshot already fired (or timed out) for this session.
    // wa-agent will have rendered the reply via `run_agent_with`'s
    // Response already — sending a duplicate direct message would
    // double-post. Drop silently.
    if payload.to.as_deref().unwrap_or("").is_empty() && ev.session_id.is_some() {
        debug!(
            session_id = ?ev.session_id,
            "outbound dispatch skipped: reactive path already handled"
        );
        return Ok(());
    }

    // Proactive or reactive-after-timeout path: send directly. wa-agent
    // buffers through its outbox so offline sends still land.
    match payload.kind.as_str() {
        "text" => {
            let to = payload.to.ok_or_else(|| anyhow::anyhow!("missing `to`"))?;
            let text = payload.text.unwrap_or_default();
            if text.is_empty() {
                debug!("dropping empty outbound text");
                return Ok(());
            }
            tracing::info!(
                to = %to,
                text_preview = %text.chars().take(60).collect::<String>(),
                session_id = ?ev.session_id,
                "DISPATCH_EVENT proactive send_text"
            );
            let sent_id = session
                .send_text(&to, &text)
                .await
                .map_err(|e| anyhow::anyhow!("send_text: {e}"))?;
            if let Some(qid) = extract_question_id_marker(&text) {
                ask_reply_index.insert(sent_id, qid);
            }
        }
        "reply" => {
            let to = payload.to.ok_or_else(|| anyhow::anyhow!("missing `to`"))?;
            let msg_id = payload
                .msg_id
                .ok_or_else(|| anyhow::anyhow!("reply missing `msg_id`"))?;
            let text = payload.text.unwrap_or_default();
            session
                .send_reply(&to, &msg_id, &text)
                .await
                .map_err(|e| anyhow::anyhow!("send_reply: {e}"))?;
        }
        "react" => {
            let to = payload.to.ok_or_else(|| anyhow::anyhow!("missing `to`"))?;
            let msg_id = payload
                .msg_id
                .ok_or_else(|| anyhow::anyhow!("react missing `msg_id`"))?;
            let emoji = payload
                .emoji
                .ok_or_else(|| anyhow::anyhow!("react missing `emoji`"))?;
            session
                .send_reaction(&to, &msg_id, &emoji)
                .await
                .map_err(|e| anyhow::anyhow!("send_reaction: {e}"))?;
        }
        "media" => {
            let to = payload
                .to
                .ok_or_else(|| anyhow::anyhow!("media missing `to`"))?;
            let url = payload
                .url
                .ok_or_else(|| anyhow::anyhow!("media missing `url`"))?;
            // Cap how long we wait on the media origin. Without the
            // timeout a slow or deliberately-hanging URL would pin the
            // whole dispatcher task — blocking every other outbound
            // message for this account.
            const MEDIA_DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
            let (bytes, mime) = tokio::time::timeout(
                MEDIA_DOWNLOAD_TIMEOUT,
                crate::media::download_from_url(&url, MAX_DOWNLOAD_BYTES),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "media download timed out after {}s for {url}",
                    MEDIA_DOWNLOAD_TIMEOUT.as_secs()
                )
            })??;
            crate::media::send_media_auto(
                session,
                &to,
                &bytes,
                &mime,
                payload.caption.as_deref(),
                payload.file_name.as_deref(),
            )
            .await?;
        }
        "voice_note" => {
            use base64::{engine::general_purpose::STANDARD as B64, Engine};
            let to = payload
                .to
                .clone()
                .ok_or_else(|| anyhow::anyhow!("voice_note missing `to`"))?;
            let b64 = payload
                .audio_bytes_b64
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("voice_note missing `audio_bytes_b64`"))?;
            let mimetype = payload
                .mimetype
                .clone()
                .unwrap_or_else(|| "audio/mpeg".to_string());
            let audio = B64
                .decode(b64)
                .map_err(|e| anyhow::anyhow!("voice_note audio_bytes_b64 decode: {e}"))?;
            if audio.is_empty() {
                return Err(anyhow::anyhow!(
                    "voice_note audio_bytes_b64 decoded to 0 bytes"
                ));
            }
            tracing::info!(
                to = %to,
                bytes = audio.len(),
                mimetype = %mimetype,
                session_id = ?ev.session_id,
                "DISPATCH_EVENT proactive send_voice_note"
            );
            // Switch the peer phone's indicator to "grabando audio…"
            // immediately before the upload + send. Best-effort —
            // a presence-emit failure must not abort the voice
            // note delivery (the audio is the actual payload; the
            // indicator is cosmetic UX). Same shape as the
            // wa-agent `apply_response` path so both inbound-driven
            // and proactive sends paint identically on the peer.
            if let Err(e) = session
                .send_chat_presence(
                    &to,
                    whatsapp_rs::ChatPresenceState::Composing,
                    Some(whatsapp_rs::ChatPresenceMedia::Audio),
                )
                .await
            {
                tracing::warn!(
                    to = %to,
                    error = %e,
                    "send_chat_presence(Composing, Audio) failed; continuing voice_note send"
                );
            }
            let send_result = session
                .send_voice_note(&to, &audio, &mimetype)
                .await
                .map_err(|e| anyhow::anyhow!("send_voice_note: {e}"));
            // Always emit the paused stanza, regardless of the
            // send outcome. If the send succeeded we don't want
            // the peer phone stuck on "grabando…" until WA's TTL
            // (~10s) reaps the indicator; if it failed, paused is
            // still the right state to leave the chat in.
            if let Err(e) = session
                .send_chat_presence(&to, whatsapp_rs::ChatPresenceState::Paused, None)
                .await
            {
                tracing::warn!(
                    to = %to,
                    error = %e,
                    "send_chat_presence(Paused) failed after voice_note send"
                );
            }
            send_result?;
        }
        other => {
            return Err(anyhow::anyhow!("unknown outbound kind `{other}`"));
        }
    }
    Ok(())
}

fn extract_question_id_marker(text: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("[question_id]") {
            let id = rest.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

// ── Sessionless oneshot resolver (exposed for integration tests) ─────────────

#[doc(hidden)]
pub mod __test {
    use super::{default_kind, OutboundPayload};
    use crate::plugin::PendingMap;
    use nexo_broker::Event;

    /// Resolve the reactive oneshot without calling `Session`. Exposes
    /// the session-id → oneshot routing logic so tests can assert it
    /// without a real `wa-agent` connection.
    pub fn try_resolve_reactive(ev: &Event, pending: &PendingMap) -> ResolveOutcome {
        let payload: OutboundPayload =
            serde_json::from_value(ev.payload.clone()).unwrap_or(OutboundPayload {
                to: None,
                text: None,
                kind: default_kind(),
                msg_id: None,
                emoji: None,
                url: None,
                caption: None,
                file_name: None,
                audio_bytes_b64: None,
                mimetype: None,
            });
        if payload.kind != "text" {
            return ResolveOutcome::NotReactive;
        }
        let (Some(sid), Some(text)) = (ev.session_id, payload.text) else {
            return ResolveOutcome::NotReactive;
        };
        let Some((_, tx)) = pending.remove(&sid) else {
            return ResolveOutcome::NoPending;
        };
        match tx.send(text) {
            Ok(()) => ResolveOutcome::Delivered,
            Err(_) => ResolveOutcome::ReceiverGone,
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum ResolveOutcome {
        Delivered,
        ReceiverGone,
        NoPending,
        NotReactive,
    }
}

#[doc(hidden)]
pub use __test::try_resolve_reactive as __test_try_resolve;
