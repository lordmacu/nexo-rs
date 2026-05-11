//! Phase 91.x.wasm.phase-4d — bridge that lets the local Candle
//! Whisper backend participate in a [`CompositeProvider`] chain
//! as the offline fallback leg.
//!
//! The cloud providers (OpenAI / Groq / Anthropic) all speak the
//! [`SttProvider`] trait, which takes an in-memory audio buffer.
//! Local Candle is path-based
//! ([`crate::stt::transcribe_candle::transcribe_file`]) because
//! the shared audio decode chain reads from disk via
//! [`tokio::fs`]. This adapter writes the in-memory buffer to a
//! tempfile with an extension matching the MIME so the decode
//! path recognises the container, then calls Candle.
//!
//! Tempfile cleanup is RAII — the file is removed on drop even
//! if inference panics, so voice notes don't leak onto the
//! operator's disk.
//!
//! Typical fallback chain shape:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use nexo_microapp_sdk::stt::TranscribeConfig;
//! # use nexo_microapp_sdk::stt::cloud::{
//! #     anthropic::AnthropicVoiceStream,
//! #     local_candle::LocalCandleProvider,
//! #     CompositeProvider, SttProvider,
//! # };
//! let cfg = Arc::new(TranscribeConfig {
//!     model_id: Some("openai/whisper-tiny".into()),
//!     ..Default::default()
//! });
//! let chain = CompositeProvider::new(vec![
//!     Box::new(AnthropicVoiceStream::new("oauth_token".into())),
//!     Box::new(LocalCandleProvider::new(cfg)),
//! ]);
//! # let _: Box<dyn SttProvider> = Box::new(chain);
//! ```
//!
//! The Anthropic leg runs first against the public
//! `voice_stream` WebSocket; if the network drops or the OAuth
//! token expires, the chain falls through to local Candle
//! inference. Operators on metered cellular get the cheap path
//! when online and a guaranteed offline backstop.

#![cfg(all(feature = "stt-cloud-wasm", feature = "stt-candle"))]

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use super::SttProvider;
use crate::stt::{transcribe_candle, SttError, TranscribeConfig};

/// Local Candle Whisper inference exposed under the
/// [`SttProvider`] trait so it composes with the cloud legs.
///
/// Construction is cheap — the Candle backend lazy-loads the
/// model on the first transcription call, not at construction.
pub struct LocalCandleProvider {
    cfg: Arc<TranscribeConfig>,
}

impl std::fmt::Debug for LocalCandleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `TranscribeConfig` derives Debug, but the noisy fields
        // (paths, model ids) are exactly what an operator wants
        // to see when a fallback chain trips here. Inline the
        // relevant subset.
        f.debug_struct("LocalCandleProvider")
            .field("model_path", &self.cfg.model_path)
            .field("model_id", &self.cfg.model_id)
            .field("lang_hint", &self.cfg.lang_hint)
            .finish()
    }
}

impl LocalCandleProvider {
    /// Wrap a [`TranscribeConfig`] under the cloud provider
    /// trait. Pass the same `Arc<TranscribeConfig>` you'd hand
    /// to [`crate::stt::InboundTransformHandler::new`] so model
    /// auto-fetch + lang hints stay consistent with the
    /// non-fallback path.
    pub fn new(cfg: Arc<TranscribeConfig>) -> Self {
        Self { cfg }
    }
}

/// Map a MIME prefix to the filename extension the local decode
/// chain recognises. Candle's audio path only decodes ogg-opus
/// today (the format WhatsApp + Telegram voice notes use);
/// anything else returns `None` so the trait impl surfaces a
/// clear [`SttError::UnsupportedFormat`].
fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let lower = mime.to_lowercase();
    if lower.starts_with("audio/ogg") || lower.starts_with("audio/opus") {
        Some("ogg")
    } else {
        None
    }
}

#[async_trait]
impl SttProvider for LocalCandleProvider {
    async fn transcribe(
        &self,
        audio_bytes: Vec<u8>,
        audio_mime: &str,
        lang_hint: Option<&str>,
    ) -> Result<String, SttError> {
        if audio_bytes.is_empty() {
            return Err(SttError::EmptyAudio);
        }
        let ext = extension_for_mime(audio_mime).ok_or_else(|| {
            SttError::UnsupportedFormat(format!(
                "LocalCandleProvider only decodes ogg-opus (got {audio_mime:?}); \
                 transcode upstream or route through a cloud leg that accepts \
                 the source format"
            ))
        })?;

        // RAII tempfile: dropped at end-of-scope so the audio
        // buffer doesn't survive on disk if inference panics or
        // a downstream `await` cancels.
        let tempfile = tempfile::Builder::new()
            .prefix("nexo-stt-candle-")
            .suffix(&format!(".{ext}"))
            .tempfile()
            .map_err(SttError::Io)?;
        let path: PathBuf = tempfile.path().to_path_buf();
        tokio::fs::write(&path, &audio_bytes).await?;

        // Honour a per-request lang hint without mutating the
        // shared cfg — clone on Cow::Owned only if it differs.
        // The Anthropic / cloud legs accept BCP-47 with region
        // (`es-AR`); Candle wants the bare 2-letter code, but
        // `transcribe_candle::transcribe_file` already normalises
        // internally, so pass-through is safe.
        let cfg = if lang_hint.is_some()
            && lang_hint.map(str::to_string) != self.cfg.lang_hint
        {
            let mut c = (*self.cfg).clone();
            c.lang_hint = lang_hint.map(str::to_string);
            Cow::Owned(c)
        } else {
            Cow::Borrowed(self.cfg.as_ref())
        };
        let out = transcribe_candle::transcribe_file(&path, cfg.as_ref()).await?;

        // `tempfile` drops here — file removed.
        Ok(out)
    }

    fn name(&self) -> &'static str {
        "local-candle"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable() {
        let p = LocalCandleProvider::new(Arc::new(TranscribeConfig::default()));
        assert_eq!(p.name(), "local-candle");
    }

    #[test]
    fn debug_surfaces_cfg_state() {
        let cfg = Arc::new(TranscribeConfig {
            lang_hint: Some("es".into()),
            model_id: Some("openai/whisper-tiny".into()),
            ..Default::default()
        });
        let p = LocalCandleProvider::new(cfg);
        let dbg = format!("{p:?}");
        assert!(dbg.contains("LocalCandleProvider"));
        assert!(dbg.contains("\"es\""));
        assert!(dbg.contains("openai/whisper-tiny"));
    }

    #[tokio::test]
    async fn empty_audio_rejected_before_tempfile() {
        let p = LocalCandleProvider::new(Arc::new(TranscribeConfig::default()));
        let err = match p.transcribe(vec![], "audio/ogg", None).await {
            Ok(t) => panic!("expected error, got {t:?}"),
            Err(e) => e,
        };
        assert!(matches!(err, SttError::EmptyAudio));
    }

    #[tokio::test]
    async fn unsupported_mime_rejected() {
        let p = LocalCandleProvider::new(Arc::new(TranscribeConfig::default()));
        let err = match p.transcribe(vec![1, 2, 3], "audio/mp3", None).await {
            Ok(t) => panic!("expected error, got {t:?}"),
            Err(e) => e,
        };
        assert!(matches!(err, SttError::UnsupportedFormat(_)));
    }

    #[test]
    fn extension_picker_matrix() {
        assert_eq!(extension_for_mime("audio/ogg"), Some("ogg"));
        assert_eq!(extension_for_mime("audio/ogg; codecs=opus"), Some("ogg"));
        assert_eq!(extension_for_mime("AUDIO/OGG"), Some("ogg"));
        assert_eq!(extension_for_mime("audio/opus"), Some("ogg"));
        assert_eq!(extension_for_mime("audio/mp3"), None);
        assert_eq!(extension_for_mime("audio/L16; rate=16000"), None);
        assert_eq!(extension_for_mime(""), None);
    }
}
