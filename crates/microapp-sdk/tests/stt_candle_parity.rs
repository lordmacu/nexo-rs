//! Phase 91.8 — parity tests: Candle backend vs whisper-rs legacy.
//!
//! Runs the two STT backends side-by-side on the same audio
//! fixtures and computes Word Error Rate (WER) between their
//! transcripts. Build flags:
//!
//! ```bash
//! # Activate both backends so the test binary can call into
//! # `transcribe::transcribe_file` AND `transcribe_candle::transcribe_file`
//! # within the same process. The `--features stt,stt-candle` build
//! # is a parity-test-only configuration — production deployments
//! # pick exactly one backend.
//! cargo nextest run -p nexo-microapp-sdk \
//!   --features stt,stt-candle \
//!   --no-default-features \
//!   --ignored \
//!   --test stt_candle_parity
//! ```
//!
//! Marked `#[ignore]` because each run mmap-loads a 150 MB
//! whisper-tiny SafeTensors + downloads a GGML `.bin` for the
//! legacy backend; that's wildly disproportionate to a normal
//! `cargo test` cycle. Run the parity suite manually when:
//!
//! - landing a Candle backend change (91.4 inference, 91.5
//!   tokenizer, 91.7 HF Hub fetch).
//! - cutting a release candidate that targets a Candle bump.
//! - investigating a transcript-quality regression report.
//!
//! Required local assets (paths configured via env vars below):
//!
//! - `NEXO_STT_PARITY_WHISPER_GGML` — path to a GGML model
//!   compatible with whisper-rs (e.g. `ggml-tiny-q5_1.bin` from
//!   <https://huggingface.co/ggerganov/whisper.cpp>).
//! - `NEXO_STT_PARITY_SAFETENSORS_DIR` — directory containing
//!   `model.safetensors` + `tokenizer.json` + `config.json` for
//!   the same Whisper size (e.g.
//!   `openai/whisper-tiny` from
//!   <https://huggingface.co/openai/whisper-tiny>).
//! - `NEXO_STT_PARITY_FIXTURES_DIR` — directory with the audio
//!   files listed in [`FIXTURES`] below. Each fixture is a
//!   ≤ 30-second ogg-opus voice note (the same format
//!   WhatsApp / Telegram ship).

#![cfg(all(feature = "stt", feature = "stt-candle"))]

use std::path::{Path, PathBuf};

use nexo_microapp_sdk::stt::{transcribe, transcribe_candle, TranscribeConfig};

/// Audio fixtures the parity check should agree on. Each value
/// is a `(filename, language_hint, max_acceptable_wer)` triple.
/// Pick clean speech with low background noise; conversational
/// crosstalk widens the WER tolerance well past the 5 % cap the
/// plan asks for.
const FIXTURES: &[(&str, &str, f64)] = &[
    ("voice-note-es.ogg", "es", 0.05),
    ("voice-note-en.ogg", "en", 0.05),
    ("voice-note-mixed.ogg", "es", 0.10),
];

/// Word Error Rate between two transcripts. Uses straight
/// Levenshtein distance on the whitespace-tokenised word lists —
/// good enough for the comparison-grade signal we need. A purpose-
/// built WER crate would add a dep for a single use case; this is
/// ~30 LOC of standard DP that we own outright.
fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let r: Vec<&str> = reference.split_whitespace().collect();
    let h: Vec<&str> = hypothesis.split_whitespace().collect();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    // Wagner-Fischer Levenshtein on token sequences.
    let n = r.len();
    let m = h.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if r[i - 1] == h[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1) // deletion
                .min(curr[j - 1] + 1) // insertion
                .min(prev[j - 1] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m] as f64 / n as f64
}

fn locate_fixtures_dir() -> Option<PathBuf> {
    std::env::var("NEXO_STT_PARITY_FIXTURES_DIR")
        .ok()
        .map(PathBuf::from)
}

fn locate_ggml_path() -> Option<PathBuf> {
    std::env::var("NEXO_STT_PARITY_WHISPER_GGML")
        .ok()
        .map(PathBuf::from)
}

fn locate_safetensors_dir() -> Option<PathBuf> {
    std::env::var("NEXO_STT_PARITY_SAFETENSORS_DIR")
        .ok()
        .map(PathBuf::from)
}

#[allow(deprecated)] // populates the legacy `ffmpeg_path` field
fn build_cfg(model_path: PathBuf, lang_hint: &str) -> TranscribeConfig {
    TranscribeConfig {
        model_path,
        lang_hint: Some(lang_hint.into()),
        ffmpeg_path: PathBuf::from("ffmpeg"),
        target_sample_rate: 16_000,
        model_id: None,
    }
}

async fn transcribe_with_each(
    fixture: &Path,
    lang: &str,
    ggml: &Path,
    safetensors_dir: &Path,
) -> (String, String) {
    let cpp_cfg = build_cfg(ggml.to_path_buf(), lang);
    let cpp = transcribe::transcribe_file(fixture, &cpp_cfg)
        .await
        .unwrap_or_else(|e| panic!("whisper-rs failed on {}: {e}", fixture.display()));

    let candle_cfg = build_cfg(safetensors_dir.to_path_buf(), lang);
    let candle = transcribe_candle::transcribe_file(fixture, &candle_cfg)
        .await
        .unwrap_or_else(|e| panic!("candle failed on {}: {e}", fixture.display()));

    (cpp, candle)
}

#[tokio::test]
#[ignore = "requires NEXO_STT_PARITY_* env vars + ~200 MB of local models"]
async fn parity_within_wer_threshold() {
    let fixtures_dir = locate_fixtures_dir().expect(
        "set NEXO_STT_PARITY_FIXTURES_DIR to a directory of voice-note fixtures",
    );
    let ggml = locate_ggml_path().expect(
        "set NEXO_STT_PARITY_WHISPER_GGML to a GGML whisper-tiny file (e.g. \
         ggml-tiny-q5_1.bin)",
    );
    let safetensors_dir = locate_safetensors_dir().expect(
        "set NEXO_STT_PARITY_SAFETENSORS_DIR to the openai/whisper-tiny \
         SafeTensors directory",
    );

    let mut failures: Vec<String> = Vec::new();
    for (name, lang, max_wer) in FIXTURES {
        let fixture = fixtures_dir.join(name);
        if !fixture.exists() {
            failures.push(format!("missing fixture: {}", fixture.display()));
            continue;
        }
        let (cpp, candle) = transcribe_with_each(&fixture, lang, &ggml, &safetensors_dir).await;
        let wer = word_error_rate(&cpp, &candle);
        eprintln!(
            "[parity] fixture={name} lang={lang} wer={wer:.3} cpp={cpp:?} candle={candle:?}"
        );
        if wer > *max_wer {
            failures.push(format!(
                "{name}: WER {wer:.3} > threshold {max_wer:.3}\n  whisper-rs: {cpp:?}\n  candle:     {candle:?}"
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "STT parity failed on {} fixture(s):\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn wer_helper_identical_inputs_score_zero() {
    assert_eq!(
        word_error_rate("hello world how are you", "hello world how are you"),
        0.0
    );
}

#[test]
fn wer_helper_single_substitution_scores_one_over_n() {
    // 5 words, 1 wrong → 1/5 = 0.2
    let wer = word_error_rate("hello world how are you", "hello world how were you");
    assert!((wer - 0.2).abs() < 1e-9, "expected 0.2, got {wer}");
}

#[test]
fn wer_helper_empty_reference_returns_zero_or_one() {
    assert_eq!(word_error_rate("", ""), 0.0);
    assert_eq!(word_error_rate("", "garbage"), 1.0);
}
