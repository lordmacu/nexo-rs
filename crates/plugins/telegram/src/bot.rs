//! Thin client for the Telegram Bot HTTP API.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;

/// Maximum length Telegram accepts in a single `sendMessage`.
/// Wire limit is 4096 UTF-16 code units.
pub const MAX_TEXT_LEN: usize = 4096;
/// Maximum caption length on media messages.
pub const MAX_CAPTION_LEN: usize = 1024;

/// Normalise common `parse_mode` typos to the exact strings Telegram's
/// API accepts. Returns `Err` for values that obviously won't work so
/// the caller gets a clear error instead of an opaque 400 from the API.
pub fn normalize_parse_mode(raw: &str) -> Result<&'static str, String> {
    match raw {
        "MarkdownV2" | "markdownv2" | "mdv2" | "MDV2" | "MD2" => Ok("MarkdownV2"),
        "Markdown" | "markdown" | "md" | "MD" => Ok("Markdown"),
        "HTML" | "html" | "Html" => Ok("HTML"),
        other => Err(format!(
            "unsupported parse_mode `{other}` — use `MarkdownV2`, `Markdown`, or `HTML`"
        )),
    }
}

/// Where to source a file for send_* APIs.
#[derive(Debug, Clone)]
pub enum MediaSource {
    Url(String),
    FileId(String),
    Path(PathBuf),
}

impl MediaSource {
    /// Deserialize from JSON: `{source:"url|file_id|path", value:"..."}` or a bare string
    /// (heuristic: http(s) → URL, existing path → Path, else FileId).
    pub fn from_json(v: &serde_json::Value) -> anyhow::Result<Self> {
        if let Some(s) = v.as_str() {
            return Ok(Self::from_str_heuristic(s));
        }
        let obj = v
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("media source must be string or object"))?;
        let source = obj.get("source").and_then(|v| v.as_str()).unwrap_or("auto");
        let value = obj
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("media source missing `value`"))?;
        match source {
            "url" => Ok(Self::Url(value.to_string())),
            "file_id" => Ok(Self::FileId(value.to_string())),
            "path" => Ok(Self::Path(PathBuf::from(value))),
            "auto" => Ok(Self::from_str_heuristic(value)),
            other => Err(anyhow::anyhow!("unknown media source `{other}`")),
        }
    }

    fn from_str_heuristic(s: &str) -> Self {
        if s.starts_with("http://") || s.starts_with("https://") {
            Self::Url(s.to_string())
        } else if Path::new(s).exists() {
            Self::Path(PathBuf::from(s))
        } else {
            Self::FileId(s.to_string())
        }
    }
}

/// We keep the bot token in a separate field so `base` / `file_base`
/// stay empty of secrets in Debug formatters. `Debug` is manually
/// implemented to redact it — accidental `{:?}` logs still leak the
/// URL shape but never the token.
///
/// `circuit` wraps every outbound HTTP call to the Telegram API.
/// One breaker per `BotClient` instance — when a deployment runs
/// multiple bots (multi-tenant), each gets its own breaker so a
/// single bad token doesn't cascade across tenants. FOLLOWUPS H-1.
#[derive(Clone)]
pub struct BotClient {
    http: Client,
    /// Root host without the `/bot<token>` suffix — safe to include in
    /// user-facing logs.
    host_root: String,
    /// The token itself. Never included in `Debug` or error messages.
    token: String,
    /// CircuitBreaker shared across every API call this client makes.
    /// Trips after `failure_threshold` consecutive failures (defaults
    /// to 3 per `CircuitBreakerConfig::default()`); reopens after
    /// `success_threshold` consecutive successes.
    circuit: Arc<nexo_resilience::CircuitBreaker>,
}

impl std::fmt::Debug for BotClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotClient")
            .field("host_root", &self.host_root)
            .field("token", &"<redacted>")
            .field("circuit", &self.circuit.name())
            .finish()
    }
}

impl BotClient {
    pub fn new(token: &str, base_override: Option<&str>) -> Self {
        if token.trim().is_empty() {
            tracing::warn!(
                "telegram: bot token is empty — getMe will fail with 404 and the plugin won't start"
            );
        }
        let host_root = base_override
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| "https://api.telegram.org".to_string());
        let http = Client::builder()
            .user_agent("agent-rs-telegram/0.1")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(90))
            .build()
            .expect("reqwest client build");
        // Breaker name carries the redacted host (never the token) so
        // `nexo_resilience` log lines stay safe to scrape.
        let breaker_name = format!("telegram.{}", redact_host(&host_root));
        let circuit = Arc::new(nexo_resilience::CircuitBreaker::new(
            breaker_name,
            nexo_resilience::CircuitBreakerConfig::default(),
        ));
        Self {
            http,
            host_root,
            token: token.to_string(),
            circuit,
        }
    }

    fn api_base(&self) -> String {
        format!("{}/bot{}", self.host_root, self.token)
    }

    fn file_base(&self) -> String {
        format!("{}/file/bot{}", self.host_root, self.token)
    }

    /// Safe, redacted host for logs / status output. Never contains the
    /// bot token. Use this anywhere the URL might end up in a user-
    /// facing log or exported metric label.
    pub fn endpoint_host(&self) -> String {
        self.host_root.clone()
    }

    pub async fn get_me(&self) -> anyhow::Result<GetMeResponse> {
        self.call_json("getMe", &json!({})).await
    }

    pub async fn get_updates(
        &self,
        offset: i64,
        timeout_secs: u64,
        allowed_updates: &[&str],
    ) -> anyhow::Result<Vec<Update>> {
        let url = format!("{}/getUpdates", self.api_base());
        let resp = self
            .http
            .get(url)
            .timeout(Duration::from_secs(timeout_secs + 10))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", timeout_secs.to_string()),
                ("allowed_updates", serde_json::to_string(allowed_updates)?),
            ])
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("getUpdates HTTP {status}: {text}");
        }
        let parsed: ApiEnvelope<Vec<Update>> = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parse getUpdates: {e}: {text}"))?;
        parsed.unwrap("getUpdates")
    }

    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
    ) -> anyhow::Result<SendMessageResponse> {
        self.send_message_full(chat_id, text, reply_to_message_id, None, None)
            .await
    }

    pub async fn send_message_full(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        parse_mode: Option<&str>,
        reply_markup: Option<&serde_json::Value>,
    ) -> anyhow::Result<SendMessageResponse> {
        let mut body = json!({ "chat_id": chat_id, "text": text });
        if let Some(rid) = reply_to_message_id {
            body["reply_to_message_id"] = json!(rid);
        }
        if let Some(pm) = parse_mode {
            body["parse_mode"] = json!(normalize_parse_mode(pm).map_err(|e| anyhow::anyhow!(e))?);
        }
        if let Some(rm) = reply_markup {
            body["reply_markup"] = rm.clone();
        }
        self.call_json("sendMessage", &body).await
    }

    pub async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        parse_mode: Option<&str>,
    ) -> anyhow::Result<SendMessageResponse> {
        let mut body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
        });
        if let Some(pm) = parse_mode {
            body["parse_mode"] = json!(normalize_parse_mode(pm).map_err(|e| anyhow::anyhow!(e))?);
        }
        self.call_json("editMessageText", &body).await
    }

    pub async fn set_message_reaction(
        &self,
        chat_id: i64,
        message_id: i64,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "reaction": [{"type":"emoji","emoji": emoji}],
        });
        let _: serde_json::Value = self.call_json("setMessageReaction", &body).await?;
        Ok(())
    }

    /// ACK an inline-keyboard button press so Telegram stops showing
    /// the loading spinner on the button. `text` (optional) renders
    /// as a small toast on the user's device. `show_alert=true` turns
    /// it into a full modal the user must dismiss.
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> anyhow::Result<()> {
        let mut body = json!({ "callback_query_id": callback_query_id });
        if let Some(t) = text {
            body["text"] = json!(t);
        }
        if show_alert {
            body["show_alert"] = json!(true);
        }
        let _: serde_json::Value = self.call_json("answerCallbackQuery", &body).await?;
        Ok(())
    }

    pub async fn send_chat_action(&self, chat_id: i64, action: &str) -> anyhow::Result<()> {
        let _: serde_json::Value = self
            .call_json(
                "sendChatAction",
                &json!({ "chat_id": chat_id, "action": action }),
            )
            .await?;
        Ok(())
    }

    pub async fn send_location(
        &self,
        chat_id: i64,
        latitude: f64,
        longitude: f64,
    ) -> anyhow::Result<SendMessageResponse> {
        self.call_json(
            "sendLocation",
            &json!({
                "chat_id": chat_id,
                "latitude": latitude,
                "longitude": longitude,
            }),
        )
        .await
    }

    pub async fn send_photo(
        &self,
        chat_id: i64,
        source: &MediaSource,
        caption: Option<&str>,
        parse_mode: Option<&str>,
    ) -> anyhow::Result<SendMessageResponse> {
        self.send_media(
            chat_id,
            "sendPhoto",
            "photo",
            source,
            caption,
            parse_mode,
            &[],
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_audio(
        &self,
        chat_id: i64,
        source: &MediaSource,
        caption: Option<&str>,
        title: Option<&str>,
        performer: Option<&str>,
        duration: Option<u64>,
        parse_mode: Option<&str>,
    ) -> anyhow::Result<SendMessageResponse> {
        let mut extra: Vec<(&str, String)> = Vec::new();
        if let Some(t) = title {
            extra.push(("title", t.to_string()));
        }
        if let Some(p) = performer {
            extra.push(("performer", p.to_string()));
        }
        if let Some(d) = duration {
            extra.push(("duration", d.to_string()));
        }
        let extra_ref: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.send_media(
            chat_id,
            "sendAudio",
            "audio",
            source,
            caption,
            parse_mode,
            &extra_ref,
        )
        .await
    }

    pub async fn send_voice(
        &self,
        chat_id: i64,
        source: &MediaSource,
        caption: Option<&str>,
        duration: Option<u64>,
        parse_mode: Option<&str>,
    ) -> anyhow::Result<SendMessageResponse> {
        let dur_s;
        let mut extra: Vec<(&str, &str)> = Vec::new();
        if let Some(d) = duration {
            dur_s = d.to_string();
            extra.push(("duration", &dur_s));
        }
        self.send_media(
            chat_id,
            "sendVoice",
            "voice",
            source,
            caption,
            parse_mode,
            &extra,
        )
        .await
    }

    pub async fn send_video(
        &self,
        chat_id: i64,
        source: &MediaSource,
        caption: Option<&str>,
        duration: Option<u64>,
        parse_mode: Option<&str>,
    ) -> anyhow::Result<SendMessageResponse> {
        let dur_s;
        let mut extra: Vec<(&str, &str)> = Vec::new();
        if let Some(d) = duration {
            dur_s = d.to_string();
            extra.push(("duration", &dur_s));
        }
        self.send_media(
            chat_id,
            "sendVideo",
            "video",
            source,
            caption,
            parse_mode,
            &extra,
        )
        .await
    }

    pub async fn send_document(
        &self,
        chat_id: i64,
        source: &MediaSource,
        caption: Option<&str>,
        parse_mode: Option<&str>,
    ) -> anyhow::Result<SendMessageResponse> {
        self.send_media(
            chat_id,
            "sendDocument",
            "document",
            source,
            caption,
            parse_mode,
            &[],
        )
        .await
    }

    pub async fn send_animation(
        &self,
        chat_id: i64,
        source: &MediaSource,
        caption: Option<&str>,
        parse_mode: Option<&str>,
    ) -> anyhow::Result<SendMessageResponse> {
        self.send_media(
            chat_id,
            "sendAnimation",
            "animation",
            source,
            caption,
            parse_mode,
            &[],
        )
        .await
    }

    /// Shared backbone: URL / file_id variants go as JSON, local paths as multipart.
    #[allow(clippy::too_many_arguments)]
    async fn send_media(
        &self,
        chat_id: i64,
        endpoint: &str,
        field: &str,
        source: &MediaSource,
        caption: Option<&str>,
        parse_mode: Option<&str>,
        extras: &[(&str, &str)],
    ) -> anyhow::Result<SendMessageResponse> {
        match source {
            MediaSource::Url(u) => {
                let mut body = json!({ "chat_id": chat_id, field: u });
                if let Some(c) = caption {
                    body["caption"] = json!(truncate_utf16(c, MAX_CAPTION_LEN));
                }
                if let Some(pm) = parse_mode {
                    body["parse_mode"] =
                        json!(normalize_parse_mode(pm).map_err(|e| anyhow::anyhow!(e))?);
                }
                for (k, v) in extras {
                    body[*k] = json!(v);
                }
                self.call_json(endpoint, &body).await
            }
            MediaSource::FileId(fid) => {
                let mut body = json!({ "chat_id": chat_id, field: fid });
                if let Some(c) = caption {
                    body["caption"] = json!(truncate_utf16(c, MAX_CAPTION_LEN));
                }
                if let Some(pm) = parse_mode {
                    body["parse_mode"] =
                        json!(normalize_parse_mode(pm).map_err(|e| anyhow::anyhow!(e))?);
                }
                for (k, v) in extras {
                    body[*k] = json!(v);
                }
                self.call_json(endpoint, &body).await
            }
            MediaSource::Path(p) => {
                // Stream the file directly from disk into the multipart
                // body instead of buffering the whole thing in RAM.
                // Telegram Bot API accepts uploads up to 50 MB — a video
                // at that size would otherwise burn 50 MB of memory per
                // concurrent send. With stream, we only hold a small
                // chunk at a time.
                let filename = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file.bin")
                    .to_string();
                let metadata = tokio::fs::metadata(p)
                    .await
                    .map_err(|e| anyhow::anyhow!("stat {}: {e}", p.display()))?;
                let file_size = metadata.len();
                let file = tokio::fs::File::open(p)
                    .await
                    .map_err(|e| anyhow::anyhow!("open {}: {e}", p.display()))?;
                let reader = tokio_util::io::ReaderStream::new(file);
                let body = reqwest::Body::wrap_stream(reader);
                let part = Part::stream_with_length(body, file_size).file_name(filename);
                let mut form = Form::new()
                    .text("chat_id", chat_id.to_string())
                    .part(field.to_string(), part);
                if let Some(c) = caption {
                    form = form.text("caption", truncate_utf16(c, MAX_CAPTION_LEN));
                }
                if let Some(pm) = parse_mode {
                    let normalized = normalize_parse_mode(pm).map_err(|e| anyhow::anyhow!(e))?;
                    form = form.text("parse_mode", normalized.to_string());
                }
                for (k, v) in extras {
                    form = form.text(k.to_string(), v.to_string());
                }
                let url = format!("{}/{endpoint}", self.api_base());
                let text = self
                    .run_breakered(|| async {
                        let resp = self.http.post(url).multipart(form).send().await?;
                        let status = resp.status();
                        let text = resp.text().await?;
                        if !status.is_success() {
                            anyhow::bail!("{endpoint} HTTP {status}: {text}");
                        }
                        Ok::<String, anyhow::Error>(text)
                    })
                    .await?;
                let parsed: ApiEnvelope<SendMessageResponse> = serde_json::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("parse {endpoint}: {e}: {text}"))?;
                parsed.unwrap(endpoint)
            }
        }
    }

    pub async fn get_file(&self, file_id: &str) -> anyhow::Result<FileInfo> {
        self.call_json("getFile", &json!({ "file_id": file_id }))
            .await
    }

    /// Stream a file to `dest`. Returns bytes written.
    pub async fn download_file(&self, file_path: &str, dest: &Path) -> anyhow::Result<u64> {
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let url = format!("{}/{file_path}", self.file_base());
        let dest = dest.to_path_buf();
        self.run_breakered(|| async move {
            let resp = self.http.get(url).send().await?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("download HTTP {status}: {body}");
            }
            let mut file = tokio::fs::File::create(&dest).await?;
            let mut stream = resp.bytes_stream();
            let mut total: u64 = 0;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                file.write_all(&chunk).await?;
                total += chunk.len() as u64;
            }
            file.flush().await?;
            Ok::<u64, anyhow::Error>(total)
        })
        .await
    }

    async fn call_json<T: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<T> {
        let url = format!("{}/{endpoint}", self.api_base());
        let text = self
            .run_breakered(|| async {
                let resp = self.http.post(url).json(body).send().await?;
                let status = resp.status();
                let text = resp.text().await?;
                if !status.is_success() {
                    anyhow::bail!("{endpoint} HTTP {status}: {text}");
                }
                Ok::<String, anyhow::Error>(text)
            })
            .await?;
        let parsed: ApiEnvelope<T> = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parse {endpoint}: {e}: {text}"))?;
        parsed.unwrap(endpoint)
    }

    /// Wrap an HTTP-issuing async closure with the per-client
    /// CircuitBreaker. `Open` short-circuits with a clear "breaker
    /// open" error; transport / 4xx / 5xx errors flow through
    /// unchanged but trip the failure counter so a sustained burst
    /// opens the breaker and stops hammering Telegram.
    /// FOLLOWUPS H-1.
    async fn run_breakered<F, Fut, T>(&self, op: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        match self.circuit.call(op).await {
            Ok(v) => Ok(v),
            Err(nexo_resilience::CircuitError::Open(name)) => {
                anyhow::bail!("telegram circuit breaker open ({name})")
            }
            Err(nexo_resilience::CircuitError::Inner(e)) => Err(e),
        }
    }
}

/// Strip path from a hostname so a breaker name like
/// `telegram.https://api.telegram.org` collapses to
/// `telegram.api.telegram.org`. Never includes the bot token.
fn redact_host(host_root: &str) -> String {
    host_root
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(host_root)
        .to_string()
}

/// Length in UTF-16 code units (what Telegram actually measures against
/// its wire caps). A supplementary-plane emoji is 1 `char` but 2 UTF-16
/// units, so `chars().count()` underestimates for emoji-heavy text.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

pub(crate) fn truncate_utf16(s: &str, max: usize) -> String {
    if utf16_len(s) <= max {
        return s.to_string();
    }
    tracing::warn!(
        original_utf16_units = utf16_len(s),
        max,
        "telegram: caption/text truncated to fit per-message limit (visible ellipsis added)"
    );
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut used: usize = 0;
    for ch in s.chars() {
        let w = ch.len_utf16();
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Split `text` into Telegram-safe segments measured in UTF-16 code
/// units. Prefers newline splits, hard-splits otherwise.
pub fn split_text(text: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_units: usize = 0;
    for line in text.split_inclusive('\n') {
        let line_units = utf16_len(line);
        if line_units > max {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                // cur_units gets overwritten a few lines down when we
                // assign the hard-split buffer back into `cur`, so no
                // need to reset it here.
            }
            let mut buf = String::new();
            let mut buf_units: usize = 0;
            for ch in line.chars() {
                let w = ch.len_utf16();
                if buf_units + w > max {
                    out.push(std::mem::take(&mut buf));
                    buf_units = 0;
                }
                buf.push(ch);
                buf_units += w;
            }
            cur = buf;
            cur_units = buf_units;
            continue;
        }
        if cur_units + line_units > max {
            out.push(std::mem::take(&mut cur));
            cur_units = 0;
        }
        cur.push_str(line);
        cur_units += line_units;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    error_code: Option<i64>,
}

impl<T> ApiEnvelope<T> {
    fn unwrap(self, ctx: &str) -> anyhow::Result<T> {
        if !self.ok {
            return Err(anyhow::anyhow!(
                "{ctx} failed: code={:?} description={:?}",
                self.error_code,
                self.description
            ));
        }
        self.result
            .ok_or_else(|| anyhow::anyhow!("{ctx} response had no `result`"))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub edited_message: Option<Message>,
    #[serde(default)]
    pub channel_post: Option<Message>,
    /// Inline-keyboard button press. Must be acknowledged with
    /// `answerCallbackQuery` or Telegram keeps a loading spinner on
    /// the button forever.
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
    /// Bot membership status change in a chat (added/removed/kicked,
    /// user starts/stops the DM, user blocks the bot). Telegram only
    /// delivers this when `my_chat_member` is in `allowed_updates`.
    #[serde(default)]
    pub my_chat_member: Option<ChatMemberUpdated>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMemberUpdated {
    pub chat: Chat,
    pub from: TelegramUser,
    pub date: i64,
    pub old_chat_member: ChatMember,
    pub new_chat_member: ChatMember,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMember {
    /// One of `creator`, `administrator`, `member`, `restricted`,
    /// `left`, `kicked`. The transition old→new tells us what happened.
    pub status: String,
    pub user: TelegramUser,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CallbackQuery {
    pub id: String,
    #[serde(default)]
    pub from: Option<TelegramUser>,
    #[serde(default)]
    pub message: Option<Box<Message>>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub inline_message_id: Option<String>,
    #[serde(default)]
    pub chat_instance: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub message_id: i64,
    pub date: i64,
    pub chat: Chat,
    #[serde(default)]
    pub from: Option<TelegramUser>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub reply_to_message: Option<Box<Message>>,
    /// Forwarding metadata — populated when the incoming message is a
    /// forward. Bot API >= 7.0 emits `forward_origin` as a tagged enum;
    /// older versions used `forward_from` / `forward_from_chat`. We
    /// keep the legacy fields parseable for backward compat and expose
    /// `forward_origin` for clients on newer API versions.
    #[serde(default)]
    pub forward_from: Option<TelegramUser>,
    #[serde(default)]
    pub forward_from_chat: Option<Chat>,
    #[serde(default)]
    pub forward_date: Option<i64>,
    #[serde(default)]
    pub forward_origin: Option<serde_json::Value>,
    #[serde(default)]
    pub photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    pub voice: Option<Voice>,
    #[serde(default)]
    pub audio: Option<Audio>,
    #[serde(default)]
    pub video: Option<Video>,
    #[serde(default)]
    pub video_note: Option<VideoNote>,
    #[serde(default)]
    pub animation: Option<Animation>,
    #[serde(default)]
    pub document: Option<Document>,
    #[serde(default)]
    pub sticker: Option<Sticker>,
    #[serde(default)]
    pub location: Option<Location>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TelegramUser {
    pub id: i64,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Voice {
    pub file_id: String,
    pub file_unique_id: String,
    pub duration: u32,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Audio {
    pub file_id: String,
    pub file_unique_id: String,
    pub duration: u32,
    #[serde(default)]
    pub performer: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Video {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: u32,
    pub height: u32,
    pub duration: u32,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VideoNote {
    pub file_id: String,
    pub file_unique_id: String,
    pub length: u32,
    pub duration: u32,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Animation {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: u32,
    pub height: u32,
    pub duration: u32,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Document {
    pub file_id: String,
    pub file_unique_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Sticker {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub is_animated: bool,
    #[serde(default)]
    pub is_video: bool,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Deserialize)]
pub struct GetMeResponse {
    pub id: i64,
    pub is_bot: bool,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageResponse {
    pub message_id: i64,
    pub chat: Chat,
}

#[derive(Debug, Deserialize)]
pub struct FileInfo {
    pub file_id: String,
    pub file_unique_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_short_intact() {
        let out = split_text("hola", 100);
        assert_eq!(out, vec!["hola"]);
    }

    #[test]
    fn split_breaks_at_newlines() {
        let s = "aaaa\nbbbb\ncccc\ndddd\n";
        let out = split_text(s, 10);
        for seg in &out {
            assert!(utf16_len(seg) <= 10, "segment too long: {seg:?}");
        }
        assert_eq!(out.concat(), s);
    }

    #[test]
    fn split_hard_breaks_oversize_line() {
        let long_line = "x".repeat(25);
        let out = split_text(&long_line, 10);
        assert!(out.iter().all(|s| utf16_len(s) <= 10));
        assert_eq!(out.concat(), long_line);
    }

    #[test]
    fn split_counts_utf16_units_for_emoji() {
        // Each "🎉" is 1 char Rust but 2 UTF-16 code units (supplementary plane).
        // With max=4, we should fit at most 2 emojis per segment.
        let s = "🎉🎉🎉🎉".to_string();
        let out = split_text(&s, 4);
        assert!(out.iter().all(|seg| utf16_len(seg) <= 4));
        assert_eq!(out.concat(), s);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn truncate_respects_utf16_budget() {
        let s = "🎉🎉🎉";
        let out = truncate_utf16(s, 4);
        assert!(utf16_len(&out) <= 4);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn media_source_url_string() {
        let s = MediaSource::from_json(&json!("https://x.com/a.jpg")).unwrap();
        assert!(matches!(s, MediaSource::Url(_)));
    }

    #[test]
    fn media_source_object_file_id() {
        let s = MediaSource::from_json(&json!({"source":"file_id","value":"AgAD..."})).unwrap();
        assert!(matches!(s, MediaSource::FileId(_)));
    }

    #[test]
    fn parse_mode_normalizes_common_typos() {
        assert_eq!(normalize_parse_mode("MarkdownV2").unwrap(), "MarkdownV2");
        assert_eq!(normalize_parse_mode("markdownv2").unwrap(), "MarkdownV2");
        assert_eq!(normalize_parse_mode("mdv2").unwrap(), "MarkdownV2");
        assert_eq!(normalize_parse_mode("markdown").unwrap(), "Markdown");
        assert_eq!(normalize_parse_mode("HTML").unwrap(), "HTML");
        assert_eq!(normalize_parse_mode("html").unwrap(), "HTML");
        let err = normalize_parse_mode("richtext").unwrap_err();
        assert!(err.contains("unsupported parse_mode"));
    }

    #[test]
    fn callback_query_deserialize() {
        let raw = json!({
            "update_id": 1,
            "callback_query": {
                "id": "cq_1",
                "from": {"id": 42, "is_bot": false, "first_name": "Alice"},
                "message": {
                    "message_id": 99,
                    "date": 1700000000,
                    "chat": {"id": -100, "type": "supergroup"}
                },
                "data": "yes",
                "chat_instance": "ci_1"
            }
        });
        let upd: Update = serde_json::from_value(raw).unwrap();
        let cq = upd.callback_query.unwrap();
        assert_eq!(cq.id, "cq_1");
        assert_eq!(cq.data.as_deref(), Some("yes"));
        assert_eq!(cq.message.unwrap().message_id, 99);
    }

    #[test]
    fn forward_origin_parses_into_message() {
        let raw = json!({
            "message_id": 12,
            "date": 1700000000,
            "chat": {"id": 5, "type": "private"},
            "forward_origin": {
                "type": "user",
                "date": 1699999900,
                "sender_user": {"id": 77, "is_bot": false, "first_name": "Bob"}
            },
            "text": "old message"
        });
        let msg: Message = serde_json::from_value(raw).unwrap();
        assert!(msg.forward_origin.is_some());
        let o = msg.forward_origin.unwrap();
        assert_eq!(o["type"], "user");
        assert_eq!(o["sender_user"]["id"], 77);
    }
}
