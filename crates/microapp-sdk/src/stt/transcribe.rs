//! Whisper-backed transcription for short voice notes.
//!
//! Pipeline (matches the legacy `agent-creator-microapp::stt` shape):
//! 1. Read audio file from disk.
//! 2. ffmpeg → mono PCM s16le at the configured sample rate.
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
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
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

/// Pipe the source through ffmpeg → s16le mono at the configured
/// sample rate, capture raw PCM (no WAV header — `-f s16le`).
async fn decode_to_pcm_mono(path: &Path, cfg: &TranscribeConfig) -> Result<Vec<u8>> {
    let bytes = tokio::fs::read(path).await?;
    let sample_rate = cfg.target_sample_rate.to_string();
    let mut child = tokio::process::Command::new(&cfg.ffmpeg_path)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-ac",
            "1",
            "-ar",
            sample_rate.as_str(),
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            SttError::Ffmpeg(format!(
                "spawn {} (is it installed?): {e}",
                cfg.ffmpeg_path.display()
            ))
        })?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| SttError::Ffmpeg("stdin missing".into()))?;
        tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
            drop(stdin);
        });
    }
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SttError::Ffmpeg(format!(
            "exit {:?}: {stderr}",
            output.status.code()
        )));
    }
    Ok(output.stdout)
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
