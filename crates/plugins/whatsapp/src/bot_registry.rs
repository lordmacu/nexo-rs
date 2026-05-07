//! Global registry mapping a WhatsApp **plugin instance label** to
//! the live `wa_agent::Session` for that account.
//!
//! Each `WhatsappPlugin` registers its `Session` here at boot under
//! its `cfg.instance` label (`"default"` when absent). The
//! dispatcher's `WaBotHandle` impl looks the session up by that
//! label so admin RPC routes
//! (`nexo/admin/whatsapp/bot/{list,send}`) can drive the right
//! account in multi-account setups.
//!
//! v1 keys on **instance label**, not agent id, because the plugin
//! today doesn't carry the agent binding directly. Microapp callers
//! pass the instance label as the `agent_id` field for now —
//! resolving real agent ids → instance labels via `agents.yaml` is
//! an additive follow-up that doesn't change the wire shape.

use std::sync::{Arc, OnceLock};

use anyhow::anyhow;
use async_trait::async_trait;
use dashmap::DashMap;

use nexo_core::agent::admin_rpc::wa_bot::{WaBotHandle, WaBotResult};
use nexo_tool_meta::admin::wa_bot::BotInfo;

/// `instance_label` → live `Session`. Mounted lazily — the first
/// `WhatsappPlugin::start()` initialises the lock.
static REGISTRY: OnceLock<Arc<DashMap<String, Arc<whatsapp_rs::Session>>>> = OnceLock::new();

/// Idempotent accessor — `OnceLock` initialiser races resolve to the
/// SAME `Arc<DashMap>` so every caller (registration + lookup +
/// dispatcher handle) shares one map.
fn map() -> Arc<DashMap<String, Arc<whatsapp_rs::Session>>> {
    REGISTRY.get_or_init(|| Arc::new(DashMap::new())).clone()
}

/// Register a paired `Session` under `instance_label`. Overwrites
/// any prior entry for the same label (re-pair scenarios). Called
/// from `WhatsappPlugin::start()` once `wa-agent` finishes its
/// connect handshake.
pub fn register(instance_label: &str, session: Arc<whatsapp_rs::Session>) {
    map().insert(instance_label.to_string(), session);
}

/// Drop the entry for `instance_label`. Called on plugin shutdown
/// so the registry doesn't hand stale `Arc<Session>`s to admin
/// callers after the plugin's background loops have stopped.
pub fn unregister(instance_label: &str) {
    map().remove(instance_label);
}

/// Look up the live `Session` for `instance_label`. Returns `None`
/// when the plugin instance isn't paired or hasn't booted yet.
pub fn lookup(instance_label: &str) -> Option<Arc<whatsapp_rs::Session>> {
    map().get(instance_label).map(|v| v.clone())
}

/// Iterate every registered `(instance_label, Session)` pair —
/// handy for "list every assigned bot across every account" admin
/// flows.
pub fn entries() -> Vec<(String, Arc<whatsapp_rs::Session>)> {
    map()
        .iter()
        .map(|kv| (kv.key().clone(), kv.value().clone()))
        .collect()
}

/// Dispatcher-side handle. Stateless — every call pulls the live
/// `Session` from the global registry, so re-pairs propagate
/// without re-injecting the handle.
pub struct WhatsappBotHandle;

/// Cap on every WhatsApp iq we make from the admin RPC handler.
/// The admin dispatcher reads one JSON-RPC frame at a time off the
/// microapp's stdin — if list_bots blocks the full 60s the
/// underlying `send_iq_await` allows, every other admin call backs
/// up behind it and the operator UI freezes. 8 s is a comfortable
/// trade-off: long enough for a roundtripping iq on a healthy
/// connection, short enough that an unresponsive server unblocks
/// the queue fast.
const ADMIN_IQ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// JID of Meta AI on every WhatsApp account. The `<iq xmlns="bot"
/// v="2">` query only enumerates **explicitly enrolled** AI
/// personas; Meta AI itself is implicit and never appears in the
/// section list. We surface it manually so the bubble UI can chat
/// with it from the first connect.
const META_AI_JID: &str = "718584497008509@bot";
const META_AI_PERSONA_ID: &str = "867051314767696$760019659443059";

#[async_trait]
impl WaBotHandle for WhatsappBotHandle {
    async fn list_bots(&self, agent_id: &str) -> WaBotResult<Vec<BotInfo>> {
        // For now `agent_id` is the WhatsApp plugin instance label.
        // Document upgrade path: when we add `agents.yaml` lookup,
        // resolve real agent ids → instance labels here before the
        // registry hit.
        let session = lookup(agent_id)
            .ok_or_else(|| anyhow!("no whatsapp session registered for {agent_id}"))?;
        // Always start with Meta AI — the official iq query
        // (`<iq xmlns="bot" v="2">`) does not list it because it's
        // an implicit, account-wide persona on every WhatsApp
        // install. Custom personas the operator has enrolled show
        // up via the iq below and get appended.
        let mut bots = vec![BotInfo {
            jid: META_AI_JID.to_string(),
            persona_id: META_AI_PERSONA_ID.to_string(),
        }];
        match tokio::time::timeout(ADMIN_IQ_TIMEOUT, session.list_bots()).await {
            Ok(Ok(discovered)) => {
                for b in discovered {
                    if b.jid == META_AI_JID {
                        continue;
                    }
                    bots.push(BotInfo {
                        jid: b.jid,
                        persona_id: b.persona_id,
                    });
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(target: "wa::bot_registry", error = %e, "discovery iq failed; returning default bot list");
            }
            Err(_) => {
                tracing::warn!(
                    target: "wa::bot_registry",
                    timeout = ?ADMIN_IQ_TIMEOUT,
                    "discovery iq timed out; returning default bot list"
                );
            }
        }
        Ok(bots)
    }

    async fn send_to_bot(&self, agent_id: &str, bot_jid: &str, text: &str) -> WaBotResult<String> {
        let session = lookup(agent_id)
            .ok_or_else(|| anyhow!("no whatsapp session registered for {agent_id}"))?;
        // Use the dedicated bot path: messageSecret + BotMetadata
        // get attached so the bot can encrypt + send a reply we'll
        // be able to decrypt. Falls back to the default Meta AI
        // persona id when the caller doesn't carry one.
        let id = tokio::time::timeout(
            ADMIN_IQ_TIMEOUT,
            session.send_text_to_bot(bot_jid, text, META_AI_PERSONA_ID),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "send_to_bot timed out after {:?} — message stanza ack pending",
                ADMIN_IQ_TIMEOUT
            )
        })??;
        Ok(id)
    }
}
