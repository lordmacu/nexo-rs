//! Inbound bridge — translates a wa-agent `AgentCtx` into a broker event
//! and blocks on the LLM reply so `run_agent_with` can render the
//! response with its native typing heartbeat.
//!
//! We publish on `plugin.inbound.whatsapp` with `Event.session_id` set
//! to a deterministic UUIDv5 derived from the remote JID. Core's agent
//! runtime already debounces by `session_id` and replies on
//! `plugin.outbound.whatsapp` carrying the same `session_id`, which
//! the outbound dispatcher (Phase 6.4) routes back through the
//! [`PendingMap`].

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_config::WhatsappPluginConfig;
use tokio::sync::oneshot;
use uuid::Uuid;
use whatsapp_rs::agent::{AgentCtx, Response};

use crate::events::InboundEvent;
use crate::plugin::AskReplyIndex;
use crate::plugin::PendingMap;
use crate::session_id::session_id_for_jid;
use whatsapp_rs::messages::MessageContent;

pub const TOPIC_INBOUND: &str = "plugin.inbound.whatsapp";
pub const SOURCE: &str = "plugin.whatsapp";

/// Build the inbound topic for this account. When `instance` is set,
/// publishes land on `plugin.inbound.whatsapp.<instance>` so an agent
/// binding (`inbound_bindings: [{plugin: whatsapp, instance: X}]`) can
/// pin itself to one account without cross-talk.
pub fn inbound_topic_for(instance: Option<&str>) -> String {
    match instance {
        Some(inst) if !inst.is_empty() => format!("{}.{}", TOPIC_INBOUND, inst),
        _ => TOPIC_INBOUND.to_string(),
    }
}

/// Build the outbound topic for this account. Mirrors `inbound_topic_for`
/// — the dispatcher subscribes to its own
/// `plugin.outbound.whatsapp.<instance>` so agents can route replies to
/// a specific account.
pub fn outbound_topic_for(instance: Option<&str>) -> String {
    match instance {
        Some(inst) if !inst.is_empty() => {
            format!("{}.{}", crate::dispatch::TOPIC_OUTBOUND, inst)
        }
        _ => crate::dispatch::TOPIC_OUTBOUND.to_string(),
    }
}

/// Encapsulates one reactive inbound → broker → oneshot cycle. Kept as
/// its own function so unit tests can drive it with a `LocalBroker`
/// without spinning up a real `wa-agent` session.
///
/// Returns the reply text the outbound dispatcher delivered, or `None`
/// on timeout (caller decides what to render to the user).
pub async fn bridge_step(
    broker: &AnyBroker,
    pending: &PendingMap,
    cfg: &WhatsappPluginConfig,
    session_id: Uuid,
    event_payload: InboundEvent,
) -> Option<String> {
    let inbound_topic = inbound_topic_for(cfg.instance.as_deref());
    let (tx, rx) = oneshot::channel::<String>();
    // Last inbound wins: if a previous message on this session is still
    // awaiting, its handler sees a dropped sender and falls through to
    // the timeout path. This matches the core runtime's debounce model.
    pending.insert(session_id, tx);

    let mut event = Event::new(&inbound_topic, SOURCE, event_payload.to_payload());
    event.session_id = Some(session_id);
    if let Err(e) = broker.publish(&inbound_topic, event).await {
        tracing::warn!(%session_id, error = %e, "inbound publish failed");
        pending.remove(&session_id);
        return None;
    }

    match tokio::time::timeout(Duration::from_millis(cfg.bridge.response_timeout_ms), rx).await {
        Ok(Ok(text)) => Some(text),
        Ok(Err(_cancelled)) => {
            // Sender was dropped — a newer inbound on the same session
            // won the slot. Silent Noop is the right call here.
            tracing::debug!(%session_id, "bridge sender superseded");
            None
        }
        Err(_elapsed) => {
            pending.remove(&session_id);
            let ev = InboundEvent::BridgeTimeout { session_id };
            let mut out = Event::new(&inbound_topic, SOURCE, ev.to_payload());
            out.session_id = Some(session_id);
            let _ = broker.publish(&inbound_topic, out).await;
            None
        }
    }
}

/// Build the closure passed to `Session::run_agent_with`. Captures
/// cheap clones of broker / pending / cfg / session so each invocation
/// stays self-contained.
///
/// When the incoming `AgentCtx` carries media, a sibling task downloads
/// the file to `cfg.media_dir` and publishes an
/// `InboundEvent::MediaReceived` — the handler itself never blocks on
/// the download, so the typing heartbeat stays responsive and other
/// chats are not held up by slow media fetches.
pub fn build_handler(
    broker: AnyBroker,
    pending: PendingMap,
    ask_reply_index: AskReplyIndex,
    cfg: Arc<WhatsappPluginConfig>,
    session: Arc<whatsapp_rs::Session>,
) -> impl Fn(AgentCtx) -> futures::future::BoxFuture<'static, Response> + Send + Sync + 'static {
    move |ctx: AgentCtx| {
        let broker = broker.clone();
        let pending = pending.clone();
        let ask_reply_index = ask_reply_index.clone();
        let cfg = cfg.clone();
        let session = session.clone();
        Box::pin(async move {
            if cfg.behavior.ignore_groups && ctx.msg.key.remote_jid.ends_with("@g.us") {
                return Response::Noop;
            }

            // Kick off media download in the background — we don't want
            // to block the handler (and wa-agent's typing indicator).
            if let Some(content) = ctx.msg.message.as_ref() {
                if crate::media::variant_of_content(content).is_some() {
                    let broker_m = broker.clone();
                    let cfg_m = cfg.clone();
                    let session_m = session.clone();
                    let msg_m = ctx.msg.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            crate::media::download_inbound(&session_m, &broker_m, &cfg_m, &msg_m)
                                .await
                        {
                            tracing::warn!(error = %e, "inbound media download failed");
                        }
                    });
                }
            }

            let session_id = session_id_for_jid(ctx.jid());
            let reply_to_id = extract_reply_to_id(ctx.msg.message.as_ref());
            let ask_qid = reply_to_id
                .as_deref()
                .and_then(|rid| ask_reply_index.get(rid).map(|v| v.value().clone()))
                .or_else(|| ctx.text.as_deref().and_then(extract_question_id_marker));
            let payload = InboundEvent::Message {
                from: ctx.sender().to_string(),
                chat: ctx.jid().to_string(),
                text: ctx.text.clone(),
                reply_to: reply_to_id,
                reply_to_question_id: ask_qid.clone(),
                ask_question_id: ask_qid,
                is_group: ctx.msg.key.remote_jid.ends_with("@g.us"),
                timestamp: chrono::Utc::now().timestamp(),
                msg_id: ctx.msg.key.id.clone(),
            };

            match bridge_step(&broker, &pending, &cfg, session_id, payload).await {
                Some(text) => Response::Text(text),
                None => match cfg.bridge.on_timeout.as_str() {
                    "apology_text" => Response::Text(cfg.bridge.apology_text.clone()),
                    _ => Response::Noop,
                },
            }
        })
    }
}

/// Thin helper used by `start()` to convert plugin boot errors into
/// `anyhow` without pulling wa-agent types into the plugin.rs module.
pub fn forward_err<T: std::fmt::Display>(label: &str, e: T) -> anyhow::Error {
    anyhow::anyhow!("{label}: {e}")
}

/// Build the `whatsapp_rs::agent::Acl` from plugin config + env var.
pub fn build_acl(cfg: &WhatsappPluginConfig) -> whatsapp_rs::agent::Acl {
    let mut acl = if cfg.acl.from_env.is_empty() {
        whatsapp_rs::agent::Acl::open()
    } else {
        whatsapp_rs::agent::Acl::from_env(&cfg.acl.from_env)
    };
    for jid in &cfg.acl.allow_list {
        acl = acl.allow(jid);
    }
    acl
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

fn extract_reply_to_id(content: Option<&MessageContent>) -> Option<String> {
    match content {
        Some(MessageContent::Reply { reply_to_id, .. }) if !reply_to_id.is_empty() => {
            Some(reply_to_id.clone())
        }
        _ => None,
    }
}

#[doc(hidden)]
pub fn _unused_result<T>() -> Result<T>
where
    T: Default,
{
    Ok(T::default())
}

#[cfg(test)]
mod tests {
    use super::{extract_question_id_marker, extract_reply_to_id};
    use whatsapp_rs::messages::MessageContent;

    #[test]
    fn extract_question_id_marker_ok() {
        let text = "[question] x\n[question_id] q-123\n";
        assert_eq!(extract_question_id_marker(text).as_deref(), Some("q-123"));
    }

    #[test]
    fn extract_reply_to_id_from_reply_variant() {
        let c = MessageContent::Reply {
            reply_to_id: "wamid-1".to_string(),
            text: "answer".to_string(),
        };
        assert_eq!(extract_reply_to_id(Some(&c)).as_deref(), Some("wamid-1"));
    }
}
