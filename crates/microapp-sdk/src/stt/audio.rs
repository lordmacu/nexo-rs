//! Phase 91 — shared audio decode chain for both STT backends.
//!
//! Originally lived inside `transcribe.rs` (the whisper-rs path);
//! lifted here so the Candle backend (`transcribe_candle.rs`) can
//! reuse the same pure-Rust ogg-opus → s16 PCM → f32 pipeline. The
//! audio chain is backend-agnostic — only the inference layer
//! differs between the two.
//!
//! Wire shape:
//!
//! ```text
//! .ogg bytes ─▶ ogg::PacketReader ─▶ opus_wave::OpusDecoder ─▶ mono f32 ─▶ resample ─▶ s16 LE bytes
//! ```
//!
//! Both [`super::transcribe::transcribe_file`] (legacy whisper-rs)
//! and [`super::transcribe_candle::transcribe_file`] (Phase 91
//! Candle) call into [`decode_to_pcm_mono`] and then [`pcm_s16_to_f32`]
//! before handing samples off to their respective inference backends.

#![cfg(any(feature = "stt", feature = "stt-candle"))]

use std::path::Path;

use super::{Result, SttError, TranscribeConfig};

/// Decode the audio at `path` to little-endian s16 mono PCM at
/// the configured sample rate. WhatsApp and Telegram voice notes
/// arrive as ogg-opus, which is the only family this pipeline
/// understands; non-ogg payloads (mp3 attachments, etc.) surface
/// as [`SttError::UnsupportedFormat`].
pub(crate) async fn decode_to_pcm_mono(path: &Path, cfg: &TranscribeConfig) -> Result<Vec<u8>> {
    let bytes = tokio::fs::read(path).await?;
    let target_rate = cfg.target_sample_rate;
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> { decode_ogg_opus(&bytes, target_rate) })
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
            .decode_float(
                Some(&packet.data),
                &mut buf,
                max_frame_samples as i32,
                false,
            )
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
/// format whisper.cpp's API and Candle's mel pipeline both
/// consume.
pub(crate) fn pcm_s16_to_f32(pcm: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(pcm.len() / 2);
    for chunk in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        out.push(s as f32 / i16::MAX as f32);
    }
    out
}
