//! Whisper-backed transcription for short voice notes.
//!
//! Pipeline:
//! 1. Read audio file from disk.
//! 2. Pure-Rust decode to mono PCM at the configured sample rate.
//!    Ogg-opus (WhatsApp / Telegram voice notes) flows through the
//!    `ogg` demuxer + `opus-wave` decoder; everything else routes
//!    through `symphonia`.
//! 3. Convert s16 PCM samples to f32 in `[-1.0, 1.0]`.
//! 4. Run whisper inference and concatenate segments.
//!
//! Each [`super::TranscribeConfig::model_path`] gets its own
//! `WhisperContext` cached in a process-wide map so different
//! microapps / tests don't pay the model-load cost twice for the
//! same checkpoint. whisper.cpp is reentrant for reads, so a
//! shared context is safe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{Result, SttError, TranscribeConfig};

/// Process-wide whisper-context cache keyed by model path. The
/// `Mutex` is held only during slot insertion — the
/// `Arc<WhisperContext>` itself is shared by clones.
static MODEL_CACHE: tokio::sync::OnceCell<Mutex<HashMap<PathBuf, Arc<WhisperContext>>>> =
    tokio::sync::OnceCell::const_new();

async fn load_model(path: &Path) -> Result<Arc<WhisperContext>> {
    if !path.is_file() {
        return Err(SttError::ModelMissing(path.display().to_string()));
    }
    let cache = MODEL_CACHE
        .get_or_init(|| async { Mutex::new(HashMap::new()) })
        .await;
    {
        let guard = cache.lock().await;
        if let Some(ctx) = guard.get(path) {
            return Ok(Arc::clone(ctx));
        }
    }
    // Slow path — initialise outside the lock so we don't block
    // other model loads on a different file.
    tracing::info!(model = %path.display(), "stt: loading whisper model");
    let path_str = path.to_string_lossy().into_owned();
    let ctx = tokio::task::spawn_blocking(move || {
        WhisperContext::new_with_params(&path_str, WhisperContextParameters::default())
            .map_err(|e| SttError::Whisper(format!("context init: {e}")))
    })
    .await
    .map_err(|e| SttError::Whisper(format!("context init join: {e}")))??;
    let arc = Arc::new(ctx);
    let mut guard = cache.lock().await;
    // Re-check in case a parallel caller raced us.
    Ok(guard
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::clone(&arc))
        .clone())
}

/// Transcribe the audio at `path` using `cfg`. Returns the
/// trimmed transcript.
///
/// Empty audio after ffmpeg decode → [`SttError::EmptyAudio`].
/// Empty whisper output → [`SttError::EmptyTranscript`]. Both
/// distinguished from process-level failures so the caller can
/// decide whether to fall back vs alert.
pub async fn transcribe_file(path: &Path, cfg: &TranscribeConfig) -> Result<String> {
    let started = std::time::Instant::now();
    let pcm = decode_to_pcm_mono(path, cfg).await?;
    if pcm.is_empty() {
        return Err(SttError::EmptyAudio);
    }
    let samples = pcm_s16_to_f32(&pcm);
    let lang = cfg.lang_hint.clone();
    let context = load_model(&cfg.model_path).await?;
    let transcript = tokio::task::spawn_blocking(move || -> Result<String> {
        let mut state = context
            .create_state()
            .map_err(|e| SttError::Whisper(format!("create_state: {e}")))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // whisper-rs requires a `&'static str`; leak deliberately
        // so the language string outlives the inference call.
        // Memory cost is one tiny string per unique language hint
        // observed by the process, which is bounded.
        if let Some(l) = lang.filter(|l| l != "auto") {
            params.set_language(Some(Box::leak(l.into_boxed_str())));
        }
        state
            .full(params, &samples)
            .map_err(|e| SttError::Whisper(format!("full: {e}")))?;
        let n = state
            .full_n_segments()
            .map_err(|e| SttError::Whisper(format!("segments: {e}")))?;
        let mut out = String::new();
        for i in 0..n {
            let seg = state
                .full_get_segment_text(i)
                .map_err(|e| SttError::Whisper(format!("segment text {i}: {e}")))?;
            out.push_str(&seg);
        }
        Ok(out.trim().to_string())
    })
    .await
    .map_err(|e| SttError::Whisper(format!("transcribe join: {e}")))??;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        path = %path.display(),
        transcript_len = transcript.len(),
        elapsed_ms,
        "stt: transcription ok",
    );
    if transcript.is_empty() {
        return Err(SttError::EmptyTranscript);
    }
    Ok(transcript)
}

/// Decode the audio at `path` to little-endian s16 mono PCM at
/// the configured sample rate. WhatsApp and Telegram voice notes
/// arrive as ogg-opus, which is the only family this pipeline
/// understands; non-ogg payloads (mp3 attachments, etc.) surface
/// as [`SttError::UnsupportedFormat`].
async fn decode_to_pcm_mono(path: &Path, cfg: &TranscribeConfig) -> Result<Vec<u8>> {
    let bytes = tokio::fs::read(path).await?;
    let target_rate = cfg.target_sample_rate;
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        decode_ogg_opus(&bytes, target_rate)
    })
    .await
    .map_err(|e| SttError::Decode(format!("decode join: {e}")))?
}

/// Demux an ogg container, decode opus packets with `opus-wave`,
/// downmix to mono, resample to `target_rate`, and serialise to
/// little-endian s16 PCM bytes.
fn decode_ogg_opus(bytes: &[u8], target_rate: u32) -> Result<Vec<u8>> {
    if !bytes.starts_with(b"OggS") {
        return Err(SttError::UnsupportedFormat(
            "expected ogg container (WA/TG voice notes); got something else".into(),
        ));
    }

    let cursor = std::io::Cursor::new(bytes.to_vec());
    let mut reader = ogg::PacketReader::new(cursor);

    // Packet 1 = OpusHead. Layout (RFC 7845 § 5.1):
    //   bytes 0..8   = "OpusHead"
    //   byte 8       = version
    //   byte 9       = output channel count
    //   bytes 10..12 = pre-skip samples (LE u16)
    //   bytes 12..16 = original sample rate (LE u32, may be 0)
    //   bytes 16..18 = output gain (LE i16)
    //   byte 18      = channel mapping family
    let head = reader
        .read_packet_expected()
        .map_err(|e| SttError::Decode(format!("ogg OpusHead: {e}")))?;
    if !head.data.starts_with(b"OpusHead") || head.data.len() < 19 {
        return Err(SttError::UnsupportedFormat(
            "ogg stream is not opus (missing OpusHead)".into(),
        ));
    }
    let channels = head.data[9] as usize;
    if channels == 0 {
        return Err(SttError::Decode("OpusHead reports 0 channels".into()));
    }

    // Packet 2 = OpusTags (vorbis-comment-style). Discard.
    let _tags = reader
        .read_packet_expected()
        .map_err(|e| SttError::Decode(format!("ogg OpusTags: {e}")))?;

    // Decode at the target rate when it's one of opus's supported
    // output rates so we skip the resample step entirely. Otherwise
    // decode at 48 kHz (opus internal) and linear-resample after.
    let (decoder_sr, decoder_rate_hz) = match target_rate {
        8000 => (opus_wave::SampleRate::Hz8000, 8000u32),
        12000 => (opus_wave::SampleRate::Hz12000, 12000),
        16000 => (opus_wave::SampleRate::Hz16000, 16000),
        24000 => (opus_wave::SampleRate::Hz24000, 24000),
        _ => (opus_wave::SampleRate::Hz48000, 48000),
    };
    let decoder_channels = if channels >= 2 {
        opus_wave::Channels::Stereo
    } else {
        opus_wave::Channels::Mono
    };
    let mut decoder = opus_wave::OpusDecoder::new(decoder_sr, decoder_channels)
        .map_err(|e| SttError::Decode(format!("opus decoder init: {e:?}")))?;

    // Allocate the worst-case opus frame buffer: 120 ms per channel
    // (the libopus C API recommendation — covers FEC redundancy and
    // the longest concealment frames). Smaller buffers trip
    // `BufferTooSmall` on real WhatsApp packets.
    let max_frame_samples = (decoder_rate_hz as usize / 1000) * 120;
    let dec_channels_n = match decoder_channels {
        opus_wave::Channels::Mono => 1,
        opus_wave::Channels::Stereo => 2,
    };
    let mut buf = vec![0.0f32; max_frame_samples * dec_channels_n];

    let mut mono = Vec::<f32>::new();
    while let Some(packet) = reader
        .read_packet()
        .map_err(|e| SttError::Decode(format!("ogg packet: {e}")))?
    {
        let n = decoder
            .decode_float(Some(&packet.data), &mut buf, max_frame_samples as i32, false)
            .map_err(|e| SttError::Decode(format!("opus decode: {e:?}")))?;
        let n = n as usize;
        if n == 0 {
            continue;
        }
        if dec_channels_n == 1 {
            mono.extend_from_slice(&buf[..n]);
        } else {
            for i in 0..n {
                let mut sum = 0.0f32;
                for c in 0..dec_channels_n {
                    sum += buf[i * dec_channels_n + c];
                }
                mono.push(sum / dec_channels_n as f32);
            }
        }
    }

    let resampled = if decoder_rate_hz == target_rate {
        mono
    } else {
        resample_linear(&mono, decoder_rate_hz, target_rate)
    };

    f32_mono_to_s16le_bytes(&resampled)
}

/// Linear interpolation resampler. Cheap, lossy, perfectly fine
/// for whisper STT — the model is far more tolerant of resample
/// artefacts than of latency or dependency footprint.
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

/// Convert a mono f32 PCM stream in `[-1.0, 1.0]` to little-endian
/// signed-16 bytes — the layout the legacy ffmpeg pipe used to
/// produce, kept stable so [`pcm_s16_to_f32`] still applies.
fn f32_mono_to_s16le_bytes(samples: &[f32]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let v = (s * 32767.0).clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    Ok(out)
}

/// Convert little-endian s16 PCM to f32 in `[-1.0, 1.0]` — the
/// format whisper.cpp's API expects.
fn pcm_s16_to_f32(pcm: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(pcm.len() / 2);
    for chunk in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        out.push(s as f32 / i16::MAX as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_round_trip_handles_extreme_values() {
        // Min, zero, max → -1.0, 0.0, ~1.0.
        let pcm: Vec<u8> = vec![0x00, 0x80, 0x00, 0x00, 0xFF, 0x7F];
        let f = pcm_s16_to_f32(&pcm);
        assert_eq!(f.len(), 3);
        assert!((f[0] - -1.0).abs() < 0.001);
        assert_eq!(f[1], 0.0);
        assert!((f[2] - 1.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn load_model_surfaces_missing_file_as_typed_error() {
        let p = PathBuf::from("/nonexistent/whisper-model-for-tests.bin");
        let r = load_model(&p).await;
        assert!(matches!(r, Err(SttError::ModelMissing(_))));
    }

    #[tokio::test]
    async fn transcribe_file_surfaces_missing_audio_as_io_error() {
        let cfg = TranscribeConfig::default();
        let r = transcribe_file(Path::new("/nonexistent/voice.ogg"), &cfg).await;
        // `tokio::fs::read` errors first → mapped to `Io`.
        assert!(matches!(r, Err(SttError::Io(_))));
    }
}
