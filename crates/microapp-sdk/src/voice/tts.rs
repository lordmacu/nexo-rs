//! Text-to-speech provider trait + Microsoft Edge Read-Aloud
//! impl + ffmpeg transcode + the end-to-end
//! [`synthesize_voice_note`] pipeline.
//!
//! Provider abstraction so microapps can swap in another TTS
//! backend (Azure cognitive services, ElevenLabs, Google) by
//! implementing [`TtsProvider`]. The Edge impl that ships here is
//! what `agent-creator-microapp` used pre-extraction; both come
//! pre-wired with the SSML retry-on-empty fallback that the
//! public Edge endpoint occasionally needs.
//!
//! The high-level [`synthesize_voice_note`] composes:
//! 1. Markdown → sentence-boundary normaliser
//!    ([`super::normalize::normalise_markdown_for_tts`]).
//! 2. Operator markers + auto-detect → SSML
//!    ([`super::ssml::apply_ssml_hints`]).
//! 3. Emoji / unsafe-char strip
//!    ([`super::normalize::strip_emojis_for_tts`]).
//! 4. Punctuation collapse
//!    ([`super::normalize::collapse_punctuation`]).
//! 5. `provider.synthesize_raw(body, voice_id)` → mp3 bytes.
//! 6. ffmpeg transcode → opus/ogg ([`transcode_mp3_to_opus_ogg`]).
//! 7. Strip operator markers from the original text → display
//!    transcript field on [`VoiceNote`].

use std::borrow::Cow;
use std::io::Cursor;

use async_trait::async_trait;

use super::normalize::{collapse_punctuation, normalise_markdown_for_tts, strip_emojis_for_tts};
use super::ssml::{apply_ssml_hints, strip_voice_markers};
use super::{Result, VoiceError};

/// Format we ask Microsoft Edge for. The public endpoint reliably
/// streams the mp3 profiles; opus profiles are documented but the
/// WS closes with zero audio frames in practice (verified
/// 2026-05-08 — three retries all empty). We get the mp3, then
/// transcode to OGG/Opus via ffmpeg before handing the bytes to
/// WhatsApp's PTT path — that combo is what `send_voice_note`
/// actually needs to render as a voice bubble that recipients can
/// play. Pure-Rust mp3→opus transcode is a follow-up (would let
/// us drop the ffmpeg dep entirely).
pub const EDGE_AUDIO_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

/// MIME type sent with the OGG/Opus payload after transcode. Both
/// the container AND the codec hint are required — iOS WhatsApp
/// rejects bare `audio/ogg` for PTT.
pub const VOICE_NOTE_MIME: &str = "audio/ogg; codecs=opus";

/// What [`synthesize_voice_note`] returns: the audio payload, its
/// MIME hint, and the marker-stripped text the caller stores as
/// the chat-visible transcript.
#[derive(Debug, Clone)]
pub struct VoiceNote {
    /// OGG/Opus bytes ready to hand to a WhatsApp PTT sender.
    pub audio_bytes: Vec<u8>,
    /// Always [`VOICE_NOTE_MIME`]; surfaced as a field so callers
    /// don't pull the constant in two places.
    pub mimetype: &'static str,
    /// Original text with all `[em]…[/em]`-style markers stripped
    /// — what the chat UI / firehose audit should display.
    pub transcript: String,
}

/// TTS backend. Implementors return raw mp3 bytes; the
/// pipeline-level [`synthesize_voice_note`] handles SSML
/// preparation + transcode.
///
/// Implementors are responsible for any provider-specific
/// fallbacks (Edge's empty-body retry-with-plain-text path lives
/// inside [`EdgeTtsProvider::synthesize_raw`]).
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize `body` (already SSML-prepared) with `voice_id`.
    /// Returns the raw mp3 bytes. Empty output is an error
    /// because the pipeline can't produce a voice note from
    /// nothing.
    async fn synthesize_raw(&self, body: &str, voice_id: &str) -> Result<Vec<u8>>;
}

/// Microsoft Edge Read-Aloud provider. Wraps the public WSS
/// endpoint via the `msedge-tts` crate.
///
/// `rate` is the speaking-rate delta in percent (`-100..+200`).
/// The agent-creator-microapp default of `-8` (~8% slower than
/// the voice's default) reads naturally on phone speakers; the
/// SDK default matches it.
#[derive(Debug, Clone)]
pub struct EdgeTtsProvider {
    /// Speaking rate delta in percent. `0` = voice's natural cadence.
    pub rate: i32,
    /// Audio format passed to Edge. Override only when the caller
    /// has a reason to bypass the default mp3 → opus transcode.
    pub audio_format: String,
}

impl Default for EdgeTtsProvider {
    fn default() -> Self {
        Self {
            rate: -8,
            audio_format: EDGE_AUDIO_FORMAT.to_string(),
        }
    }
}

#[async_trait]
impl TtsProvider for EdgeTtsProvider {
    async fn synthesize_raw(&self, body: &str, voice_id: &str) -> Result<Vec<u8>> {
        if voice_id.trim().is_empty() {
            return Err(VoiceError::EmptyVoiceId);
        }
        // Try the full body first. If Edge returns 0 bytes, retry
        // with every SSML tag stripped — that tells us whether the
        // failure is from the SSML payload (Edge rejected our
        // markup) or from the endpoint itself (network / service
        // flake). Both branches log loud enough to diagnose.
        let mp3 = match call_edge(body, voice_id, &self.audio_format, self.rate).await {
            Ok(bytes) if !bytes.is_empty() => bytes,
            Ok(_empty) => {
                let plain = strip_ssml_tags(body);
                tracing::warn!(
                    ssml_body_len = body.len(),
                    plain_body_len = plain.len(),
                    ssml_body = %body,
                    "voice: edge returned 0 bytes; retrying with plain text",
                );
                match call_edge(&plain, voice_id, &self.audio_format, self.rate).await {
                    Ok(bytes) if !bytes.is_empty() => {
                        tracing::warn!(
                            "voice: plain-text fallback succeeded — SSML body was rejected by edge",
                        );
                        bytes
                    }
                    Ok(_) => return Err(VoiceError::EmptySynthesis),
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
        Ok(mp3)
    }
}

/// One round-trip through `msedge-tts`. Returns the raw mp3
/// bytes (which may be empty if Edge accepted the request but
/// produced no audio — caller decides how to react).
async fn call_edge(body: &str, voice: &str, audio_format: &str, rate: i32) -> Result<Vec<u8>> {
    let body_owned = body.to_string();
    let voice_owned = voice.to_string();
    let format_owned = audio_format.to_string();
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut client = msedge_tts::tts::client::connect()
            .map_err(|e| VoiceError::Edge(format!("connect: {e}")))?;
        let cfg = msedge_tts::tts::SpeechConfig {
            voice_name: voice_owned,
            audio_format: format_owned,
            pitch: 0,
            rate,
            volume: 0,
        };
        let synthesized = client
            .synthesize(&body_owned, &cfg)
            .map_err(|e| VoiceError::Edge(format!("synthesize: {e}")))?;
        Ok(synthesized.audio_bytes)
    })
    .await
    .map_err(|e| VoiceError::Edge(format!("synthesize join: {e}")))?
}

/// Drop every `<…>` tag from `input`, leaving the inner text.
/// Used as a diagnostic fallback when Edge rejects the SSML body.
fn strip_ssml_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if ch == '>' {
            in_tag = false;
            continue;
        }
        if !in_tag {
            out.push(ch);
        }
    }
    // Squeeze double spaces left behind by tag removal.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pure-Rust mp3 → ogg-opus transcode. Replaces the legacy
/// `ffmpeg -c:a libopus` subprocess so the pipeline stays
/// in-process (no spawn, no PATH lookup, cross-compile friendly).
///
/// Stages:
/// 1. `symphonia` decodes the mp3 to f32 PCM at the source rate
///    (Edge defaults to 24 kHz mono — same shape we want for the
///    encoder, so the resample step is usually a no-op).
/// 2. `opus-wave::OpusEncoder` encodes 20 ms frames at 24 kHz
///    mono in `Voip` mode (the application opus is tuned for).
/// 3. `ogg::PacketWriter` muxes the result into an ogg container,
///    starting with `OpusHead` + `OpusTags` per RFC 7845 § 5.
///
/// The resulting bytes are exactly the wire format WhatsApp PTT
/// renders as a voice-note bubble — same shape `ffmpeg` produced
/// before, just without the binary dependency.
pub async fn transcode_mp3_to_opus_ogg(mp3: &[u8]) -> Result<Vec<u8>> {
    let mp3_owned = mp3.to_vec();
    tokio::task::spawn_blocking(move || transcode_mp3_to_opus_ogg_blocking(&mp3_owned))
        .await
        .map_err(|e| VoiceError::Ffmpeg(format!("transcode join: {e}")))?
}

/// Synchronous core of [`transcode_mp3_to_opus_ogg`]. Lives here
/// instead of inline so callers can drive it from inside an
/// existing `spawn_blocking` if they already own one.
fn transcode_mp3_to_opus_ogg_blocking(mp3: &[u8]) -> Result<Vec<u8>> {
    use ogg::PacketWriteEndInfo;
    use opus_wave::{Application, Channels, OpusEncoder, SampleRate};
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    if mp3.is_empty() {
        return Err(VoiceError::Ffmpeg("mp3 input is empty".into()));
    }

    // ── Stage 1: decode mp3 → f32 mono PCM at source rate. ────
    let cursor = Cursor::new(mp3.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| VoiceError::Ffmpeg(format!("probe mp3: {e}")))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| VoiceError::Ffmpeg("no audio track in mp3".into()))?;
    let track_id = track.id;
    let source_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| VoiceError::Ffmpeg("mp3 has no sample rate".into()))?;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| VoiceError::Ffmpeg(format!("mp3 decoder init: {e}")))?;

    let mut pcm_at_source: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut input_channels: usize = 1;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(VoiceError::Ffmpeg(format!("mp3 packet: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Skip frames that the decoder can't recover (rare for
            // Edge mp3 output — we'd rather lose a few ms than abort
            // the whole synthesis).
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(VoiceError::Ffmpeg(format!("mp3 decode: {e}"))),
        };
        if sample_buf.is_none() {
            let spec = *decoded.spec();
            input_channels = spec.channels.count().max(1);
            sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        }
        let sb = sample_buf.as_mut().unwrap();
        sb.copy_interleaved_ref(decoded);
        let interleaved = sb.samples();
        if input_channels == 1 {
            pcm_at_source.extend_from_slice(interleaved);
        } else {
            for chunk in interleaved.chunks_exact(input_channels) {
                let avg = chunk.iter().sum::<f32>() / input_channels as f32;
                pcm_at_source.push(avg);
            }
        }
    }
    if pcm_at_source.is_empty() {
        return Err(VoiceError::Ffmpeg("mp3 decoded to empty PCM".into()));
    }

    // ── Stage 1b: linear resample to 24 kHz if Edge ever ships
    // a different rate. Edge's default mp3 profile is 24 kHz so
    // this is usually a no-op.
    const TARGET_RATE: u32 = 24_000;
    let pcm_24k = if source_rate == TARGET_RATE {
        pcm_at_source
    } else {
        resample_linear(&pcm_at_source, source_rate, TARGET_RATE)
    };

    // ── Stage 2: configure opus encoder. ──────────────────────
    let mut encoder = OpusEncoder::new(SampleRate::Hz24000, Channels::Mono, Application::Voip)
        .map_err(|e| VoiceError::Ffmpeg(format!("opus encoder init: {e:?}")))?;
    // 20 ms is the canonical opus voice frame; 480 samples at 24 kHz.
    const FRAME_MS: u32 = 20;
    let frame_size_per_channel: i32 = (TARGET_RATE * FRAME_MS / 1000) as i32;
    let frame_size_usize = frame_size_per_channel as usize;

    // ── Stage 3: ogg muxer with OpusHead + OpusTags + audio. ──
    let mut output = Vec::<u8>::new();
    let serial: u32 = 0xCA5C_ADE0; // arbitrary stable stream id
    {
        let mut writer = ogg::PacketWriter::new(Cursor::new(&mut output));

        // OpusHead — RFC 7845 § 5.1 (19 bytes, channel mapping
        // family 0 for the mono case).
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(1); // output channel count
        // Pre-skip: the standard libopus value of 312 samples at
        // 48 kHz (the encoder's algorithmic delay) is what every
        // major decoder expects.
        head.extend_from_slice(&312u16.to_le_bytes());
        head.extend_from_slice(&TARGET_RATE.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family

        writer
            .write_packet(
                Cow::<[u8]>::Owned(head),
                serial,
                PacketWriteEndInfo::EndPage,
                0,
            )
            .map_err(|e| VoiceError::Ffmpeg(format!("ogg OpusHead: {e}")))?;

        // OpusTags — RFC 7845 § 5.2.
        let vendor = b"nexo-microapp-sdk";
        let mut tags = Vec::with_capacity(8 + 4 + vendor.len() + 4);
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes()); // 0 user comments
        writer
            .write_packet(
                Cow::<[u8]>::Owned(tags),
                serial,
                PacketWriteEndInfo::EndPage,
                0,
            )
            .map_err(|e| VoiceError::Ffmpeg(format!("ogg OpusTags: {e}")))?;

        // Audio packets. Granule position counts samples at the
        // mandatory 48 kHz reference rate (RFC 7845 § 4); each
        // 24 kHz frame of 480 samples = 960 samples at 48 kHz.
        let total_frames = pcm_24k.len() / frame_size_usize;
        if total_frames == 0 {
            return Err(VoiceError::Ffmpeg(
                "mp3 too short to encode a single 20 ms opus frame".into(),
            ));
        }
        let mut packet_buf = vec![0u8; 4000];
        let packet_buf_cap: i32 = packet_buf.len() as i32;
        let mut granule_48k: u64 = 0;
        for i in 0..total_frames {
            let start = i * frame_size_usize;
            let frame = &pcm_24k[start..start + frame_size_usize];
            let len = encoder
                .encode_float(
                    frame,
                    frame_size_per_channel,
                    &mut packet_buf,
                    packet_buf_cap,
                )
                .map_err(|e| VoiceError::Ffmpeg(format!("opus encode: {e:?}")))?;
            if len <= 0 {
                continue;
            }
            granule_48k += (frame_size_usize as u64) * 2; // 24 kHz → 48 kHz
            let info = if i + 1 == total_frames {
                PacketWriteEndInfo::EndStream
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            let bytes = packet_buf[..len as usize].to_vec();
            writer
                .write_packet(Cow::<[u8]>::Owned(bytes), serial, info, granule_48k)
                .map_err(|e| VoiceError::Ffmpeg(format!("ogg audio: {e}")))?;
        }
        // Writer drops here, flushing any pending page state.
    }

    if output.is_empty() {
        return Err(VoiceError::Ffmpeg("produced 0 bytes".into()));
    }
    Ok(output)
}

/// Linear interpolation resampler. Quality is fine for the
/// voice-note use case; whisper-grade resampling is overkill when
/// the consumer is a phone speaker.
fn resample_linear(input: &[f32], from_hz: u32, to_hz: u32) -> Vec<f32> {
    if from_hz == to_hz || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from_hz as f64 / to_hz as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    let last_idx = input.len() - 1;
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let i0 = src.floor() as usize;
        let i1 = (i0 + 1).min(last_idx);
        let frac = (src - i0 as f64) as f32;
        let s0 = input[i0];
        let s1 = input[i1];
        out.push(s0 + (s1 - s0) * frac);
    }
    out
}

/// End-to-end pipeline: text → SSML → mp3 → opus/ogg, with the
/// chat-visible transcript stripped of operator markers.
///
/// Empty input or empty post-strip body → [`VoiceError::EmptyText`].
/// Empty `voice_id` → [`VoiceError::EmptyVoiceId`].
pub async fn synthesize_voice_note(
    text: &str,
    voice_id: &str,
    provider: &dyn TtsProvider,
) -> Result<VoiceNote> {
    if voice_id.trim().is_empty() {
        return Err(VoiceError::EmptyVoiceId);
    }
    // Order matters: SSML hints run BEFORE the symbol stripper so
    // raw `$` / `-` are still present for the regexes; the
    // stripper preserves bytes inside `<…>` so the tags survive.
    let normalised = normalise_markdown_for_tts(text);
    let with_ssml = apply_ssml_hints(&normalised);
    let stripped = strip_emojis_for_tts(&with_ssml);
    let body = collapse_punctuation(stripped.trim());
    if body.is_empty() {
        return Err(VoiceError::EmptyText);
    }

    // Operator-visible: which markers did the LLM emit and which
    // SSML tags survive into the body the provider will speak.
    let counts = count_markers(text);
    let tag_counts = count_ssml_tags(&body);
    let body_preview: String = body.chars().take(400).collect();
    tracing::info!(
        marker_pause = counts.pause,
        marker_em = counts.em,
        marker_strong = counts.strong,
        marker_spell = counts.spell,
        marker_slow = counts.slow,
        marker_fast = counts.fast,
        ssml_break = tag_counts.break_,
        ssml_emphasis = tag_counts.emphasis,
        ssml_say_as = tag_counts.say_as,
        ssml_prosody = tag_counts.prosody,
        body_len = body.len(),
        body_preview = %body_preview,
        "voice: ssml pipeline ready",
    );

    let mp3 = provider.synthesize_raw(&body, voice_id).await?;
    let audio_bytes = transcode_mp3_to_opus_ogg(&mp3).await?;
    let transcript = strip_voice_markers(text);
    Ok(VoiceNote {
        audio_bytes,
        mimetype: VOICE_NOTE_MIME,
        transcript,
    })
}

// ── Diagnostics ────────────────────────────────────────────────

#[derive(Default)]
struct MarkerCounts {
    pause: usize,
    em: usize,
    strong: usize,
    spell: usize,
    slow: usize,
    fast: usize,
}

fn count_markers(input: &str) -> MarkerCounts {
    MarkerCounts {
        pause: input.matches("[pause=").count(),
        em: input.matches("[em]").count(),
        strong: input.matches("[strong]").count(),
        spell: input.matches("[spell]").count(),
        slow: input.matches("[slow]").count(),
        fast: input.matches("[fast]").count(),
    }
}

#[derive(Default)]
struct TagCounts {
    break_: usize,
    emphasis: usize,
    say_as: usize,
    prosody: usize,
}

fn count_ssml_tags(input: &str) -> TagCounts {
    TagCounts {
        break_: input.matches("<break ").count(),
        emphasis: input.matches("<emphasis ").count(),
        say_as: input.matches("<say-as ").count(),
        prosody: input.matches("<prosody ").count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Provider stub that records the body it sees + lets the test
    /// inject a canned mp3 (or error). Lets us exercise the
    /// pipeline without standing up the real Edge websocket.
    struct StubProvider {
        canned: Result<Vec<u8>>,
    }

    #[async_trait]
    impl TtsProvider for StubProvider {
        async fn synthesize_raw(&self, _body: &str, _voice: &str) -> Result<Vec<u8>> {
            match &self.canned {
                Ok(b) => Ok(b.clone()),
                Err(e) => Err(match e {
                    VoiceError::Edge(s) => VoiceError::Edge(s.clone()),
                    VoiceError::EmptySynthesis => VoiceError::EmptySynthesis,
                    _ => VoiceError::Edge("stub".into()),
                }),
            }
        }
    }

    #[tokio::test]
    async fn synthesize_voice_note_rejects_empty_text() {
        let p = StubProvider {
            canned: Ok(b"x".to_vec()),
        };
        let r = synthesize_voice_note("   ", "es-MX-DaliaNeural", &p).await;
        assert!(matches!(r, Err(VoiceError::EmptyText)));
    }

    #[tokio::test]
    async fn synthesize_voice_note_rejects_empty_voice() {
        let p = StubProvider {
            canned: Ok(b"x".to_vec()),
        };
        let r = synthesize_voice_note("hola", "", &p).await;
        assert!(matches!(r, Err(VoiceError::EmptyVoiceId)));
    }

    #[test]
    fn strip_ssml_tags_drops_break_and_say_as() {
        let s = strip_ssml_tags(
            r#"hola <break time="200ms"/> mundo <say-as interpret-as="characters">SIC</say-as>"#,
        );
        assert_eq!(s, "hola mundo SIC");
    }

    #[test]
    fn marker_counts_tracks_each_kind() {
        let raw = "[pause=400ms] [em]foo[/em] [strong]bar[/strong] [spell]X[/spell]";
        let c = count_markers(raw);
        assert_eq!(c.pause, 1);
        assert_eq!(c.em, 1);
        assert_eq!(c.strong, 1);
        assert_eq!(c.spell, 1);
    }
}
