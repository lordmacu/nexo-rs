//! Lifecycle — translates `wa-agent` `MessageEvent` lifecycle variants
//! into broker-visible `InboundEvent`s (Connected / Disconnected /
//! Reconnecting) and exposes a lightweight [`PluginHealth`] snapshot.
//!
//! The agent loop (`run_agent_with`) only surfaces `NewMessage` to our
//! handler. Everything else — connection state changes, reconnection
//! progress — still travels through the `broadcast::Receiver` the crate
//! exposes via [`whatsapp_rs::Session::events`], so we subscribe a
//! second receiver here and forward the variants we care about.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::bridge::SOURCE;
use crate::events::InboundEvent;

/// What the plugin knows about its own state. Cheap to compute, snapped
/// on demand — not a live-updating metric. Returned by
/// [`crate::WhatsappPlugin::health`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHealth {
    pub connected: bool,
    pub our_jid: Option<String>,
    pub outbox_pending: usize,
    /// Seconds since the last lifecycle event was observed. `None` when
    /// no events have been seen yet (pre-connect window).
    pub last_event_age_secs: Option<u64>,
    /// Last `Reconnecting { attempt, .. }` seen — useful for UIs.
    pub last_reconnect_attempt: Option<u32>,
}

/// Internal, thread-safe state updated by the forwarder. Cloned into
/// the plugin via `Arc<Mutex<_>>` so `health()` can read without racing
/// with the event task.
#[derive(Debug, Default)]
pub struct LifecycleState {
    pub connected: bool,
    pub our_jid: Option<String>,
    pub last_event: Option<Instant>,
    pub last_reconnect_attempt: Option<u32>,
}

pub type SharedLifecycle = Arc<Mutex<LifecycleState>>;

/// Spawn the forwarder. Owns a `broadcast::Receiver` on the session,
/// translates interesting variants, publishes them to the broker, and
/// keeps `state` current for `health()`.
///
/// Phase 82.10.r — `event_emitter` + `instance_label` are threaded
/// in so wa-agent's `MessageEvent::Typing` variant surfaces as
/// `AgentEventKind::PeerTyping` via the boot firehose. Standalone
/// embeds without admin-bootstrap pass `None` and the typing emit
/// silently skips.
pub fn spawn(
    broker: AnyBroker,
    session: Arc<whatsapp_rs::Session>,
    state: SharedLifecycle,
    pairing: crate::pairing::SharedPairingState,
    cancel: CancellationToken,
    inbound_topic: String,
    event_emitter: Option<Arc<dyn nexo_core::agent::agent_events::AgentEventEmitter>>,
    instance_label: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = session.events();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("whatsapp lifecycle forwarder cancelled");
                    break;
                }
                ev = rx.recv() => {
                    let ev = match ev {
                        Ok(e) => e,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(lagged = n, "lifecycle receiver lagged — continuing");
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if let Err(e) = forward(
                        &broker,
                        &session,
                        &state,
                        &pairing,
                        &inbound_topic,
                        event_emitter.as_ref(),
                        &instance_label,
                        ev,
                    ).await {
                        warn!(error = %e, "lifecycle forward failed");
                    }
                }
            }
        }
    })
}

async fn forward(
    broker: &AnyBroker,
    session: &whatsapp_rs::Session,
    state: &SharedLifecycle,
    pairing: &crate::pairing::SharedPairingState,
    inbound_topic: &str,
    event_emitter: Option<&Arc<dyn nexo_core::agent::agent_events::AgentEventEmitter>>,
    instance_label: &str,
    ev: whatsapp_rs::MessageEvent,
) -> Result<()> {
    // Phase 82.10.r — handle Typing presence outside the lifecycle
    // state-machine block since it doesn't mutate connect/disconnect
    // state and skips the broker `InboundEvent` emit.
    // Passive monitor — log every NewMessage that hits the session,
    // including chats the agent's ACL would otherwise filter out
    // (Meta AI replies, third-party bots, archived contacts). Pure
    // observation: no broker emit, no agent routing. Useful for
    // debugging "did wa-agent see this stanza at all?".
    if let whatsapp_rs::MessageEvent::NewMessage { msg } = &ev {
        let from = msg
            .key
            .participant
            .clone()
            .unwrap_or_else(|| msg.key.remote_jid.clone());
        let text_preview = whatsapp_rs::agent::extract_text(msg.message.as_ref())
            .map(|t| t.chars().take(300).collect::<String>())
            .unwrap_or_default();
        let content_kind = msg.message.as_ref().map(|c| {
            use whatsapp_rs::messages::MessageContent;
            match c {
                MessageContent::Text { .. } => "text",
                MessageContent::Image { .. } => "image",
                MessageContent::Video { .. } => "video",
                MessageContent::Audio { .. } => "audio",
                MessageContent::Document { .. } => "document",
                MessageContent::Sticker { .. } => "sticker",
                MessageContent::Location { .. } => "location",
                MessageContent::Contact { .. } => "contact",
                MessageContent::Reaction { .. } => "reaction",
                MessageContent::Reply { .. } => "reply",
                MessageContent::Poll { .. } => "poll",
                MessageContent::LinkPreview { .. } => "link_preview",
                MessageContent::Buttons { .. } => "buttons",
                MessageContent::List { .. } => "list",
            }
        });
        info!(
            target: "wa::monitor",
            from = %from,
            chat = %msg.key.remote_jid,
            from_me = msg.key.from_me,
            msg_id = %msg.key.id,
            push_name = %msg.push_name.as_deref().unwrap_or(""),
            content = content_kind.unwrap_or("none"),
            text_len = text_preview.len(),
            text = %text_preview,
            "wa-monitor: inbound message"
        );
    }
    // Bot replies — Meta AI etc. — surface to the firehose so
    // microapps can render them in a dedicated bubble UI without
    // routing through the agent dispatcher (which would otherwise
    // try to reply to the bot and create a fan-out loop).
    if let whatsapp_rs::MessageEvent::BotMessage {
        bot_jid,
        msg_id,
        target_id,
        edit,
        text,
    } = &ev
    {
        if let Some(emitter) = event_emitter {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let evt = nexo_tool_meta::admin::agent_events::AgentEventKind::WhatsappBotMessage {
                instance: instance_label.to_string(),
                bot_jid: bot_jid.clone(),
                msg_id: msg_id.clone(),
                target_id: target_id.clone(),
                edit: edit.clone(),
                text: text.clone(),
                at_ms: now_ms,
            };
            emitter.emit(evt).await;
        }
        return Ok(());
    }
    if let whatsapp_rs::MessageEvent::Typing { jid, composing } = &ev {
        if let Some(emitter) = event_emitter {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let evt = nexo_tool_meta::admin::agent_events::AgentEventKind::PeerTyping {
                channel: "whatsapp".to_string(),
                account_id: instance_label.to_string(),
                sender_id: jid.clone(),
                composing: *composing,
                at_ms: now_ms,
                agent_id: None,
                tenant_id: None,
            };
            emitter.emit(evt).await;
        }
        return Ok(());
    }
    let mut out: Option<InboundEvent> = None;
    {
        let mut s = state.lock().await;
        s.last_event = Some(Instant::now());
        match &ev {
            whatsapp_rs::MessageEvent::Connected => {
                s.connected = true;
                s.our_jid = Some(session.our_jid.clone());
                pairing.set_connected(true);
                pairing.set_our_jid(Some(session.our_jid.clone())).await;
                // Once paired the QR is stale — drop it so the UI
                // renders the "connected" state instead of a dead code.
                pairing.clear_qr().await;
                out = Some(InboundEvent::Connected {
                    our_jid: session.our_jid.clone(),
                });
            }
            whatsapp_rs::MessageEvent::Disconnected { reason, .. } => {
                s.connected = false;
                pairing.set_connected(false);
                out = Some(InboundEvent::Disconnected {
                    reason: reason.clone(),
                });
            }
            whatsapp_rs::MessageEvent::Reconnecting { attempt, .. } => {
                s.last_reconnect_attempt = Some(*attempt);
                pairing.set_reconnect_attempt(Some(*attempt)).await;
                out = Some(InboundEvent::Reconnecting { attempt: *attempt });
            }
            _ => {}
        }
    }
    if let Some(inbound) = out {
        let event = Event::new(inbound_topic, SOURCE, inbound.to_payload());
        broker.publish(inbound_topic, event).await.ok();
    }
    Ok(())
}

/// Snapshot the current health. Cheap — no IO.
pub async fn snapshot(
    state: &SharedLifecycle,
    session: &Option<Arc<whatsapp_rs::Session>>,
) -> PluginHealth {
    let s = state.lock().await;
    let outbox_pending = if let Some(sess) = session {
        sess.outbox_pending_count().await
    } else {
        0
    };
    PluginHealth {
        connected: s.connected,
        our_jid: s.our_jid.clone(),
        outbox_pending,
        last_event_age_secs: s.last_event.map(|t| t.elapsed().as_secs()),
        last_reconnect_attempt: s.last_reconnect_attempt,
    }
}
