use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_broker::{AnyBroker, BrokerHandle, Event};
use agent_config::types::plugins::TelegramPluginConfig;
use agent_core::agent::plugin::{Command, Plugin, Response};
use async_trait::async_trait;
use dashmap::DashMap;
use serde::Deserialize;
use tokio::sync::{oneshot, Mutex, OnceCell};
use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bot::{
    split_text, truncate_utf16, BotClient, MediaSource, Message as TgMessage, MAX_TEXT_LEN,
};
use crate::events::{ForwardInfo, InboundEvent, MediaDescriptor};
use crate::session_id::session_id_for_chat;

pub const TOPIC_INBOUND: &str = "plugin.inbound.telegram";
pub const TOPIC_OUTBOUND: &str = "plugin.outbound.telegram";

/// Build the inbound topic for this plugin. When `instance` is set,
/// publishes land on `plugin.inbound.telegram.<instance>` so a specific
/// agent binding (`inbound_bindings: [{plugin: telegram, instance: X}]`)
/// can subscribe without cross-talk from other bots.
fn inbound_topic_for(instance: Option<&str>) -> String {
    match instance {
        Some(inst) if !inst.is_empty() => format!("{}.{}", TOPIC_INBOUND, inst),
        _ => TOPIC_INBOUND.to_string(),
    }
}

/// Mirror of `inbound_topic_for` for outbound: the dispatcher of a
/// specific instance subscribes to its own `plugin.outbound.telegram.<instance>`
/// so agents can route replies without cross-talk between bots.
fn outbound_topic_for(instance: Option<&str>) -> String {
    match instance {
        Some(inst) if !inst.is_empty() => format!("{}.{}", TOPIC_OUTBOUND, inst),
        _ => TOPIC_OUTBOUND.to_string(),
    }
}
pub const SOURCE: &str = "plugin.telegram";
const DEFAULT_BRIDGE_TIMEOUT_MS: u64 = 120_000;

/// One pending reply waiting on the bridge. Each inbound message gets
/// its own entry even when they share a session_id (same chat) —
/// otherwise fast consecutive messages clobber one another.
pub struct PendingEntry {
    pub entry_id: Uuid,
    pub tx: oneshot::Sender<String>,
    pub typing_abort: AbortHandle,
}

/// Pending replies keyed by session_id. Value is a FIFO queue because
/// the core runtime serialises messages per session (first in, first
/// replied), so a queue pop aligns reply N with message N.
pub type PendingMap = Arc<DashMap<Uuid, VecDeque<PendingEntry>>>;

#[derive(Debug, Clone, Default)]
pub struct PluginHealth {
    pub connected: bool,
    pub bot_username: Option<String>,
    pub last_error: Option<String>,
    pub updates_processed: u64,
    /// Unix seconds of the most recent inbound update we processed —
    /// compare against `Utc::now()` to detect a poller that silently
    /// stopped receiving (token revoked, network cut, Telegram down).
    pub last_update_ts: i64,
    pub outbound_success: u64,
    pub outbound_failure: u64,
    pub bridge_timeouts: u64,
}

pub struct TelegramPlugin {
    cfg: Arc<TelegramPluginConfig>,
    /// Per-instance registry name. `"telegram"` when no instance label
    /// is set (legacy single-bot), `"telegram.<instance>"` otherwise.
    /// `PluginRegistry` keys on this string, so multi-bot setups need
    /// unique names to avoid one overwriting another on register.
    registry_name: String,
    bot: OnceCell<Arc<BotClient>>,
    broker: OnceCell<AnyBroker>,
    pending: PendingMap,
    health: Arc<Mutex<PluginHealth>>,
    session_to_chat: Arc<DashMap<Uuid, i64>>,
    shutdown: CancellationToken,
    media_cache_dir: PathBuf,
    /// Top-level background tasks (poller + dispatcher) so `stop()`
    /// can join them instead of just signalling cancellation.
    spawned: Mutex<Vec<JoinHandle<()>>>,
}

impl TelegramPlugin {
    pub fn new(cfg: TelegramPluginConfig) -> Self {
        // Each instance gets its own media subdir so two bots running
        // in the same process don't race on a shared filename and so
        // the offset file (`<media_dir>/offset`) doesn't collide.
        let base_dir = std::env::var("TELEGRAM_MEDIA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("agent-telegram"));
        let media_cache_dir = match cfg.instance.as_deref() {
            Some(inst) if !inst.is_empty() => base_dir.join(inst),
            _ => base_dir,
        };
        let registry_name = match cfg.instance.as_deref() {
            Some(inst) if !inst.is_empty() => format!("telegram.{inst}"),
            _ => "telegram".to_string(),
        };
        Self {
            cfg: Arc::new(cfg),
            registry_name,
            bot: OnceCell::new(),
            broker: OnceCell::new(),
            pending: Arc::new(DashMap::new()),
            health: Arc::new(Mutex::new(PluginHealth::default())),
            session_to_chat: Arc::new(DashMap::new()),
            shutdown: CancellationToken::new(),
            media_cache_dir,
            spawned: Mutex::new(Vec::new()),
        }
    }

    pub fn config(&self) -> &TelegramPluginConfig {
        &self.cfg
    }

    pub async fn health(&self) -> PluginHealth {
        self.health.lock().await.clone()
    }
}

#[async_trait]
impl Plugin for TelegramPlugin {
    fn name(&self) -> &str {
        &self.registry_name
    }

    async fn start(&self, broker: AnyBroker) -> anyhow::Result<()> {
        let bot = Arc::new(BotClient::new(&self.cfg.token, None));
        let me = bot.get_me().await?;
        {
            let mut h = self.health.lock().await;
            h.connected = true;
            h.bot_username = me.username.clone();
            h.last_error = None;
        }
        self.bot
            .set(bot.clone())
            .map_err(|_| anyhow::anyhow!("telegram plugin already started"))?;
        self.broker
            .set(broker.clone())
            .map_err(|_| anyhow::anyhow!("telegram plugin already started"))?;

        tokio::fs::create_dir_all(&self.media_cache_dir).await.ok();

        let inbound_topic = inbound_topic_for(self.cfg.instance.as_deref());
        let connected = InboundEvent::Connected {
            bot_username: me.username.clone().unwrap_or_default(),
            bot_id: me.id,
        };
        let ev = Event::new(&inbound_topic, SOURCE, connected.to_payload());
        let _ = broker.publish(&inbound_topic, ev).await;

        let mut handles: Vec<JoinHandle<()>> = Vec::new();
        if self.cfg.polling.enabled {
            handles.push(spawn_poller(
                bot.clone(),
                broker.clone(),
                self.cfg.clone(),
                self.pending.clone(),
                self.session_to_chat.clone(),
                self.health.clone(),
                self.shutdown.clone(),
                self.media_cache_dir.clone(),
            ));
        }

        let disp = spawn_dispatcher(
            bot,
            broker,
            self.pending.clone(),
            self.session_to_chat.clone(),
            self.shutdown.clone(),
            self.health.clone(),
            self.cfg.instance.clone(),
        )
        .await?;
        handles.push(disp);

        *self.spawned.lock().await = handles;
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        self.shutdown.cancel();
        {
            let mut h = self.health.lock().await;
            h.connected = false;
        }
        // Drain pending bridges so their timers stop firing chat_action.
        for mut pair in self.pending.iter_mut() {
            for entry in pair.value_mut().drain(..) {
                entry.typing_abort.abort();
            }
        }
        self.pending.clear();
        // Wait for background tasks to acknowledge cancellation.
        let mut handles = std::mem::take(&mut *self.spawned.lock().await);
        for h in handles.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(5), h).await;
        }
        Ok(())
    }

    async fn send_command(&self, cmd: Command) -> anyhow::Result<Response> {
        let bot = self
            .bot
            .get()
            .ok_or_else(|| anyhow::anyhow!("telegram plugin not started"))?;
        match cmd {
            Command::SendMessage { to, text } => {
                let chat_id: i64 = to
                    .parse()
                    .map_err(|_| anyhow::anyhow!("`to` must be a chat id (integer)"))?;
                let sent = send_text_chunked(bot, chat_id, &text, None, None).await?;
                Ok(Response::MessageSent { message_id: sent })
            }
            Command::SendMedia { .. } => Ok(Response::Error {
                message: "use Custom{send_photo|send_audio|send_voice|send_video|send_document|send_animation} instead".into(),
            }),
            Command::Custom { name, payload } => dispatch_custom(bot, &name, payload).await,
        }
    }
}

#[doc(hidden)]
pub async fn dispatch_custom(
    bot: &Arc<BotClient>,
    name: &str,
    payload: serde_json::Value,
) -> anyhow::Result<Response> {
    match name {
        "chat_action" => {
            #[derive(Deserialize)]
            struct P {
                chat_id: i64,
                action: String,
            }
            let p: P = serde_json::from_value(payload)?;
            bot.send_chat_action(p.chat_id, &p.action).await?;
            Ok(Response::Ok)
        }
        "reply" => {
            #[derive(Deserialize)]
            struct P {
                chat_id: i64,
                msg_id: i64,
                text: String,
                #[serde(default)]
                parse_mode: Option<String>,
            }
            let p: P = serde_json::from_value(payload)?;
            let last = send_text_chunked(
                bot,
                p.chat_id,
                &p.text,
                Some(p.msg_id),
                p.parse_mode.as_deref(),
            )
            .await?;
            Ok(Response::MessageSent { message_id: last })
        }
        "send_with_format" => {
            #[derive(Deserialize)]
            struct P {
                chat_id: i64,
                text: String,
                parse_mode: String,
                #[serde(default)]
                reply_markup: Option<serde_json::Value>,
            }
            let p: P = serde_json::from_value(payload)?;
            // Formatted + inline-keyboard messages can't be safely split
            // across segments (Markdown entity spans, button layouts), so
            // cap the whole text with the same UTF-16 budget Telegram uses.
            let text = truncate_utf16(&p.text, MAX_TEXT_LEN);
            let sent = bot
                .send_message_full(
                    p.chat_id,
                    &text,
                    None,
                    Some(&p.parse_mode),
                    p.reply_markup.as_ref(),
                )
                .await?;
            Ok(Response::MessageSent {
                message_id: sent.message_id.to_string(),
            })
        }
        "edit_message" => {
            #[derive(Deserialize)]
            struct P {
                chat_id: i64,
                message_id: i64,
                text: String,
                #[serde(default)]
                parse_mode: Option<String>,
            }
            let p: P = serde_json::from_value(payload)?;
            bot.edit_message_text(p.chat_id, p.message_id, &p.text, p.parse_mode.as_deref())
                .await?;
            Ok(Response::MessageSent {
                message_id: p.message_id.to_string(),
            })
        }
        "reaction" => {
            #[derive(Deserialize)]
            struct P {
                chat_id: i64,
                message_id: i64,
                emoji: String,
            }
            let p: P = serde_json::from_value(payload)?;
            bot.set_message_reaction(p.chat_id, p.message_id, &p.emoji)
                .await?;
            Ok(Response::Ok)
        }
        "send_location" => {
            #[derive(Deserialize)]
            struct P {
                chat_id: i64,
                latitude: f64,
                longitude: f64,
            }
            let p: P = serde_json::from_value(payload)?;
            let sent = bot
                .send_location(p.chat_id, p.latitude, p.longitude)
                .await?;
            Ok(Response::MessageSent {
                message_id: sent.message_id.to_string(),
            })
        }
        "send_photo" => send_media_cmd(bot, payload, SendMediaKind::Photo).await,
        "send_audio" => send_media_cmd(bot, payload, SendMediaKind::Audio).await,
        "send_voice" => send_media_cmd(bot, payload, SendMediaKind::Voice).await,
        "send_video" => send_media_cmd(bot, payload, SendMediaKind::Video).await,
        "send_document" => send_media_cmd(bot, payload, SendMediaKind::Document).await,
        "send_animation" => send_media_cmd(bot, payload, SendMediaKind::Animation).await,
        _ => Ok(Response::Error {
            message: format!("unknown custom command `{name}`"),
        }),
    }
}

#[derive(Clone, Copy)]
enum SendMediaKind {
    Photo,
    Audio,
    Voice,
    Video,
    Document,
    Animation,
}

async fn send_media_cmd(
    bot: &Arc<BotClient>,
    payload: serde_json::Value,
    kind: SendMediaKind,
) -> anyhow::Result<Response> {
    #[derive(Deserialize)]
    struct P {
        chat_id: i64,
        source: serde_json::Value,
        #[serde(default)]
        caption: Option<String>,
        #[serde(default)]
        parse_mode: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        performer: Option<String>,
        #[serde(default)]
        duration: Option<u64>,
    }
    let p: P = serde_json::from_value(payload)?;
    let src = MediaSource::from_json(&p.source)?;
    let pm = p.parse_mode.as_deref();
    let cap = p.caption.as_deref();
    let sent = match kind {
        SendMediaKind::Photo => bot.send_photo(p.chat_id, &src, cap, pm).await?,
        SendMediaKind::Audio => {
            bot.send_audio(
                p.chat_id,
                &src,
                cap,
                p.title.as_deref(),
                p.performer.as_deref(),
                p.duration,
                pm,
            )
            .await?
        }
        SendMediaKind::Voice => bot.send_voice(p.chat_id, &src, cap, p.duration, pm).await?,
        SendMediaKind::Video => bot.send_video(p.chat_id, &src, cap, p.duration, pm).await?,
        SendMediaKind::Document => bot.send_document(p.chat_id, &src, cap, pm).await?,
        SendMediaKind::Animation => bot.send_animation(p.chat_id, &src, cap, pm).await?,
    };
    Ok(Response::MessageSent {
        message_id: sent.message_id.to_string(),
    })
}

/// Send text, splitting at Telegram's per-message cap. Returns the
/// `message_id` of the LAST chunk (useful for downstream reply refs).
///
/// Threading note: when `reply_to` is set, **every** chunk carries it
/// — not just the first. Long replies otherwise lose the "in reply to"
/// hint mid-message, which is jarring in busy group chats where the
/// last chunks can be separated from the first by other users' turns.
async fn send_text_chunked(
    bot: &Arc<BotClient>,
    chat_id: i64,
    text: &str,
    reply_to: Option<i64>,
    parse_mode: Option<&str>,
) -> anyhow::Result<String> {
    let segments = split_text(text, MAX_TEXT_LEN);
    let mut last_id = String::new();
    for seg in segments.iter() {
        let sent = bot
            .send_message_full(chat_id, seg, reply_to, parse_mode, None)
            .await?;
        last_id = sent.message_id.to_string();
    }
    Ok(last_id)
}

#[allow(clippy::too_many_arguments)]
fn spawn_poller(
    bot: Arc<BotClient>,
    broker: AnyBroker,
    cfg: Arc<TelegramPluginConfig>,
    pending: PendingMap,
    session_to_chat: Arc<DashMap<Uuid, i64>>,
    health: Arc<Mutex<PluginHealth>>,
    shutdown: CancellationToken,
    media_dir: PathBuf,
) -> JoinHandle<()> {
    // The bridge waits this long per message before giving up on the
    // agent's reply. Configurable: long tool chains (LLM → tool → LLM)
    // routinely breach the old 30s default.
    let bridge_timeout_ms = if cfg.bridge_timeout_ms == 0 {
        DEFAULT_BRIDGE_TIMEOUT_MS
    } else {
        cfg.bridge_timeout_ms
    };
    // Precompute the allowlist as a HashSet so repeated membership
    // checks are O(1) instead of linear over a Vec.
    let allowlist: HashSet<i64> = cfg.allowlist.chat_ids.iter().copied().collect();
    let has_allowlist = !allowlist.is_empty();
    // Telegram long-polling: we tell their server how long to hold the
    // request open. The user-facing `polling.interval_ms` is a total
    // round-trip cadence hint; clamp to [1, 50] seconds (Telegram's
    // own limit) and subtract a small margin so the HTTP timeout still
    // outwaits the server by ~10s.
    let poll_secs: u64 = (cfg.polling.interval_ms / 1000).clamp(1, 50);
    // Offset persistence: load last-seen offset from disk so restarts
    // don't replay the update backlog (Telegram retains up to 24h).
    let offset_path = resolve_offset_path(&cfg, &media_dir);
    // Per-instance inbound topic. Single-bot setups fall through to
    // the legacy `plugin.inbound.telegram`; multi-bot setups use
    // `plugin.inbound.telegram.<instance>` so agent bindings can target
    // a specific bot.
    let inbound_topic = inbound_topic_for(cfg.instance.as_deref());
    tokio::spawn(async move {
        let mut offset: i64 = load_offset(&offset_path).await;
        if offset > 0 {
            tracing::info!(
                offset,
                path = %offset_path.display(),
                "telegram resumed from persisted offset"
            );
        }
        // Backoff for transient getUpdates failures: 1s → 2s → 4s → 8s …
        // up to 60s cap. Resets to 1s after any successful poll.
        let mut backoff = Duration::from_secs(1);
        const MAX_BACKOFF: Duration = Duration::from_secs(60);
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            let updates = match bot
                .get_updates(
                    offset,
                    poll_secs,
                    &[
                        "message",
                        "edited_message",
                        "channel_post",
                        "callback_query",
                        "my_chat_member",
                    ],
                )
                .await
            {
                Ok(u) => {
                    backoff = Duration::from_secs(1);
                    u
                }
                Err(e) => {
                    tracing::warn!(error = %e, backoff_ms = backoff.as_millis() as u64, "telegram getUpdates failed");
                    {
                        let mut h = health.lock().await;
                        h.last_error = Some(e.to_string());
                    }
                    let wait = backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(wait) => {}
                    }
                    continue;
                }
            };

            let had_updates = !updates.is_empty();
            for upd in updates {
                offset = offset.max(upd.update_id + 1);
                // Membership change (bot added/removed/kicked from a
                // chat, or user blocked/unblocked the bot in DM).
                // Informational; fire the event and move on — no bridge
                // reply expected.
                if let Some(mcm) = upd.my_chat_member.clone() {
                    let chat_id_mcm = mcm.chat.id;
                    if has_allowlist && !allowlist.contains(&chat_id_mcm) {
                        continue;
                    }
                    let changed_by = mcm
                        .from
                        .username
                        .clone()
                        .unwrap_or_else(|| mcm.from.id.to_string());
                    let event = InboundEvent::ChatMembership {
                        chat: chat_id_mcm.to_string(),
                        chat_title: mcm.chat.title.clone(),
                        old_status: mcm.old_chat_member.status.clone(),
                        new_status: mcm.new_chat_member.status.clone(),
                        changed_by,
                        timestamp: mcm.date,
                    };
                    let mut ev = Event::new(&inbound_topic, SOURCE, event.to_payload());
                    ev.session_id = Some(session_id_for_chat(chat_id_mcm));
                    let _ = broker.publish(&inbound_topic, ev).await;
                    continue;
                }
                // Inline-keyboard button press. ACK immediately so
                // Telegram stops the loading spinner, then publish an
                // informational event for the agent runtime.
                if let Some(cq) = upd.callback_query.clone() {
                    let _ = bot.answer_callback_query(&cq.id, None, false).await;
                    let chat_id_cq = cq.message.as_ref().map(|m| m.chat.id).unwrap_or(0);
                    if has_allowlist && chat_id_cq != 0 && !allowlist.contains(&chat_id_cq) {
                        continue;
                    }
                    let from = cq
                        .from
                        .as_ref()
                        .map(|u| u.id.to_string())
                        .unwrap_or_default();
                    let event = InboundEvent::CallbackQuery {
                        from,
                        chat: chat_id_cq.to_string(),
                        data: cq.data.clone().unwrap_or_default(),
                        msg_id: cq.message.as_ref().map(|m| m.message_id.to_string()),
                        username: cq.from.as_ref().and_then(|u| u.username.clone()),
                        callback_id: cq.id.clone(),
                    };
                    let mut ev = Event::new(&inbound_topic, SOURCE, event.to_payload());
                    if chat_id_cq != 0 {
                        ev.session_id = Some(session_id_for_chat(chat_id_cq));
                    }
                    let _ = broker.publish(&inbound_topic, ev).await;
                    continue;
                }
                // Accept message / edited_message / channel_post.
                let Some(msg) = upd
                    .message
                    .clone()
                    .or_else(|| upd.edited_message.clone())
                    .or_else(|| upd.channel_post.clone())
                else {
                    continue;
                };
                let chat_id = msg.chat.id;
                if has_allowlist && !allowlist.contains(&chat_id) {
                    tracing::debug!(chat_id, "telegram chat outside allowlist; ignored");
                    continue;
                }
                let session_id = session_id_for_chat(chat_id);
                session_to_chat.insert(session_id, chat_id);

                // Download media (best-effort). If it fails, keep going
                // with text-only — skills can still reply to the caption.
                let media = download_media(&bot, &msg, &media_dir).await;
                let latitude = msg.location.as_ref().map(|l| l.latitude);
                let longitude = msg.location.as_ref().map(|l| l.longitude);

                let mut text = msg
                    .text
                    .clone()
                    .or_else(|| msg.caption.clone())
                    .unwrap_or_default();

                // Auto-transcribe voice / audio into `text` so the agent
                // runtime treats the message like a regular typed turn.
                // Original audio path stays on the InboundEvent.media
                // field for skills that want the raw file.
                if cfg.auto_transcribe.enabled {
                    if let Some(m) = media.iter().find(|m| m.kind == "voice" || m.kind == "audio")
                    {
                        if text.is_empty() {
                            // Pre-announce so the user sees feedback for
                            // the transcription leg, not just the reply.
                            let _ = bot.send_chat_action(chat_id, "typing").await;
                            if let Some(t) =
                                transcribe_voice(&cfg.auto_transcribe, &m.local_path).await
                            {
                                text = t;
                            }
                        }
                    }
                }
                // Stable identity for the agent: prefer numeric user_id
                // over username (users can rename themselves, invalidating
                // memory tied to the old handle). Expose username as a
                // separate metadata field.
                let from = msg
                    .from
                    .as_ref()
                    .map(|u| u.id.to_string())
                    .unwrap_or_else(|| chat_id.to_string());
                let forward = extract_forward_info(&msg);
                let payload = InboundEvent::Message {
                    from: from.clone(),
                    chat: chat_id.to_string(),
                    chat_type: msg.chat.kind.clone(),
                    text: if text.is_empty() {
                        None
                    } else {
                        Some(text.clone())
                    },
                    reply_to: msg
                        .reply_to_message
                        .as_ref()
                        .map(|m| m.message_id.to_string()),
                    is_group: matches!(msg.chat.kind.as_str(), "group" | "supergroup" | "channel"),
                    timestamp: msg.date,
                    msg_id: msg.message_id.to_string(),
                    username: msg.from.as_ref().and_then(|u| u.username.clone()),
                    media,
                    latitude,
                    longitude,
                    forward,
                };

                // Bridge: one entry per inbound message so consecutive
                // messages from the same chat queue FIFO instead of
                // clobbering each other's oneshot channel. Each entry
                // owns an AbortHandle for its own typing ticker so the
                // ticker dies with the bridge it was created for.
                let (tx, rx) = oneshot::channel::<String>();
                let entry_id = Uuid::new_v4();
                let bot_for_typing = bot.clone();
                let typing_task = tokio::spawn(async move {
                    // Fire once immediately, then refresh every 4s —
                    // Telegram's typing indicator lasts ~5s per call.
                    let _ = bot_for_typing.send_chat_action(chat_id, "typing").await;
                    let mut interval = tokio::time::interval(Duration::from_millis(4000));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    interval.tick().await; // immediate tick, already fired above
                    loop {
                        interval.tick().await;
                        if let Err(e) = bot_for_typing.send_chat_action(chat_id, "typing").await {
                            tracing::debug!(chat_id, error = %e, "telegram typing action failed");
                        }
                    }
                });
                let entry = PendingEntry {
                    entry_id,
                    tx,
                    typing_abort: typing_task.abort_handle(),
                };
                pending.entry(session_id).or_default().push_back(entry);

                let mut ev = Event::new(&inbound_topic, SOURCE, payload.to_payload());
                ev.session_id = Some(session_id);
                if let Err(e) = broker.publish(&inbound_topic, ev).await {
                    tracing::warn!(%session_id, error = %e, "telegram inbound publish failed");
                    // Pull our entry back out — nobody will ever drain it.
                    if let Some(mut q) = pending.get_mut(&session_id) {
                        q.retain(|e| e.entry_id != entry_id);
                    }
                    typing_task.abort();
                    continue;
                }

                let bot_for_reply = bot.clone();
                let broker_for_timeout = broker.clone();
                let pending_for_timeout = pending.clone();
                let health_for_reply = health.clone();
                let msg_id = msg.message_id;
                // Clone once per inner task so each loop iteration gets
                // its own String (the outer `inbound_topic` lives for
                // the poller's whole lifetime; the inner async move
                // would otherwise consume it on the first iteration).
                let inbound_topic_inner = inbound_topic.clone();
                tokio::spawn(async move {
                    let inbound_topic = inbound_topic_inner;
                    let result =
                        tokio::time::timeout(Duration::from_millis(bridge_timeout_ms), rx).await;
                    // Regardless of outcome, make sure our typing ticker
                    // stops and our entry doesn't linger in the queue.
                    typing_task.abort();
                    if let Some(mut q) = pending_for_timeout.get_mut(&session_id) {
                        q.retain(|e| e.entry_id != entry_id);
                    }
                    match result {
                        Ok(Ok(reply)) => {
                            if !reply.is_empty() {
                                // Thread the reply to the inbound message
                                // so Telegram shows the "replies to" hint
                                // (nice UX in busy group chats).
                                match send_text_chunked(
                                    &bot_for_reply,
                                    chat_id,
                                    &reply,
                                    Some(msg_id),
                                    None,
                                )
                                .await
                                {
                                    Ok(_) => {
                                        let mut h = health_for_reply.lock().await;
                                        h.outbound_success += 1;
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            chat_id,
                                            error = %e,
                                            "telegram sendMessage failed"
                                        );
                                        let mut h = health_for_reply.lock().await;
                                        h.outbound_failure += 1;
                                    }
                                }
                            }
                        }
                        // Sender dropped (the dispatcher popped ahead of
                        // us and sent to a different entry).
                        Ok(Err(_)) => {}
                        Err(_elapsed) => {
                            let ev = InboundEvent::BridgeTimeout { session_id };
                            let mut e = Event::new(&inbound_topic, SOURCE, ev.to_payload());
                            e.session_id = Some(session_id);
                            let _ = broker_for_timeout.publish(&inbound_topic, e).await;
                            {
                                let mut h = health_for_reply.lock().await;
                                h.bridge_timeouts += 1;
                            }
                            tracing::info!(
                                %session_id,
                                chat_id,
                                bridge_timeout_ms,
                                "telegram bridge timeout"
                            );
                        }
                    }
                });

                {
                    let mut h = health.lock().await;
                    h.updates_processed += 1;
                    h.last_update_ts = chrono::Utc::now().timestamp();
                }
            }

            if had_updates {
                if let Err(e) = save_offset(&offset_path, offset).await {
                    tracing::debug!(error = %e, "telegram: persist offset failed");
                }
            }

            if shutdown.is_cancelled() {
                return;
            }
        }
    })
}

/// Translate Telegram's forward metadata into our flat `ForwardInfo`.
/// Supports both the legacy shape (`forward_from` / `forward_from_chat`)
/// and the Bot API >= 7.0 `forward_origin` tagged enum. Returns `None`
/// when the message is not a forward.
fn extract_forward_info(msg: &TgMessage) -> Option<ForwardInfo> {
    // Prefer modern `forward_origin` when present — carries author name,
    // chat title, channel post id even for "hidden-sender" forwards.
    if let Some(origin) = msg.forward_origin.as_ref() {
        let kind = origin.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let source = match kind {
            "user" => origin
                .pointer("/sender_user/first_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    origin
                        .pointer("/sender_user/username")
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("user")
                .to_string(),
            "hidden_user" => origin
                .get("sender_user_name")
                .and_then(|v| v.as_str())
                .unwrap_or("hidden user")
                .to_string(),
            "chat" => origin
                .pointer("/sender_chat/title")
                .and_then(|v| v.as_str())
                .unwrap_or("chat")
                .to_string(),
            "channel" => origin
                .pointer("/chat/title")
                .and_then(|v| v.as_str())
                .unwrap_or("channel")
                .to_string(),
            _ => "forward".to_string(),
        };
        let from_user_id = origin.pointer("/sender_user/id").and_then(|v| v.as_i64());
        let from_chat_id = origin
            .pointer("/sender_chat/id")
            .and_then(|v| v.as_i64())
            .or_else(|| origin.pointer("/chat/id").and_then(|v| v.as_i64()));
        let date = origin.get("date").and_then(|v| v.as_i64());
        return Some(ForwardInfo {
            source,
            from_user_id,
            from_chat_id,
            date,
        });
    }
    // Fall back to legacy shape.
    if let Some(u) = msg.forward_from.as_ref() {
        let source = u
            .username
            .clone()
            .map(|h| format!("@{h}"))
            .or_else(|| u.first_name.clone())
            .unwrap_or_else(|| u.id.to_string());
        return Some(ForwardInfo {
            source,
            from_user_id: Some(u.id),
            from_chat_id: None,
            date: msg.forward_date,
        });
    }
    if let Some(c) = msg.forward_from_chat.as_ref() {
        let source = c
            .title
            .clone()
            .or_else(|| c.username.clone().map(|h| format!("@{h}")))
            .unwrap_or_else(|| format!("chat:{}", c.id));
        return Some(ForwardInfo {
            source,
            from_user_id: None,
            from_chat_id: Some(c.id),
            date: msg.forward_date,
        });
    }
    None
}

fn resolve_offset_path(cfg: &TelegramPluginConfig, media_dir: &Path) -> PathBuf {
    cfg.polling
        .offset_path
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| media_dir.join("offset"))
}

pub async fn load_offset(path: &PathBuf) -> i64 {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => s.trim().parse::<i64>().unwrap_or(0),
        Err(_) => 0, // first run or IO error — start from scratch
    }
}

pub async fn save_offset(path: &PathBuf, offset: i64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    // Write to a temp file and rename atomically so a crash mid-write
    // can't leave a half-truncated offset file on disk.
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, offset.to_string()).await?;
    tokio::fs::rename(&tmp, path).await
}

/// Detect all media kinds the message carries, fetch each `file_path`
/// via `getFile`, and stream-download into `media_dir`. Returns one
/// descriptor per successfully downloaded attachment.
async fn download_media(
    bot: &BotClient,
    msg: &TgMessage,
    media_dir: &Path,
) -> Vec<MediaDescriptor> {
    let mut candidates: Vec<MediaDownloadInput> = Vec::new();
    // Pick highest-quality photo variant.
    if let Some(photos) = msg.photo.as_ref().and_then(|v| v.last()) {
        candidates.push(MediaDownloadInput {
            kind: "photo",
            file_id: photos.file_id.clone(),
            mime: None,
            size: photos.file_size,
            duration: None,
            width: Some(photos.width),
            height: Some(photos.height),
            file_name: None,
            ext_hint: "jpg",
        });
    }
    if let Some(v) = &msg.voice {
        candidates.push(MediaDownloadInput {
            kind: "voice",
            file_id: v.file_id.clone(),
            mime: v.mime_type.clone(),
            size: v.file_size,
            duration: Some(v.duration),
            width: None,
            height: None,
            file_name: None,
            ext_hint: "oga",
        });
    }
    if let Some(a) = &msg.audio {
        candidates.push(MediaDownloadInput {
            kind: "audio",
            file_id: a.file_id.clone(),
            mime: a.mime_type.clone(),
            size: a.file_size,
            duration: Some(a.duration),
            width: None,
            height: None,
            file_name: a.file_name.clone(),
            ext_hint: "mp3",
        });
    }
    if let Some(v) = &msg.video {
        candidates.push(MediaDownloadInput {
            kind: "video",
            file_id: v.file_id.clone(),
            mime: v.mime_type.clone(),
            size: v.file_size,
            duration: Some(v.duration),
            width: Some(v.width),
            height: Some(v.height),
            file_name: v.file_name.clone(),
            ext_hint: "mp4",
        });
    }
    if let Some(v) = &msg.video_note {
        candidates.push(MediaDownloadInput {
            kind: "video_note",
            file_id: v.file_id.clone(),
            mime: None,
            size: v.file_size,
            duration: Some(v.duration),
            width: Some(v.length),
            height: Some(v.length),
            file_name: None,
            ext_hint: "mp4",
        });
    }
    if let Some(a) = &msg.animation {
        candidates.push(MediaDownloadInput {
            kind: "animation",
            file_id: a.file_id.clone(),
            mime: a.mime_type.clone(),
            size: a.file_size,
            duration: Some(a.duration),
            width: Some(a.width),
            height: Some(a.height),
            file_name: a.file_name.clone(),
            ext_hint: "mp4",
        });
    }
    if let Some(d) = &msg.document {
        candidates.push(MediaDownloadInput {
            kind: "document",
            file_id: d.file_id.clone(),
            mime: d.mime_type.clone(),
            size: d.file_size,
            duration: None,
            width: None,
            height: None,
            file_name: d.file_name.clone(),
            ext_hint: "bin",
        });
    }
    if let Some(s) = &msg.sticker {
        candidates.push(MediaDownloadInput {
            kind: "sticker",
            file_id: s.file_id.clone(),
            mime: None,
            size: s.file_size,
            duration: None,
            width: Some(s.width),
            height: Some(s.height),
            file_name: None,
            ext_hint: if s.is_video {
                "webm"
            } else if s.is_animated {
                "tgs"
            } else {
                "webp"
            },
        });
    }

    let mut out: Vec<MediaDescriptor> = Vec::new();
    for c in candidates {
        if let Some(m) = download_one_media(bot, msg, media_dir, c).await {
            out.push(m);
        }
    }
    out
}

async fn download_one_media(
    bot: &BotClient,
    msg: &TgMessage,
    media_dir: &Path,
    c: MediaDownloadInput,
) -> Option<MediaDescriptor> {
    let info = match bot.get_file(&c.file_id).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, kind = c.kind, "telegram getFile failed");
            return None;
        }
    };
    let remote_path = info.file_path?;
    let ext = std::path::Path::new(&remote_path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or(c.ext_hint);
    let dest = media_dir.join(format!(
        "{}-{}-{}.{ext}",
        msg.chat.id,
        msg.message_id,
        c.file_id.chars().take(10).collect::<String>()
    ));
    // Cache dedup: same chat/msg/file_id prefix lands on the same path.
    // A re-delivered or forwarded media can reuse it without re-downloading.
    let size_on_disk = tokio::fs::metadata(&dest).await.ok().map(|m| m.len());
    if size_on_disk.is_some() {
        tracing::debug!(path = %dest.display(), kind = c.kind, "telegram media cache hit");
    } else if let Err(e) = bot.download_file(&remote_path, &dest).await {
        tracing::warn!(error = %e, kind = c.kind, "telegram download failed");
        return None;
    }

    Some(MediaDescriptor {
        kind: c.kind.to_string(),
        local_path: dest.to_string_lossy().to_string(),
        file_id: c.file_id,
        mime_type: c.mime,
        file_size: c.size,
        duration_s: c.duration,
        width: c.width,
        height: c.height,
        file_name: c.file_name,
    })
}

struct MediaDownloadInput {
    kind: &'static str,
    file_id: String,
    mime: Option<String>,
    size: Option<u64>,
    duration: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    file_name: Option<String>,
    ext_hint: &'static str,
}

/// Spawn the openai-whisper extension binary, send a single
/// `tools/call { transcribe_file, file_path }` JSON-RPC request, wait
/// for the reply line, and return the transcribed text (or None on any
/// failure — caller proceeds with empty text).
async fn transcribe_voice(
    cfg: &agent_config::types::plugins::TelegramAutoTranscribeConfig,
    audio_path: &str,
) -> Option<String> {
    let command = cfg.command.trim();
    if command.is_empty() {
        return None;
    }
    if !std::path::Path::new(command).exists() {
        tracing::warn!(
            command,
            "telegram auto_transcribe: whisper binary not found"
        );
        return None;
    }

    let mut args = serde_json::json!({ "file_path": audio_path, "response_format": "text" });
    if let Some(lang) = cfg.language.as_ref().filter(|s| !s.is_empty()) {
        args["language"] = serde_json::Value::String(lang.clone());
    }
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "transcribe_file", "arguments": args },
    });
    let shutdown = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"shutdown"});

    // Spawn the child OUTSIDE the timeout future so we can still kill
    // it from the timeout arm — the old shape dropped the future mid-
    // await on timeout, leaking the subprocess.
    let mut child = match tokio::process::Command::new(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(command, error = %e, "telegram auto_transcribe: spawn failed");
            return None;
        }
    };
    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            tracing::warn!("telegram auto_transcribe: child has no stdin");
            let _ = child.kill().await;
            return None;
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            tracing::warn!("telegram auto_transcribe: child has no stdout");
            let _ = child.kill().await;
            return None;
        }
    };

    let payload = format!("{request}\n{shutdown}\n");
    {
        use tokio::io::AsyncWriteExt;
        if let Err(e) = stdin.write_all(payload.as_bytes()).await {
            tracing::warn!(error = %e, "telegram auto_transcribe: stdin write failed");
            let _ = child.kill().await;
            return None;
        }
    }
    drop(stdin);

    let read_loop = async {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stdout).lines();
        while let Some(line) = reader.next_line().await.map_err(|e| e.to_string())? {
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if v.get("id").and_then(|v| v.as_u64()) != Some(1) {
                continue;
            }
            if let Some(err) = v.get("error") {
                return Err(format!("whisper error: {err}"));
            }
            if let Some(text) = v.pointer("/result/text").and_then(|v| v.as_str()) {
                return Ok(text.to_string());
            }
        }
        Err("whisper produced no text".to_string())
    };

    let text = match tokio::time::timeout(Duration::from_millis(cfg.timeout_ms), read_loop).await {
        Ok(Ok(text)) => Some(text),
        Ok(Err(e)) => {
            tracing::warn!(audio_path, error = %e, "telegram auto_transcribe failed");
            None
        }
        Err(_) => {
            tracing::warn!(
                audio_path,
                timeout_ms = cfg.timeout_ms,
                "telegram auto_transcribe timeout — killing whisper child"
            );
            None
        }
    };
    // Always reap the child — either the read loop saw shutdown and it
    // exited cleanly, or we hit the timeout and must kill it. The
    // `kill_on_drop(true)` flag is a last-resort net if this path is
    // skipped somehow.
    let _ = child.kill().await;
    let _ = child.wait().await;
    text
}

async fn spawn_dispatcher(
    bot: Arc<BotClient>,
    broker: AnyBroker,
    pending: PendingMap,
    session_to_chat: Arc<DashMap<Uuid, i64>>,
    shutdown: CancellationToken,
    health: Arc<Mutex<PluginHealth>>,
    instance: Option<String>,
) -> anyhow::Result<JoinHandle<()>> {
    let outbound_topic = outbound_topic_for(instance.as_deref());
    tracing::info!(
        topic = %outbound_topic,
        instance = instance.as_deref().unwrap_or("-"),
        "telegram dispatcher subscribing"
    );
    let mut sub = broker.subscribe(&outbound_topic).await?;
    let handle = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                _ = shutdown.cancelled() => return,
                ev = sub.next() => ev,
            };
            let Some(event) = event else {
                return;
            };
            let sid: Option<Uuid> = event.session_id;
            let payload: serde_json::Value = event.payload;

            // Text replies: pop the oldest pending entry for this
            // session_id. FIFO matches the core runtime's per-session
            // serialisation: first message in gets first reply out.
            if let Some(sid) = sid {
                let popped = pending.get_mut(&sid).and_then(|mut q| q.pop_front());
                // Clean up empty queues so DashMap doesn't grow unbounded
                // over churn of short-lived chats.
                let mut drop_session = false;
                if let Some(q) = pending.get(&sid) {
                    if q.is_empty() {
                        drop_session = true;
                    }
                }
                if drop_session {
                    pending.remove(&sid);
                }
                if let Some(entry) = popped {
                    entry.typing_abort.abort();
                    let text: String = payload
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if entry.tx.send(text).is_ok() {
                        continue;
                    }
                    // Receiver died — fall through to the proactive path.
                }
            }

            let explicit: Option<i64> = payload
                .get("to")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.parse::<i64>().ok())
                .or_else(|| payload.get("chat_id").and_then(serde_json::Value::as_i64));
            let chat_id: Option<i64> =
                explicit.or_else(|| sid.and_then(|s| session_to_chat.get(&s).map(|c| *c.value())));
            let Some(cid) = chat_id else {
                tracing::debug!("telegram outbound dropped: no chat_id resolved");
                continue;
            };

            // Rich proactive path: branch by `kind` field or by presence
            // of media hints; fall back to text.
            let kind = payload.get("kind").and_then(serde_json::Value::as_str);
            let source = payload.get("source").cloned();
            let caption = payload
                .get("caption")
                .and_then(serde_json::Value::as_str)
                .map(|s| s.to_string());
            let parse_mode = payload
                .get("parse_mode")
                .and_then(serde_json::Value::as_str)
                .map(|s| s.to_string());

            // Proactive paths haven't had a typing indicator kicked off
            // yet (only bridge-routed replies get one via the poller),
            // so fire the appropriate chat_action before the send lands.
            let action = match kind {
                Some("photo") | Some("animation") => "upload_photo",
                Some("voice") | Some("audio") => "record_voice",
                Some("video") => "upload_video",
                Some("document") => "upload_document",
                _ => "typing",
            };
            let _ = bot.send_chat_action(cid, action).await;

            match (kind, source) {
                (Some(k @ "photo"), Some(src)) => match MediaSource::from_json(&src) {
                    Ok(s) => {
                        if let Err(e) = bot
                            .send_photo(cid, &s, caption.as_deref(), parse_mode.as_deref())
                            .await
                        {
                            tracing::warn!(chat_id = cid, kind = k, error = %e, "telegram media send failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(kind = k, error = %e, "telegram: invalid media source")
                    }
                },
                (Some(k @ "voice"), Some(src)) => match MediaSource::from_json(&src) {
                    Ok(s) => {
                        if let Err(e) = bot
                            .send_voice(cid, &s, caption.as_deref(), None, parse_mode.as_deref())
                            .await
                        {
                            tracing::warn!(chat_id = cid, kind = k, error = %e, "telegram media send failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(kind = k, error = %e, "telegram: invalid media source")
                    }
                },
                (Some(k @ "audio"), Some(src)) => match MediaSource::from_json(&src) {
                    Ok(s) => {
                        if let Err(e) = bot
                            .send_audio(
                                cid,
                                &s,
                                caption.as_deref(),
                                None,
                                None,
                                None,
                                parse_mode.as_deref(),
                            )
                            .await
                        {
                            tracing::warn!(chat_id = cid, kind = k, error = %e, "telegram media send failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(kind = k, error = %e, "telegram: invalid media source")
                    }
                },
                (Some(k @ "video"), Some(src)) => match MediaSource::from_json(&src) {
                    Ok(s) => {
                        if let Err(e) = bot
                            .send_video(cid, &s, caption.as_deref(), None, parse_mode.as_deref())
                            .await
                        {
                            tracing::warn!(chat_id = cid, kind = k, error = %e, "telegram media send failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(kind = k, error = %e, "telegram: invalid media source")
                    }
                },
                (Some(k @ "document"), Some(src)) => match MediaSource::from_json(&src) {
                    Ok(s) => {
                        if let Err(e) = bot
                            .send_document(cid, &s, caption.as_deref(), parse_mode.as_deref())
                            .await
                        {
                            tracing::warn!(chat_id = cid, kind = k, error = %e, "telegram media send failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(kind = k, error = %e, "telegram: invalid media source")
                    }
                },
                (Some(k @ "animation"), Some(src)) => match MediaSource::from_json(&src) {
                    Ok(s) => {
                        if let Err(e) = bot
                            .send_animation(cid, &s, caption.as_deref(), parse_mode.as_deref())
                            .await
                        {
                            tracing::warn!(chat_id = cid, kind = k, error = %e, "telegram media send failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(kind = k, error = %e, "telegram: invalid media source")
                    }
                },
                _ => {
                    let text: String = payload
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if text.is_empty() {
                        tracing::debug!("telegram outbound dropped: empty text");
                        continue;
                    }
                    match send_text_chunked(&bot, cid, &text, None, parse_mode.as_deref()).await {
                        Ok(_) => {
                            let mut h = health.lock().await;
                            h.outbound_success += 1;
                        }
                        Err(e) => {
                            tracing::warn!(chat_id = cid, error = %e, "telegram proactive send failed");
                            let mut h = health.lock().await;
                            h.outbound_failure += 1;
                        }
                    }
                }
            }
        }
    });
    Ok(handle)
}

#[cfg(test)]
mod topic_tests {
    use super::{inbound_topic_for, outbound_topic_for};

    #[test]
    fn single_bot_uses_legacy_topic() {
        assert_eq!(inbound_topic_for(None), "plugin.inbound.telegram");
        assert_eq!(outbound_topic_for(None), "plugin.outbound.telegram");
    }

    #[test]
    fn empty_instance_same_as_none() {
        assert_eq!(inbound_topic_for(Some("")), "plugin.inbound.telegram");
        assert_eq!(outbound_topic_for(Some("")), "plugin.outbound.telegram");
    }

    #[test]
    fn named_instance_appends_segment() {
        assert_eq!(
            inbound_topic_for(Some("sales")),
            "plugin.inbound.telegram.sales"
        );
        assert_eq!(
            outbound_topic_for(Some("sales")),
            "plugin.outbound.telegram.sales"
        );
    }

    #[test]
    fn inbound_and_outbound_use_matching_suffixes() {
        // A core runtime subscribing to `plugin.inbound.telegram.>`
        // needs the outbound side to mirror the same instance segment
        // so routing round-trips cleanly.
        let inst = Some("bot_v2");
        let in_t = inbound_topic_for(inst);
        let out_t = outbound_topic_for(inst);
        let in_suffix = in_t.strip_prefix("plugin.inbound.telegram").unwrap_or("");
        let out_suffix = out_t.strip_prefix("plugin.outbound.telegram").unwrap_or("");
        assert_eq!(in_suffix, out_suffix);
        assert_eq!(in_suffix, ".bot_v2");
    }
}
