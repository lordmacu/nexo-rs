//! Outbound and inbound media helpers.
//!
//! * **Outbound** — a `SendMedia { to, url, caption }` command lands on
//!   `plugin.outbound.whatsapp`; the dispatcher calls
//!   [`download_from_url`] to fetch the bytes and
//!   [`send_media_auto`] to pick the right `Session::send_*` variant
//!   based on the content MIME.
//! * **Inbound** — when the bridge handler sees a `MessageContent::{Image,
//!   Video, Audio, Document, Sticker}`, it calls
//!   [`download_inbound`] which fetches bytes, writes them to
//!   `cfg.media_dir/{msg_id}.{ext}`, and publishes an
//!   `InboundEvent::MediaReceived` alongside the normal text event.
//!
//! MIME sniffing keeps the routing logic pure (unit-testable) — actual
//! network IO stays behind the `download_*` functions.

use std::path::PathBuf;

use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_config::WhatsappPluginConfig;
use anyhow::{Context, Result};

use crate::bridge::SOURCE;
use crate::events::InboundEvent;
use crate::session_id::session_id_for_jid;

/// Which `Session::send_*` call a given MIME should go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaVariant {
    Image,
    Video,
    Audio,
    VoiceNote,
    Document,
}

impl MediaVariant {
    /// Map a MIME type to a variant. Unknown mimes fall through to
    /// `Document` so the user still gets the file.
    pub fn from_mime(mime: &str) -> Self {
        let main = mime.split('/').next().unwrap_or("");
        match main {
            "image" => Self::Image,
            "video" => Self::Video,
            "audio" => {
                // OGG/opus with the WhatsApp-native codec path is the
                // only consumer-side voice-note shape. Everything else
                // (mp3, wav, m4a) ships as a regular audio message.
                if mime.eq_ignore_ascii_case("audio/ogg; codecs=opus")
                    || mime.eq_ignore_ascii_case("audio/ogg;codecs=opus")
                    || mime.eq_ignore_ascii_case("audio/opus")
                {
                    Self::VoiceNote
                } else {
                    Self::Audio
                }
            }
            _ => Self::Document,
        }
    }

    /// File extension we append when writing inbound media to disk.
    pub fn default_ext(self) -> &'static str {
        match self {
            Self::Image => "jpg",
            Self::Video => "mp4",
            Self::Audio => "m4a",
            Self::VoiceNote => "ogg",
            Self::Document => "bin",
        }
    }
}

/// Map a wa-agent `MessageContent` variant tag to [`MediaVariant`] and
/// expose the associated `media::MediaType`, since the crate uses two
/// related enums.
pub(crate) fn variant_of_content(
    content: &whatsapp_rs::MessageContent,
) -> Option<(
    MediaVariant,
    whatsapp_rs::media::MediaType,
    String, /*mime*/
)> {
    use whatsapp_rs::MessageContent as C;
    match content {
        C::Image { info, .. } => Some((
            MediaVariant::Image,
            whatsapp_rs::media::MediaType::Image,
            info.mimetype.clone(),
        )),
        C::Video { info, .. } => Some((
            MediaVariant::Video,
            whatsapp_rs::media::MediaType::Video,
            info.mimetype.clone(),
        )),
        C::Audio { info, .. } => Some((
            MediaVariant::from_mime(&info.mimetype),
            whatsapp_rs::media::MediaType::Audio,
            info.mimetype.clone(),
        )),
        C::Document { info, .. } => Some((
            MediaVariant::Document,
            whatsapp_rs::media::MediaType::Document,
            info.mimetype.clone(),
        )),
        C::Sticker { info } => Some((
            MediaVariant::Image,
            whatsapp_rs::media::MediaType::Sticker,
            info.mimetype.clone(),
        )),
        _ => None,
    }
}

/// Extract an optional caption from a media content variant. Non-media
/// or uncaptioned variants return `None`.
pub(crate) fn caption_of(content: &whatsapp_rs::MessageContent) -> Option<String> {
    use whatsapp_rs::MessageContent as C;
    match content {
        C::Image { caption, .. } => caption.clone(),
        C::Video { caption, .. } => caption.clone(),
        _ => None,
    }
}

/// Extract the original file name for a `Document` variant.
#[allow(dead_code)]
pub(crate) fn file_name_of(content: &whatsapp_rs::MessageContent) -> Option<String> {
    if let whatsapp_rs::MessageContent::Document { file_name, .. } = content {
        Some(file_name.clone())
    } else {
        None
    }
}

/// Fetch arbitrary URL → bytes. `Content-Type` is returned alongside so
/// the caller can route to the right send variant. Oversized responses
/// are rejected early to avoid OOM on hostile URLs.
pub async fn download_from_url(url: &str, max_bytes: usize) -> Result<(Vec<u8>, String)> {
    let resp = reqwest::get(url)
        .await
        .with_context(|| format!("GET {url} failed"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("GET {url} → HTTP {status}");
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            anyhow::bail!("GET {url}: content-length {len} exceeds cap {max_bytes}");
        }
    }
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("reading body from {url}"))?;
    if bytes.len() > max_bytes {
        anyhow::bail!("GET {url}: {} bytes exceeds cap {max_bytes}", bytes.len());
    }
    Ok((bytes.to_vec(), mime))
}

/// Dispatch bytes → the matching `Session::send_*` call.
pub async fn send_media_auto(
    session: &whatsapp_rs::Session,
    to: &str,
    bytes: &[u8],
    mime: &str,
    caption: Option<&str>,
    file_name: Option<&str>,
) -> Result<()> {
    let variant = MediaVariant::from_mime(mime);
    match variant {
        MediaVariant::Image => {
            session
                .send_image(to, bytes, caption)
                .await
                .map_err(|e| anyhow::anyhow!("send_image: {e}"))?;
        }
        MediaVariant::Video => {
            session
                .send_video(to, bytes, caption)
                .await
                .map_err(|e| anyhow::anyhow!("send_video: {e}"))?;
        }
        MediaVariant::Audio => {
            session
                .send_audio(to, bytes, mime)
                .await
                .map_err(|e| anyhow::anyhow!("send_audio: {e}"))?;
        }
        MediaVariant::VoiceNote => {
            session
                .send_voice_note(to, bytes, mime)
                .await
                .map_err(|e| anyhow::anyhow!("send_voice_note: {e}"))?;
        }
        MediaVariant::Document => {
            let name = file_name.unwrap_or("file.bin");
            session
                .send_document(to, bytes, mime, name)
                .await
                .map_err(|e| anyhow::anyhow!("send_document: {e}"))?;
        }
    }
    Ok(())
}

/// Download an inbound media message to disk and publish a
/// `MediaReceived` event. Safe to call speculatively — non-media
/// `MessageContent` variants are no-ops.
pub async fn download_inbound(
    session: &whatsapp_rs::Session,
    broker: &AnyBroker,
    cfg: &WhatsappPluginConfig,
    msg: &whatsapp_rs::WAMessage,
) -> Result<()> {
    let Some(content) = msg.message.as_ref() else {
        return Ok(());
    };
    let Some((variant, media_type, mime)) = variant_of_content(content) else {
        return Ok(());
    };

    let info = match content {
        whatsapp_rs::MessageContent::Image { info, .. }
        | whatsapp_rs::MessageContent::Video { info, .. }
        | whatsapp_rs::MessageContent::Audio { info, .. }
        | whatsapp_rs::MessageContent::Document { info, .. }
        | whatsapp_rs::MessageContent::Sticker { info } => info,
        _ => return Ok(()),
    };

    let bytes = session
        .download_media(info, media_type)
        .await
        .map_err(|e| anyhow::anyhow!("download_media: {e}"))?;

    let dir = PathBuf::from(&cfg.media_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let ext = variant.default_ext();
    let path = dir.join(format!("{}.{}", sanitize(&msg.key.id), ext));
    // Atomic write: stage to `<path>.tmp` then rename. Without this,
    // a crash mid-write leaves a truncated file at `path`. Future
    // dedup checks see the file exists and skip the (now-incomplete)
    // download forever.
    let tmp = path.with_extension(format!("{ext}.tmp"));
    std::fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;

    let session_id = session_id_for_jid(&msg.key.remote_jid);
    let ev = InboundEvent::MediaReceived {
        from: msg
            .key
            .participant
            .clone()
            .unwrap_or_else(|| msg.key.remote_jid.clone()),
        chat: msg.key.remote_jid.clone(),
        msg_id: msg.key.id.clone(),
        local_path: path,
        mime,
        caption: caption_of(content),
    };
    let inbound_topic = crate::bridge::inbound_topic_for(cfg.instance.as_deref());
    let mut event = Event::new(&inbound_topic, SOURCE, ev.to_payload());
    event.session_id = Some(session_id);
    let _ = broker.publish(&inbound_topic, event).await;
    Ok(())
}

/// Prevent path traversal from a WA-provided message id landing in the
/// file system. WA ids are typically alphanumeric but belt-and-braces.
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_routes_image_video_audio() {
        assert_eq!(MediaVariant::from_mime("image/jpeg"), MediaVariant::Image);
        assert_eq!(MediaVariant::from_mime("image/png"), MediaVariant::Image);
        assert_eq!(MediaVariant::from_mime("video/mp4"), MediaVariant::Video);
        assert_eq!(MediaVariant::from_mime("audio/mpeg"), MediaVariant::Audio);
    }

    #[test]
    fn voice_note_recognised() {
        assert_eq!(
            MediaVariant::from_mime("audio/ogg; codecs=opus"),
            MediaVariant::VoiceNote
        );
        assert_eq!(
            MediaVariant::from_mime("audio/ogg;codecs=opus"),
            MediaVariant::VoiceNote
        );
        assert_eq!(
            MediaVariant::from_mime("audio/opus"),
            MediaVariant::VoiceNote
        );
    }

    #[test]
    fn unknown_mime_falls_through_to_document() {
        assert_eq!(
            MediaVariant::from_mime("application/pdf"),
            MediaVariant::Document
        );
        assert_eq!(MediaVariant::from_mime(""), MediaVariant::Document);
        assert_eq!(
            MediaVariant::from_mime("weird/format"),
            MediaVariant::Document
        );
    }

    #[test]
    fn default_ext_matches_variant() {
        assert_eq!(MediaVariant::Image.default_ext(), "jpg");
        assert_eq!(MediaVariant::Video.default_ext(), "mp4");
        assert_eq!(MediaVariant::VoiceNote.default_ext(), "ogg");
    }

    #[test]
    fn sanitize_blocks_traversal() {
        assert_eq!(sanitize("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize("3EB0ABCD-42_a"), "3EB0ABCD-42_a");
    }
}
