//! Phase 91.4 — Candle Whisper inference backend.
//!
//! Mirrors the public surface of [`super::transcribe`] (the
//! whisper-rs legacy backend) byte-for-byte so the dispatch in
//! [`super::mod`] (lands in 91.6) can re-export either path
//! transparently. Specifically:
//!
//! - [`transcribe_file`] takes the same `Path` + `TranscribeConfig`
//!   shape and returns the same `Result<String, SttError>`.
//! - Audio decode chain (`ogg-opus → s16 PCM → f32`) is reused
//!   verbatim from `super::transcribe::decode_to_pcm_mono` — only
//!   the inference layer differs between backends.
//!
//! Internals at a glance (full design in
//! `PHASE-91-CANDLE-API-RESEARCH.md`):
//!
//! ```text
//! ogg bytes ─▶ opus decoder ─▶ s16 PCM ─▶ f32 ─▶ log-mel ─▶ Tensor (1,80,frames)
//!                                                                │
//!                                                                ▼
//!                                                  encoder.forward(mel)
//!                                                                │
//!                                                                ▼
//!                                        Greedy decoder loop (SOT + lang + TRANSCRIBE
//!                                        + NO_TIMESTAMPS → … → EOT)
//!                                                                │
//!                                                                ▼
//!                                            tokenizer.decode(generated_tokens) → String
//! ```

#![cfg(feature = "stt-candle")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use candle_core::{Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper as m;
use tokenizers::Tokenizer;

use super::mel;
use super::{Result, SttError, TranscribeConfig};

/// Special-token IDs resolved against the loaded tokenizer.
/// Candle exposes the Whisper special-token names as `&str`
/// constants (`<|startoftranscript|>`, `<|endoftext|>`, …); the
/// actual `u32` token IDs depend on the BPE table baked into the
/// shipped `tokenizer.json`, so we resolve them once at backend
/// load time and cache them here.
struct SpecialTokens {
    sot: u32,
    eot: u32,
    transcribe: u32,
    no_timestamps: u32,
}

/// Cached `(Whisper, Tokenizer, Config, Device)` per `model_path`.
///
/// First call to [`transcribe_file`] for a given path mmap-loads
/// the SafeTensors weights + parses `tokenizer.json` + `config.json`
/// (≈ 200 ms cold for whisper-tiny on Linux). Subsequent calls
/// hit the cache — same lifetime semantics as the whisper-rs
/// backend's `WhisperContext` cache so a downstream microapp
/// doesn't pay the load cost on every voice note.
///
/// Stored under `Arc<Mutex<…>>` so concurrent voice notes from
/// different chat turns share the same loaded model without
/// re-mmap'ing the 150 MB SafeTensors file per call.
pub(crate) struct CandleBackend {
    whisper: Mutex<m::model::Whisper>,
    tokenizer: Tokenizer,
    config: m::Config,
    device: Device,
    special: SpecialTokens,
}

static MODEL_CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, Arc<CandleBackend>>>> =
    std::sync::OnceLock::new();

/// Resolve a Candle backend for `model_dir`. Loads on first call,
/// hits the process-global cache on subsequent calls.
///
/// `model_dir` is expected to contain:
/// - `model.safetensors` (or `model.fp32.safetensors`) — Whisper
///   encoder + decoder weights.
/// - `tokenizer.json` — BPE tokenizer (the Whisper-canonical one).
/// - `config.json` — `m::Config` source-of-truth.
///
/// Same layout HuggingFace ships under `openai/whisper-tiny`
/// (Phase 91.7 wires the auto-fetch when `model_path` is empty).
fn load_backend(model_dir: &Path) -> Result<Arc<CandleBackend>> {
    let cache = MODEL_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("model cache mutex poisoned");
    if let Some(existing) = guard.get(model_dir) {
        return Ok(Arc::clone(existing));
    }

    // Resolve the three required files. Be explicit about which
    // is missing so the operator can act — silent fallbacks make
    // "why does my model not load" a hard ticket.
    if !model_dir.exists() {
        return Err(SttError::ModelMissing(format!(
            "{} — directory does not exist",
            model_dir.display()
        )));
    }

    let config_path = model_dir.join("config.json");
    if !config_path.is_file() {
        return Err(SttError::ModelMissing(format!(
            "{} — config.json not found (expected SafeTensors model directory)",
            model_dir.display()
        )));
    }
    let tokenizer_path = model_dir.join("tokenizer.json");
    if !tokenizer_path.is_file() {
        return Err(SttError::ModelMissing(format!(
            "{} — tokenizer.json not found",
            model_dir.display()
        )));
    }
    let weights_path = find_safetensors(model_dir).ok_or_else(|| {
        SttError::ModelMissing(format!(
            "{} — model.safetensors (or .fp32.safetensors) not found",
            model_dir.display()
        ))
    })?;

    let config_bytes = std::fs::read(&config_path)
        .map_err(|e| SttError::Whisper(format!("reading config.json: {e}")))?;
    let config: m::Config = serde_json::from_slice(&config_bytes)
        .map_err(|e| SttError::Whisper(format!("parsing config.json: {e}")))?;

    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| SttError::Whisper(format!("loading tokenizer.json: {e}")))?;

    // Device selection — v1 is CPU-only. GPU backends opt in via
    // Phase 91.10 features (`stt-candle-metal`, `stt-candle-cuda`,
    // `stt-candle-accelerate`). The cfg blocks land in 91.10; for
    // now hard-pin CPU so cross-compile to every target stays
    // pure-Rust.
    let device = Device::Cpu;

    // SAFETY: `from_mmaped_safetensors` requires the caller to
    // guarantee the file isn't truncated/modified for the lifetime
    // of the `VarBuilder`. We keep the resulting `Whisper` cached
    // for the process lifetime and the file lives on the local
    // disk (downloaded by HF Hub or operator-supplied) — neither
    // shrinks under us. Same usage pattern as the upstream Candle
    // Whisper example.
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&[&weights_path], m::DTYPE, &device).map_err(|e| {
            SttError::Whisper(format!(
                "mmap'ing SafeTensors at {}: {e}",
                weights_path.display()
            ))
        })?
    };
    let whisper = m::model::Whisper::load(&vb, config.clone())
        .map_err(|e| SttError::Whisper(format!("constructing Whisper from VarBuilder: {e}")))?;

    let special = SpecialTokens {
        sot: resolve_special_token(&tokenizer, m::SOT_TOKEN)?,
        eot: resolve_special_token(&tokenizer, m::EOT_TOKEN)?,
        transcribe: resolve_special_token(&tokenizer, m::TRANSCRIBE_TOKEN)?,
        no_timestamps: resolve_special_token(&tokenizer, m::NO_TIMESTAMPS_TOKEN)?,
    };

    let backend = Arc::new(CandleBackend {
        whisper: Mutex::new(whisper),
        tokenizer,
        config,
        device,
        special,
    });
    guard.insert(model_dir.to_path_buf(), Arc::clone(&backend));
    Ok(backend)
}

/// Resolve a Whisper special-token (`<|startoftranscript|>`, etc.)
/// to its `u32` BPE id. Surfaces a clear error if the loaded
/// tokenizer doesn't carry the token — a sign someone shipped a
/// mismatched `tokenizer.json` next to the weights.
fn resolve_special_token(tokenizer: &Tokenizer, name: &str) -> Result<u32> {
    tokenizer.token_to_id(name).ok_or_else(|| {
        SttError::Whisper(format!(
            "tokenizer.json missing the {name:?} special token — \
             likely a non-Whisper checkpoint shipped under model_path"
        ))
    })
}

fn find_safetensors(model_dir: &Path) -> Option<PathBuf> {
    // Prefer the canonical filename HF Hub ships first; fall back
    // to the explicit `fp32` variant some operators export when
    // they want to disambiguate from a quantised companion file.
    for name in &["model.safetensors", "model.fp32.safetensors"] {
        let p = model_dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Resolve the directory that holds the SafeTensors model weights
/// + `tokenizer.json` + `config.json`.
///
/// Precedence:
///
/// 1. `cfg.model_path` is non-empty → use it verbatim (air-gapped
///    operator deployment). The path may point at a directory or
///    at the SafeTensors file directly; we normalise to the parent
///    directory either way.
/// 2. `cfg.model_path` is empty + `cfg.model_id` is set →
///    `hf-hub` fetch into `~/.cache/huggingface/hub/`. Three
///    files: `model.safetensors`, `tokenizer.json`, `config.json`.
/// 3. Both empty → [`SttError::ModelMissing`] with both knob
///    names quoted so the operator knows which to fix.
async fn resolve_model_dir(cfg: &TranscribeConfig) -> Result<PathBuf> {
    // Treat an empty PathBuf (`PathBuf::from("")`) the same as
    // an unset field. `TranscribeConfig::default()` populates a
    // sentinel non-empty path; explicit empties signal "use
    // HF Hub".
    let path_is_empty = cfg.model_path.as_os_str().is_empty();

    if !path_is_empty {
        let p = cfg.model_path.clone();
        // Operator may point at `model.safetensors` directly OR
        // at the directory; normalise to the directory so the
        // tokenizer + config lookups in `load_backend` find
        // their files.
        if p.is_file() {
            return Ok(p
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")));
        }
        return Ok(p);
    }

    let model_id = cfg.model_id.as_deref().ok_or_else(|| {
        SttError::ModelMissing(
            "neither `model_path` nor `model_id` is set on TranscribeConfig — \
             provide either a local SafeTensors directory or a HuggingFace \
             Hub repo id (e.g. \"openai/whisper-tiny\")"
                .into(),
        )
    })?;

    fetch_from_hf_hub(model_id).await
}

/// Fetch the three required Whisper assets from HuggingFace Hub
/// into the user's cache (typically `~/.cache/huggingface/hub/`)
/// and return the directory holding them.
///
/// Cfg-gated on `stt-candle-hub` because `hf-hub` transitively
/// requires `tokio` with the `net` feature, which doesn't
/// compile on `wasm32-unknown-unknown`. WASM consumers build
/// `stt-candle` minus `stt-candle-hub` and pin
/// `TranscribeConfig::model_path` to a bundled SafeTensors
/// directory; the function signature below stays identical so
/// the caller doesn't branch on the feature flag.
#[cfg(feature = "stt-candle-hub")]
async fn fetch_from_hf_hub(model_id: &str) -> Result<PathBuf> {
    use hf_hub::api::tokio::Api;

    let api = Api::new()
        .map_err(|e| SttError::Whisper(format!("hf-hub Api init: {e}")))?;
    let repo = api.model(model_id.to_string());

    tracing::info!(
        target: "stt.candle.hf_hub",
        repo = model_id,
        "fetching Whisper assets from HuggingFace Hub (first run downloads ~150 MB)"
    );

    let weights = repo
        .get("model.safetensors")
        .await
        .map_err(|e| SttError::Whisper(format!("hf-hub fetch model.safetensors: {e}")))?;
    // The three assets land in the same snapshot directory under
    // the HF Hub cache, so the parent of the SafeTensors file
    // already holds `tokenizer.json` + `config.json`. Fetch them
    // anyway to make sure the cache row is complete; the calls
    // are no-ops when the files are already present.
    let _tokenizer = repo
        .get("tokenizer.json")
        .await
        .map_err(|e| SttError::Whisper(format!("hf-hub fetch tokenizer.json: {e}")))?;
    let _config = repo
        .get("config.json")
        .await
        .map_err(|e| SttError::Whisper(format!("hf-hub fetch config.json: {e}")))?;

    let dir = weights
        .parent()
        .ok_or_else(|| {
            SttError::Whisper(format!(
                "hf-hub returned weights path with no parent directory: {}",
                weights.display()
            ))
        })?
        .to_path_buf();
    Ok(dir)
}

/// Fallback when the `stt-candle-hub` sub-feature is disabled
/// (e.g. WASM builds). Surfaces a clear actionable error so the
/// operator knows they need to either:
/// - set `TranscribeConfig::model_path` to a local SafeTensors
///   directory (the WASM-correct path), OR
/// - rebuild on a native target with
///   `--features stt-candle,stt-candle-hub` so the auto-fetch
///   compiles.
#[cfg(not(feature = "stt-candle-hub"))]
async fn fetch_from_hf_hub(model_id: &str) -> Result<PathBuf> {
    Err(SttError::ModelMissing(format!(
        "TranscribeConfig.model_id is set to {model_id:?} but the \
         `stt-candle-hub` Cargo feature is disabled — auto-fetch from \
         HuggingFace Hub is unavailable on this build (typical when \
         targeting WASM, where `hf-hub` doesn't compile). Either set \
         `TranscribeConfig.model_path` to a local directory holding \
         model.safetensors + tokenizer.json + config.json, or rebuild \
         with `--features stt-candle,stt-candle-hub` on a native target."
    )))
}

/// Transcribe the audio at `path` using `cfg`. Returns the
/// trimmed transcript.
///
/// Wire-equivalent to [`super::transcribe::transcribe_file`] —
/// callers should not need to know which backend is active.
///
/// Empty audio after the audio-decode chain →
/// [`SttError::EmptyAudio`]. Empty Whisper output →
/// [`SttError::EmptyTranscript`].
pub async fn transcribe_file(path: &Path, cfg: &TranscribeConfig) -> Result<String> {
    let started = std::time::Instant::now();

    // Reuse the shared audio-decode chain so we share the
    // ogg-opus parser + the resample step. Only the inference
    // layer is backend-specific.
    let pcm = super::audio::decode_to_pcm_mono(path, cfg).await?;
    if pcm.is_empty() {
        return Err(SttError::EmptyAudio);
    }
    let samples = super::audio::pcm_s16_to_f32(&pcm);

    // Phase 91.7 — resolve the model directory. Either the
    // operator points at a local SafeTensors layout via
    // `model_path`, or sets `model_id` and we auto-fetch from
    // HuggingFace Hub on first call. Both empty is a hard error
    // — boot paths must not silently no-op.
    let model_dir = resolve_model_dir(cfg).await?;

    let lang_hint = cfg.lang_hint.clone();
    let transcript = tokio::task::spawn_blocking(move || -> Result<String> {
        let backend = load_backend(&model_dir)?;
        run_inference(&backend, &samples, lang_hint.as_deref())
    })
    .await
    .map_err(|e| SttError::Whisper(format!("transcribe_candle join: {e}")))??;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "stt.candle",
        path = %path.display(),
        transcript_len = transcript.len(),
        elapsed_ms,
        "stt: candle transcription ok",
    );
    if transcript.is_empty() {
        return Err(SttError::EmptyTranscript);
    }
    Ok(transcript)
}

/// Run the encoder + greedy decoder loop. Returns the trimmed
/// transcript with special tokens stripped.
///
/// Synchronous on purpose — caller wraps in `spawn_blocking` so
/// the inference cost doesn't block the tokio runtime.
fn run_inference(
    backend: &CandleBackend,
    samples: &[f32],
    lang_hint: Option<&str>,
) -> Result<String> {
    let num_mel_bins = backend.config.num_mel_bins;
    let mel_buffer = mel::compute_log_mel_spectrogram(samples, num_mel_bins)?;
    let mel_len = mel_buffer.len();
    let mel_tensor = Tensor::from_vec(
        mel_buffer,
        (1, num_mel_bins, mel_len / num_mel_bins),
        &backend.device,
    )
    .map_err(|e| SttError::Whisper(format!("building mel Tensor: {e}")))?;

    let mut whisper = backend
        .whisper
        .lock()
        .expect("whisper inference mutex poisoned");

    let audio_features = whisper
        .encoder
        .forward(&mel_tensor, true)
        .map_err(|e| SttError::Whisper(format!("encoder forward: {e}")))?;

    // Build the Whisper prompt: SOT + (optional <|lang|>) +
    // TRANSCRIBE + NO_TIMESTAMPS. Skip the language token when
    // the caller didn't supply a hint OR explicitly asked for
    // auto-detect — Whisper internally infers in that case.
    let mut prompt: Vec<u32> = vec![backend.special.sot];
    if let Some(l) = lang_hint.filter(|l| !l.is_empty() && *l != "auto") {
        // Whisper's BCP-47 hints are wrapped as `<|<code>|>` — we
        // coerce the operator-supplied hint to lowercase and
        // split off the region subtag (BCP-47 `es-AR` → `es`)
        // because the tokenizer only ships language-level
        // tokens.
        let base = l.split(|c| c == '-' || c == '_').next().unwrap_or(l);
        let token = format!("<|{}|>", base.to_lowercase());
        let id = backend.tokenizer.token_to_id(&token).ok_or_else(|| {
            SttError::Whisper(format!(
                "tokenizer rejected language hint {l:?} (looked up token {token:?})"
            ))
        })?;
        prompt.push(id);
    }
    prompt.push(backend.special.transcribe);
    prompt.push(backend.special.no_timestamps);

    // Track where the prompt ends so we can slice it off the
    // generated tokens before decoding.
    let prompt_len = prompt.len();

    // Greedy decoder loop. Cap iterations at
    // `max_target_positions` to defend against a runaway model
    // that never emits EOT.
    let max_new = backend.config.max_target_positions.saturating_sub(prompt_len);
    let mut tokens = prompt;
    for step in 0..max_new {
        let tokens_t = Tensor::new(&tokens[..], &backend.device)
            .map_err(|e| SttError::Whisper(format!("token Tensor: {e}")))?
            .unsqueeze(0)
            .map_err(|e| SttError::Whisper(format!("unsqueeze: {e}")))?;
        let ys = whisper
            .decoder
            .forward(&tokens_t, &audio_features, step == 0)
            .map_err(|e| SttError::Whisper(format!("decoder forward step {step}: {e}")))?;
        let last_step = ys
            .dim(1)
            .map_err(|e| SttError::Whisper(format!("ys.dim(1): {e}")))?
            - 1;
        let logits = whisper
            .decoder
            .final_linear(
                &ys.i((.., last_step))
                    .map_err(|e| SttError::Whisper(format!("ys index: {e}")))?,
            )
            .map_err(|e| SttError::Whisper(format!("final_linear: {e}")))?
            .i(0)
            .map_err(|e| SttError::Whisper(format!("logits.i(0): {e}")))?;
        let next = logits
            .argmax(0)
            .map_err(|e| SttError::Whisper(format!("argmax: {e}")))?
            .to_scalar::<u32>()
            .map_err(|e| SttError::Whisper(format!("argmax to_scalar: {e}")))?;
        if next == backend.special.eot {
            break;
        }
        tokens.push(next);
    }

    // Strip the prompt and decode the generated body. The
    // tokenizer is responsible for normalising the byte-level
    // BPE back to UTF-8.
    let generated = &tokens[prompt_len..];
    let raw = backend
        .tokenizer
        .decode(generated, true)
        .map_err(|e| SttError::Whisper(format!("tokenizer decode: {e}")))?;
    Ok(raw.trim().to_string())
}

#[allow(dead_code)] // exercised once the dispatch in 91.6 lands
const fn _compile_time_marker() -> &'static str {
    "phase-91.4-candle-inference"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to extract the `SttError` from a `load_backend`
    /// call. `Arc<CandleBackend>` doesn't derive `Debug`, so we
    /// can't use `.unwrap_err()` directly — match instead.
    fn expect_load_err(result: Result<Arc<CandleBackend>>) -> SttError {
        match result {
            Ok(_) => panic!("load_backend must fail for this input"),
            Err(e) => e,
        }
    }

    #[test]
    fn load_backend_missing_directory_errors_with_path() {
        // Hits the early `model_dir.exists()` branch; no Candle
        // load attempted. Validates the operator-facing error
        // message carries the missing path.
        let p = Path::new("/definitely-not-a-real-dir/whisper-tiny");
        let err = expect_load_err(load_backend(p));
        assert!(matches!(err, SttError::ModelMissing(_)));
        let msg = err.to_string();
        assert!(msg.contains("does not exist"), "got: {msg}");
        assert!(msg.contains("/definitely-not-a-real-dir"), "got: {msg}");
    }

    #[test]
    fn load_backend_missing_config_json_errors_with_hint() {
        let tmp = tempfile::tempdir().unwrap();
        // tokenizer.json + safetensors present but no config.json.
        std::fs::write(tmp.path().join("tokenizer.json"), b"{}").unwrap();
        std::fs::write(tmp.path().join("model.safetensors"), b"").unwrap();
        let err = expect_load_err(load_backend(tmp.path()));
        let msg = err.to_string();
        assert!(msg.contains("config.json not found"), "got: {msg}");
    }

    /// Build a `TranscribeConfig` with the two model-locator
    /// knobs (`model_path`, `model_id`) controllable per-test.
    /// Other fields populated with the same defaults the YAML
    /// loader applies.
    fn cfg_with_locators(model_path: PathBuf, model_id: Option<&str>) -> TranscribeConfig {
        #[allow(deprecated)] // populate the legacy `ffmpeg_path` for backwards compat
        TranscribeConfig {
            model_path,
            lang_hint: None,
            ffmpeg_path: PathBuf::from("ffmpeg"),
            target_sample_rate: 16_000,
            model_id: model_id.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn resolve_model_dir_with_both_locators_empty_fails_fast() {
        // No `model_path`, no `model_id` — the resolver must
        // refuse to silently no-op and surface a clear hint that
        // names both knobs.
        let cfg = cfg_with_locators(PathBuf::new(), None);
        let err = match resolve_model_dir(&cfg).await {
            Ok(p) => panic!("resolver must fail-fast; got Ok({})", p.display()),
            Err(e) => e,
        };
        assert!(matches!(err, SttError::ModelMissing(_)));
        let msg = err.to_string();
        assert!(msg.contains("model_path"), "must name model_path: {msg}");
        assert!(msg.contains("model_id"), "must name model_id: {msg}");
    }

    #[tokio::test]
    async fn resolve_model_dir_with_directory_returns_it_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_locators(tmp.path().to_path_buf(), None);
        let resolved = resolve_model_dir(&cfg).await.expect("directory path");
        assert_eq!(resolved, tmp.path());
    }

    #[tokio::test]
    async fn resolve_model_dir_with_file_returns_parent() {
        // Operator may pin `model_path` at the SafeTensors file
        // directly; the resolver must normalise to the parent so
        // the tokenizer + config lookups in `load_backend` find
        // their files in the same directory.
        let tmp = tempfile::tempdir().unwrap();
        let weights = tmp.path().join("model.safetensors");
        std::fs::write(&weights, b"").unwrap();
        let cfg = cfg_with_locators(weights, None);
        let resolved = resolve_model_dir(&cfg).await.expect("file path");
        assert_eq!(resolved, tmp.path());
    }

    #[test]
    fn load_backend_missing_safetensors_errors_with_hint() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("config.json"), b"{}").unwrap();
        std::fs::write(tmp.path().join("tokenizer.json"), b"{}").unwrap();
        let err = expect_load_err(load_backend(tmp.path()));
        let msg = err.to_string();
        assert!(msg.contains("model.safetensors"), "got: {msg}");
    }
}
