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

use std::io::Write;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;

use super::normalize::{collapse_punctuation, normalise_markdown_for_tts, strip_emojis_for_tts};
use super::ssml::{apply_ssml_hints, strip_voice_markers};
use super::{Result, VoiceError};

/// Format we ask Microsoft Edge for. The public endpoint reliably
/// streams the mp3 profiles; opus profiles are documented but the
/// WS sometimes closes with zero audio frames (likely an upstream
/// quirk). We get the mp3, then transcode to OGG/Opus via ffmpeg
/// before handing the bytes to WhatsApp's PTT path — that combo
/// is what `send_voice_note` actually needs to render as a voice
/// bubble that recipients can play.
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

/// Pipe the mp3 through `ffmpeg -i pipe:0 -c:a libopus -b:a 32k
/// -f ogg pipe:1`. Stays inside the process — no temp files. If
/// ffmpeg isn't installed or fails, propagate the error so the
/// caller falls back to text on this turn (better than sending an
/// unplayable PTT).
pub async fn transcode_mp3_to_opus_ogg(mp3: &[u8]) -> Result<Vec<u8>> {
    let mut child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            // Voice-note friendly: mono, 16 kHz, opus at 32 kbps.
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "libopus",
            "-b:a",
            "32k",
            "-f",
            "ogg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| VoiceError::Ffmpeg(format!("spawn (is it installed?): {e}")))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| VoiceError::Ffmpeg("stdin missing".into()))?;
        let mp3_owned = mp3.to_vec();
        tokio::spawn(async move {
            let _ = stdin.write_all(&mp3_owned).await;
            // Close stdin so ffmpeg flushes the trailer.
            drop(stdin);
            // Silence unused import warn for std::io::Write.
            let _ = std::io::sink().flush();
        });
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| VoiceError::Ffmpeg(format!("wait_with_output: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(VoiceError::Ffmpeg(format!(
            "exit {:?}: {stderr}",
            output.status.code()
        )));
    }
    if output.stdout.is_empty() {
        return Err(VoiceError::Ffmpeg("produced 0 bytes".into()));
    }
    Ok(output.stdout)
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
