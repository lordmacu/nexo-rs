//! Anthropic `voice_stream` STT
//! WebSocket client.
//!
//! Wire reference: `claude-code-leak/src/services/voiceStreamSTT.ts`
//! captures the live protocol the official Claude Code client
//! speaks to Anthropic's internal STT endpoint:
//!
//! - **URL**: `wss://api.anthropic.com/api/ws/speech_to_text/voice_stream`
//! - **Query params**: `encoding=linear16`, `sample_rate=16000`,
//!   `channels=1`, `endpointing_ms=300`, `utterance_end_ms=1000`,
//!   `language=<bcp47>`, optional repeated `keyterms=<term>`,
//!   `use_conversation_engine=true`,
//!   `stt_provider=deepgram-nova3` (gated server-side by the
//!   `tengu_cobalt_frost` GrowthBook flag).
//! - **Auth**: Bearer OAuth token (same one Claude Code uses —
//!   `nexo-llm-auth` resolves it for nexo-rs operators).
//! - **JSON control client → server**:
//!   - `{"type":"KeepAlive"}` every 8 s.
//!   - `{"type":"CloseStream"}` to finalize.
//! - **Binary frames client → server**: PCM s16 LE @ 16 kHz mono.
//! - **JSON server → client**:
//!   - `{"type":"TranscriptText","data":"..."}` — interim,
//!     cumulative on Nova 3.
//!   - `{"type":"TranscriptEndpoint"}` — utterance boundary
//!     marker (commit interim as final segment).
//!   - `{"type":"TranscriptError","error_code":"...","description":"..."}`.
//!
//! ## v1 simplifications
//!
//! The `claude-code-leak` reference implements a full live
//! push-to-talk experience: streaming microphone chunks,
//! interim transcripts surfaced to the UI, auto-finalize when a
//! new segment is detected mid-stream. Our consumers feed a
//! complete voice-note buffer in one call (WhatsApp / Telegram
//! send the audio as a single .ogg attachment) so we collapse
//! the streaming surface into a one-shot `transcribe` call:
//!
//! 1. Open the WebSocket.
//! 2. Send an initial `KeepAlive` (the connection times out
//!    quickly if we sit idle waiting for audio).
//! 3. Send the **already-decoded** PCM buffer as one binary
//!    frame (provider rejects clips > 30 s; matches the
//!    server-side limit).
//! 4. Send `CloseStream`.
//! 5. Drain server messages until the finalize state machine
//!    resolves on one of the four triggers:
//!    - `TranscriptEndpoint` post-CloseStream (~300 ms — happy
//!      path).
//!    - `no_data_timeout` (1.5 s — silent-drop guard against
//!      the anthropics/anthropic#287008 regression).
//!    - `safety_timeout` (5 s — last-resort cap).
//!    - `ws_close` (~3-5 s — connection died mid-flight).
//! 6. Concatenate every `TranscriptText` payload received and
//!    return.
//!
//! Audio format: caller MUST provide linear16 PCM at 16 kHz
//! mono (the only encoding voice_stream accepts). Ogg-opus /
//! mp3 / m4a callers should preprocess via the
//! `super::super::audio` module first (the `stt` /
//! `stt-candle` features expose `audio::decode_to_pcm_mono`).
//! If the caller passes a different `audio_mime`, we reject
//! with `UnsupportedFormat`.
//!
//! Not implemented in v1 (revisit if a microapp demands them):
//!
//! - Push-to-talk streaming chunks (single buffer only).
//! - Auto-finalize segmentation when interim text shifts
//!   non-cumulatively.
//! - Keyterms boosting beyond the static `with_keyterms`
//!   constructor (no dynamic update mid-stream).

// The voice_stream WebSocket transport is native-only
// (tokio-tungstenite drags TCP types absent on wasm32). A
// future `gloo-net::websocket` swap-in would drop the
// `not(wasm32)` clause; until then, browser microapps enabling
// `stt-cloud-anthropic` get a no-op feature flag (no symbol
// exposed). The REST legs (`openai.rs`, `groq.rs`) are
// wasm-portable — they go through `build_openai_multipart_body`
// instead of `reqwest::multipart::Form`.
#![cfg(all(
    feature = "stt-cloud-wasm",
    feature = "stt-cloud-anthropic",
    not(target_arch = "wasm32"),
))]

use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::time::{sleep_until, Instant};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

use super::SttProvider;
use crate::stt::SttError;

pub const DEFAULT_ENDPOINT: &str = "wss://api.anthropic.com/api/ws/speech_to_text/voice_stream";

/// Keep-alive cadence — must beat the server's idle-timeout
/// window. Matches the `claude-code-leak` value verbatim.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(8);

/// Resolve `TranscriptEndpoint` arrives → finalize within this
/// window. 1.5 s catches the silent-drop server bug.
const NO_DATA_TIMEOUT: Duration = Duration::from_millis(1_500);

/// Absolute cap on the finalize wait — protects against a hung
/// WebSocket that never drains. Matches the leak's 5 s safety
/// budget.
const SAFETY_TIMEOUT: Duration = Duration::from_secs(5);

const KEEPALIVE_MSG: &str = r#"{"type":"KeepAlive"}"#;
const CLOSE_STREAM_MSG: &str = r#"{"type":"CloseStream"}"#;

/// `voice_stream` STT provider. Authenticates with the same
/// OAuth Bearer token Claude Code uses (`nexo-llm-auth` resolves
/// it for the SDK operator).
#[derive(Debug, Clone)]
pub struct AnthropicVoiceStream {
    oauth_token: String,
    endpoint: String,
    keyterms: Vec<String>,
}

impl AnthropicVoiceStream {
    pub fn new(oauth_token: impl Into<String>) -> Self {
        Self {
            oauth_token: oauth_token.into(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            keyterms: Vec::new(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Domain vocabulary the underlying Deepgram Nova 3 engine
    /// should boost. Surfaces in the `keyterms=<term>` repeated
    /// query param.
    pub fn with_keyterms(mut self, keyterms: impl IntoIterator<Item = String>) -> Self {
        self.keyterms = keyterms.into_iter().collect();
        self
    }

    fn build_url(&self, lang_hint: Option<&str>) -> String {
        let lang = lang_hint
            .filter(|l| !l.is_empty() && *l != "auto")
            .map(|l| {
                // voice_stream proxy only honours language-level
                // codes; drop any BCP-47 region subtag (`es-AR` →
                // `es`).
                l.split(|c| c == '-' || c == '_')
                    .next()
                    .unwrap_or(l)
                    .to_lowercase()
            })
            .unwrap_or_else(|| "en".to_string());

        let mut url = format!(
            "{base}?encoding=linear16&sample_rate=16000&channels=1\
             &endpointing_ms=300&utterance_end_ms=1000&use_conversation_engine=true\
             &stt_provider=deepgram-nova3&language={lang}",
            base = self.endpoint,
        );
        for term in &self.keyterms {
            url.push_str("&keyterms=");
            url.push_str(&urlencode(term));
        }
        url
    }
}

/// Tiny inline urlencode — voice_stream keyterms are typically
/// single words / domain proper nouns. We avoid pulling a full
/// `urlencoding` crate for one call site by encoding only the
/// subset that breaks query-string parsing.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[async_trait]
impl SttProvider for AnthropicVoiceStream {
    async fn transcribe(
        &self,
        audio_bytes: Vec<u8>,
        audio_mime: &str,
        lang_hint: Option<&str>,
    ) -> Result<String, SttError> {
        // voice_stream only accepts linear16 PCM @ 16 kHz mono.
        // Reject other MIMEs up-front so the caller knows to
        // pre-decode via `super::super::audio::decode_to_pcm_mono`.
        if !is_acceptable_pcm_mime(audio_mime) {
            return Err(SttError::UnsupportedFormat(format!(
                "voice_stream requires linear16 PCM @ 16 kHz mono (e.g. audio/L16; \
                 rate=16000; channels=1); got {audio_mime:?}. Pre-decode via \
                 super::super::audio::decode_to_pcm_mono first."
            )));
        }
        if audio_bytes.is_empty() {
            return Err(SttError::EmptyAudio);
        }

        let url = self.build_url(lang_hint);
        let mut req = url
            .as_str()
            .into_client_request()
            .map_err(|e| SttError::Whisper(format!("voice_stream URL build: {e}")))?;
        let auth_value = format!("Bearer {}", self.oauth_token);
        let headers = req.headers_mut();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&auth_value)
                .map_err(|e| SttError::Whisper(format!("voice_stream auth header: {e}")))?,
        );
        headers.insert("x-app", HeaderValue::from_static("nexo-rs"));

        let (ws_stream, _resp) = tokio_tungstenite::connect_async(req)
            .await
            .map_err(|e| SttError::Whisper(format!("voice_stream WS connect: {e}")))?;
        let (mut sink, mut stream) = ws_stream.split();

        // Initial KeepAlive — the server idle-times the
        // connection if the first audio chunk takes > a few
        // hundred ms.
        sink.send(Message::Text(KEEPALIVE_MSG.to_string()))
            .await
            .map_err(|e| SttError::Whisper(format!("voice_stream initial KeepAlive: {e}")))?;

        // Send the audio buffer in one binary frame. Anthropic
        // voice_stream accepts streaming chunks too; for the
        // one-shot transcribe API a single frame is enough.
        sink.send(Message::Binary(audio_bytes))
            .await
            .map_err(|e| SttError::Whisper(format!("voice_stream audio send: {e}")))?;

        // Spawn a keep-alive heartbeat so we don't drop the
        // connection while the server is still processing the
        // tail of the audio. Aborted via the JoinHandle when
        // `transcribe` returns.
        let (keepalive_tx, mut keepalive_rx) = tokio::sync::mpsc::channel::<()>(1);
        let keepalive_handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(KEEPALIVE_INTERVAL);
            tick.tick().await; // skip the immediate first tick
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        if keepalive_tx.send(()).await.is_err() {
                            break;
                        }
                    }
                    _ = sleep_until(Instant::now() + Duration::from_secs(60)) => {
                        // Stop if the consumer hasn't drained
                        // the channel in a full minute — the
                        // transcribe call has almost certainly
                        // finished.
                        break;
                    }
                }
            }
        });

        // Send CloseStream to signal end of audio. Deferred to
        // the next event-loop iteration so any pending audio
        // bytes finish flushing first.
        tokio::task::yield_now().await;
        sink.send(Message::Text(CLOSE_STREAM_MSG.to_string()))
            .await
            .map_err(|e| SttError::Whisper(format!("voice_stream CloseStream: {e}")))?;

        // Finalize state machine. Four resolve triggers per
        // claude-code-leak / voiceStreamSTT.ts:
        //   - TranscriptEndpoint post-CloseStream.
        //   - no_data_timeout (1.5 s).
        //   - safety_timeout (5 s).
        //   - ws_close.
        let mut transcript_parts: Vec<String> = Vec::new();
        let mut interim_text: String = String::new();
        let safety_deadline = Instant::now() + SAFETY_TIMEOUT;
        let mut no_data_deadline = Instant::now() + NO_DATA_TIMEOUT;
        let resolved: FinalizeSource;
        loop {
            tokio::select! {
                msg = stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Some(event) = parse_event(&text) {
                                match event {
                                    VoiceStreamEvent::TranscriptText { data } => {
                                        // Bump the no-data timer
                                        // — server is alive +
                                        // sending output.
                                        no_data_deadline =
                                            Instant::now() + NO_DATA_TIMEOUT;
                                        interim_text = data;
                                    }
                                    VoiceStreamEvent::TranscriptEndpoint => {
                                        if !interim_text.is_empty() {
                                            transcript_parts.push(
                                                std::mem::take(&mut interim_text),
                                            );
                                        }
                                        resolved =
                                            FinalizeSource::PostCloseStreamEndpoint;
                                        break;
                                    }
                                    VoiceStreamEvent::TranscriptError { code, msg } => {
                                        keepalive_handle.abort();
                                        return Err(SttError::Whisper(format!(
                                            "voice_stream server error \
                                             code={code:?} msg={msg:?}"
                                        )));
                                    }
                                    VoiceStreamEvent::Other => {
                                        // Unknown JSON shape;
                                        // ignore + keep waiting.
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            resolved = FinalizeSource::WsClose;
                            // Promote any unreported interim
                            // to a final segment so we don't
                            // lose it.
                            if !interim_text.is_empty() {
                                transcript_parts
                                    .push(std::mem::take(&mut interim_text));
                            }
                            break;
                        }
                        Some(Ok(_)) => {
                            // Ping / Pong / non-text frame —
                            // tungstenite handles Pong
                            // automatically; ignore.
                        }
                        Some(Err(e)) => {
                            keepalive_handle.abort();
                            return Err(SttError::Whisper(format!(
                                "voice_stream stream error: {e}"
                            )));
                        }
                        None => {
                            resolved = FinalizeSource::WsClose;
                            if !interim_text.is_empty() {
                                transcript_parts
                                    .push(std::mem::take(&mut interim_text));
                            }
                            break;
                        }
                    }
                }
                _ = sleep_until(no_data_deadline) => {
                    resolved = FinalizeSource::NoDataTimeout;
                    if !interim_text.is_empty() {
                        transcript_parts
                            .push(std::mem::take(&mut interim_text));
                    }
                    break;
                }
                _ = sleep_until(safety_deadline) => {
                    resolved = FinalizeSource::SafetyTimeout;
                    if !interim_text.is_empty() {
                        transcript_parts
                            .push(std::mem::take(&mut interim_text));
                    }
                    break;
                }
                _ = keepalive_rx.recv() => {
                    let _ =
                        sink.send(Message::Text(KEEPALIVE_MSG.to_string())).await;
                }
            }
        }

        keepalive_handle.abort();

        // Best-effort close the socket. Ignore errors — the
        // server may have already shut down.
        let _ = sink.close().await;

        tracing::info!(
            target: "stt.cloud.anthropic",
            resolved = ?resolved,
            segments = transcript_parts.len(),
            "voice_stream finalize complete"
        );

        if transcript_parts.is_empty() {
            return Err(SttError::EmptyTranscript);
        }
        Ok(transcript_parts.join(" ").trim().to_string())
    }

    fn name(&self) -> &'static str {
        "anthropic-voice-stream"
    }
}

/// Allowed PCM MIME types. voice_stream's wire spec is
/// linear16 @ 16 kHz mono — any other shape produces garbage
/// on the server side.
fn is_acceptable_pcm_mime(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase();
    lower.starts_with("audio/l16") || lower.starts_with("audio/pcm") || lower == "audio/x-raw-int"
}

#[derive(Debug, Clone, Copy)]
enum FinalizeSource {
    PostCloseStreamEndpoint,
    NoDataTimeout,
    SafetyTimeout,
    WsClose,
}

#[derive(Debug)]
enum VoiceStreamEvent {
    TranscriptText {
        data: String,
    },
    TranscriptEndpoint,
    TranscriptError {
        code: Option<String>,
        msg: Option<String>,
    },
    Other,
}

fn parse_event(text: &str) -> Option<VoiceStreamEvent> {
    #[derive(Deserialize)]
    struct Envelope<'a> {
        #[serde(rename = "type")]
        ty: &'a str,
        #[serde(default)]
        data: Option<String>,
        #[serde(default, rename = "error_code")]
        error_code: Option<String>,
        #[serde(default)]
        description: Option<String>,
    }
    let env: Envelope = serde_json::from_str(text).ok()?;
    Some(match env.ty {
        "TranscriptText" => VoiceStreamEvent::TranscriptText {
            data: env.data.unwrap_or_default(),
        },
        "TranscriptEndpoint" => VoiceStreamEvent::TranscriptEndpoint,
        "TranscriptError" => VoiceStreamEvent::TranscriptError {
            code: env.error_code,
            msg: env.description,
        },
        _ => VoiceStreamEvent::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_anthropic_wss() {
        let p = AnthropicVoiceStream::new("sk-ant-oat01-...");
        assert_eq!(p.endpoint, DEFAULT_ENDPOINT);
        assert!(p.keyterms.is_empty());
    }

    #[test]
    fn builder_overrides_endpoint_and_keyterms() {
        let p = AnthropicVoiceStream::new("sk-ant-oat01-...")
            .with_endpoint("wss://localhost:9001/stt")
            .with_keyterms(vec!["nexo".into(), "wa-agent".into()]);
        assert_eq!(p.endpoint, "wss://localhost:9001/stt");
        assert_eq!(p.keyterms, vec!["nexo".to_string(), "wa-agent".to_string()]);
    }

    #[test]
    fn build_url_includes_canonical_query_params() {
        let p = AnthropicVoiceStream::new("token");
        let url = p.build_url(Some("es-AR"));
        assert!(url.contains("encoding=linear16"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("channels=1"));
        assert!(url.contains("endpointing_ms=300"));
        assert!(url.contains("utterance_end_ms=1000"));
        assert!(url.contains("use_conversation_engine=true"));
        assert!(url.contains("stt_provider=deepgram-nova3"));
        // BCP-47 region subtag dropped — voice_stream only
        // honours language-level codes.
        assert!(url.contains("language=es"));
        assert!(!url.contains("language=es-AR"));
    }

    #[test]
    fn build_url_default_language_when_none() {
        let p = AnthropicVoiceStream::new("token");
        let url = p.build_url(None);
        assert!(url.contains("language=en"));
    }

    #[test]
    fn build_url_includes_keyterms_repeated() {
        let p = AnthropicVoiceStream::new("token")
            .with_keyterms(vec!["nexo".into(), "wa agent".into()]);
        let url = p.build_url(Some("en"));
        // Each keyterm becomes its own `keyterms=` param +
        // url-encoded if it contains spaces.
        assert!(url.contains("keyterms=nexo"));
        assert!(url.contains("keyterms=wa%20agent"));
    }

    #[test]
    fn urlencode_passes_alphanumerics_through() {
        assert_eq!(urlencode("abc123"), "abc123");
        assert_eq!(
            urlencode("hello-world.com_test~ok"),
            "hello-world.com_test~ok"
        );
    }

    #[test]
    fn urlencode_escapes_spaces_and_unicode() {
        assert_eq!(urlencode("hello world"), "hello%20world");
        // `é` is U+00E9 = 0xC3 0xA9 in UTF-8.
        assert_eq!(urlencode("é"), "%C3%A9");
    }

    #[test]
    fn rejects_non_pcm_mime() {
        let p = AnthropicVoiceStream::new("token");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = runtime.block_on(async { p.transcribe(vec![1, 2, 3], "audio/ogg", None).await });
        match err {
            Ok(t) => panic!("expected error, got {t:?}"),
            Err(SttError::UnsupportedFormat(msg)) => {
                assert!(msg.contains("linear16"), "got: {msg}");
                assert!(msg.contains("audio/ogg"), "got: {msg}");
            }
            Err(other) => panic!("expected UnsupportedFormat, got {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_audio_buffer() {
        let p = AnthropicVoiceStream::new("token");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err =
            runtime.block_on(async { p.transcribe(vec![], "audio/L16; rate=16000", None).await });
        match err {
            Ok(t) => panic!("expected error, got {t:?}"),
            Err(SttError::EmptyAudio) => {}
            Err(other) => panic!("expected EmptyAudio, got {other:?}"),
        }
    }

    #[test]
    fn is_acceptable_pcm_mime_recognises_canonical_shapes() {
        assert!(is_acceptable_pcm_mime("audio/L16; rate=16000"));
        assert!(is_acceptable_pcm_mime("audio/l16"));
        assert!(is_acceptable_pcm_mime("audio/pcm"));
        assert!(is_acceptable_pcm_mime("AUDIO/PCM"));
        assert!(is_acceptable_pcm_mime("audio/x-raw-int"));
        assert!(!is_acceptable_pcm_mime("audio/ogg"));
        assert!(!is_acceptable_pcm_mime("audio/mpeg"));
        assert!(!is_acceptable_pcm_mime("application/json"));
    }

    #[test]
    fn parses_transcript_text_event() {
        let evt = parse_event(r#"{"type":"TranscriptText","data":"hello world"}"#).unwrap();
        match evt {
            VoiceStreamEvent::TranscriptText { data } => assert_eq!(data, "hello world"),
            other => panic!("expected TranscriptText, got {other:?}"),
        }
    }

    #[test]
    fn parses_transcript_endpoint_event() {
        let evt = parse_event(r#"{"type":"TranscriptEndpoint"}"#).unwrap();
        assert!(matches!(evt, VoiceStreamEvent::TranscriptEndpoint));
    }

    #[test]
    fn parses_transcript_error_event() {
        let evt = parse_event(
            r#"{"type":"TranscriptError","error_code":"BAD_AUDIO","description":"silence"}"#,
        )
        .unwrap();
        match evt {
            VoiceStreamEvent::TranscriptError { code, msg } => {
                assert_eq!(code.as_deref(), Some("BAD_AUDIO"));
                assert_eq!(msg.as_deref(), Some("silence"));
            }
            other => panic!("expected TranscriptError, got {other:?}"),
        }
    }

    #[test]
    fn unknown_event_type_maps_to_other() {
        let evt = parse_event(r#"{"type":"SomethingNewServerEmitted","data":"x"}"#).unwrap();
        assert!(matches!(evt, VoiceStreamEvent::Other));
    }

    #[test]
    fn malformed_json_returns_none() {
        assert!(parse_event("not json").is_none());
        assert!(parse_event("{").is_none());
    }
}
