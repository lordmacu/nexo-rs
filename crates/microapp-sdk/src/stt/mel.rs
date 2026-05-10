//! Phase 91.3 — mel-spectrogram helpers for the Candle STT backend.
//!
//! Thin wrapper over
//! [`candle_transformers::models::whisper::audio::pcm_to_mel`] —
//! the heavy lifting (STFT + 80-bin mel filterbank + log scaling)
//! already lives in Candle. We only need to:
//!
//! 1. Decode the bundled `melfilters.bytes` asset into a
//!    `Vec<f32>` (the binary ships 80 × 201 little-endian f32
//!    filterbank coefficients matching the Whisper canonical
//!    layout — same file Candle's own example uses).
//! 2. Probe the input length against the Whisper 30-second
//!    ceiling so we fail fast rather than producing a truncated
//!    transcript.
//! 3. Call Candle's `pcm_to_mel` and return the log-mel buffer.
//!
//! `melfilters.bytes` is vendored from
//! `huggingface/candle/candle-examples/examples/whisper/melfilters.bytes`
//! under the same Apache-2.0 / MIT licence. We bundle it (rather
//! than fetch from HF Hub on first use) because the file is
//! deterministic, only 64 KB, and the cost of an extra network
//! round-trip on every fresh microapp is wildly disproportionate
//! to the asset weight.

#![cfg(feature = "stt-candle")]

use std::sync::OnceLock;

use candle_transformers::models::whisper as m;

use super::SttError;

/// Maximum input length in samples. Whisper is trained on 30-second
/// clips at 16 kHz — feeding more than that produces a truncated
/// transcript silently (the encoder only sees the first 30 s).
/// We reject longer clips up front; long-form support is a
/// Phase 92.x follow-up.
pub(crate) const MAX_SAMPLES: usize = m::N_SAMPLES;

/// Bundled 80-bin mel filterbank — 80 × 201 little-endian f32
/// coefficients. Vendored verbatim from Candle's examples dir.
const MEL_FILTERS_80_BYTES: &[u8] = include_bytes!("melfilters.bytes");

/// Whisper's 128-bin variant lives in
/// `candle-examples/examples/whisper/melfilters128.bytes`; only
/// `large-v3` uses 128 bins, and v1 of the migration ships with
/// the 80-bin `tiny`/`base`/`small` family. The asset can be
/// vendored later when 91.10 / a follow-up enables larger model
/// sizes.
const NUM_MEL_BINS_DEFAULT: usize = 80;

/// Lazy-parsed `Vec<f32>` view over the bundled bytes. Built once
/// per process — the filterbank is identical for every Whisper
/// invocation, so we amortise the 64 KB → 16 320 × f32 decode.
fn mel_filters_80() -> &'static [f32] {
    static CACHE: OnceLock<Vec<f32>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut out = vec![0f32; MEL_FILTERS_80_BYTES.len() / 4];
        for (i, chunk) in MEL_FILTERS_80_BYTES.chunks_exact(4).enumerate() {
            out[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        out
    })
}

/// Build a Whisper-canonical log-mel spectrogram from a 16 kHz
/// mono f32 PCM buffer.
///
/// The output is the flat `Vec<f32>` Candle expects: shape
/// `(n_frames × num_mel_bins)` row-major, ready to be wrapped in
/// a `Tensor` of shape `(1, num_mel_bins, n_frames)` at the call
/// site.
///
/// `num_mel_bins` MUST match the loaded model's
/// `Config::num_mel_bins`. v1 only ships the 80-bin variant; any
/// other value is rejected.
pub(crate) fn compute_log_mel_spectrogram(
    samples: &[f32],
    num_mel_bins: usize,
) -> Result<Vec<f32>, SttError> {
    if samples.is_empty() {
        return Err(SttError::EmptyAudio);
    }
    if samples.len() > MAX_SAMPLES {
        return Err(SttError::Decode(format!(
            "audio length {} samples exceeds Whisper's 30-second window ({} samples \
             @ 16 kHz) — split the clip before transcribing or wait for the \
             long-form Phase 92.x follow-up",
            samples.len(),
            MAX_SAMPLES,
        )));
    }
    if num_mel_bins != NUM_MEL_BINS_DEFAULT {
        return Err(SttError::Decode(format!(
            "Phase 91 v1 only ships the 80-bin mel filterbank; the model \
             requested {num_mel_bins} bins. Use a `tiny`/`base`/`small` Whisper \
             checkpoint or wait for the 128-bin asset (Phase 91 follow-up)"
        )));
    }

    // Synthesize the minimum-viable Whisper `Config` `pcm_to_mel`
    // expects. The helper only reads `num_mel_bins` from it; every
    // other field is irrelevant for the STFT path. We could thread
    // a real `Config` from the model load — for v1 that's
    // unnecessary indirection.
    let cfg = m::Config {
        num_mel_bins,
        ..whisper_tiny_config_stub()
    };

    Ok(m::audio::pcm_to_mel(&cfg, samples, mel_filters_80()))
}

/// Returns a `Config` with every field populated by Whisper-tiny
/// defaults. The values are not consulted by `pcm_to_mel` — but
/// `Config` is `#[non_exhaustive]`-style in Candle (every field
/// must be set on construction). Real inference loads the
/// authoritative `Config` from `config.json` shipped alongside
/// the SafeTensors weights.
fn whisper_tiny_config_stub() -> m::Config {
    m::Config {
        num_mel_bins: NUM_MEL_BINS_DEFAULT,
        max_source_positions: 1_500,
        d_model: 384,
        encoder_attention_heads: 6,
        encoder_layers: 4,
        vocab_size: 51_865,
        max_target_positions: 448,
        decoder_attention_heads: 6,
        decoder_layers: 4,
        suppress_tokens: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_audio() {
        let result = compute_log_mel_spectrogram(&[], 80);
        assert!(matches!(result, Err(SttError::EmptyAudio)));
    }

    #[test]
    fn over_30s_clip_rejects_with_decode_error() {
        let samples = vec![0f32; MAX_SAMPLES + 1];
        let err = compute_log_mel_spectrogram(&samples, 80).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("30-second window"), "got: {msg}");
        assert!(msg.contains("split"), "got: {msg}");
    }

    #[test]
    fn non_80_bin_request_rejects() {
        let samples = vec![0f32; 16_000]; // 1 second
        let err = compute_log_mel_spectrogram(&samples, 128).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("80-bin"), "got: {msg}");
        assert!(msg.contains("128"), "got: {msg}");
    }

    #[test]
    fn short_silence_produces_a_buffer() {
        // 1 s of silence — pcm_to_mel should still produce a
        // non-empty output. Exact shape depends on N_FFT /
        // HOP_LENGTH constants; what matters is "not empty".
        let samples = vec![0f32; 16_000];
        let mel = compute_log_mel_spectrogram(&samples, 80)
            .expect("1s silence is valid input");
        assert!(!mel.is_empty(), "mel buffer must be non-empty");
        // Length must be divisible by num_mel_bins so the caller
        // can build a `(1, num_mel_bins, frames)` Tensor cleanly.
        assert_eq!(mel.len() % 80, 0, "mel length must align to 80 bins");
    }

    #[test]
    fn mel_filters_decode_to_expected_count() {
        let filters = mel_filters_80();
        // Whisper-canonical filterbank is 80 × 201.
        assert_eq!(filters.len(), 80 * 201, "filterbank shape must be 80×201");
        // The first element is a sentinel — must not be NaN /
        // infinite from a botched decode.
        assert!(filters[0].is_finite());
    }
}
