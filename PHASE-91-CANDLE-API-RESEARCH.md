# Phase 91.1 — Candle Whisper API research notes

Output of sub-phase 91.1 (the "spec lock-in" research step from
[`PHASE-91-STT-CANDLE-MIGRATION-PLAN.md`](PHASE-91-STT-CANDLE-MIGRATION-PLAN.md)).
Captures the concrete crate paths, constants, and call shapes the
follow-on sub-phases (91.2–91.7) will wire up so the
implementation steps don't re-derive the API.

## Crate paths used

```rust
use candle_core::{Device, IndexOp, Tensor};
use candle_nn::{ops::{log_softmax, softmax}, VarBuilder};
use candle_transformers::models::whisper::{self as m, audio, Config};
// Quantized variant (GGUF) lives in a separate VarBuilder:
use candle_transformers::quantized_var_builder::VarBuilder as QuantizedVB;
```

Source: [huggingface/candle / `candle-examples/examples/whisper/main.rs`](https://github.com/huggingface/candle/blob/main/candle-examples/examples/whisper/main.rs).

## Model loading

Two parallel code paths depending on the weights format:

```rust
// SafeTensors (full precision — what `openai/whisper-tiny` ships):
let vb = unsafe {
    VarBuilder::from_mmaped_safetensors(&[weights_path], m::DTYPE, &device)?
};
let model = m::model::Whisper::load(&vb, config)?;

// GGUF (quantized — future follow-up, not v1):
let vb = QuantizedVB::from_gguf(&weights_path, &device)?;
let model = m::quantized_model::Whisper::load(&vb, config)?;
```

`m::DTYPE` is `candle_core::DType::F32` for the standard path.

## Audio pipeline — **`m::audio::pcm_to_mel` already exists**

**Spec 91.3 simplification:** the plan assumed we'd hand-roll the
STFT + mel filterbank via `rustfft`. Candle's
`candle_transformers::models::whisper::audio::pcm_to_mel()` already
returns the log-mel buffer Candle expects, using the same Whisper
constants (`N_FFT=400`, `HOP_LENGTH=160`, 80 mel bins). We feed it
f32 PCM at 16 kHz and skip the rustfft dependency entirely.

```rust
let mel = m::audio::pcm_to_mel(&config, &samples_f32, &mel_filters)?;
let mel_len = mel.len();
let mel_tensor = Tensor::from_vec(
    mel,
    (1, config.num_mel_bins, mel_len / config.num_mel_bins),
    &device,
)?;
```

`mel_filters` is a precomputed 80×201 filterbank distributed with
the Candle examples (`mel_filters.bytes`); we either ship it
alongside the model assets or compute it once at boot (it's
deterministic from Whisper constants).

## Inference loop

```rust
// 1. Encode the audio chunk:
let audio_features = model.encoder_forward(&mel_tensor, true)?;

// 2. Seed tokens with the start-of-transcript + language + task markers:
let mut tokens: Vec<u32> = vec![m::SOT_TOKEN];
if let Some(lang_tag) = language_tag {
    tokens.push(tokenizer.token_to_id(lang_tag)?);
}
tokens.push(m::TRANSCRIBE_TOKEN);   // or TRANSLATE_TOKEN for "translate"
tokens.push(m::NO_TIMESTAMPS_TOKEN);

// 3. Step the decoder one token at a time, append, repeat until EOT:
for i in 0.. {
    let tokens_t = Tensor::new(&tokens[..], &device)?.unsqueeze(0)?;
    let ys = model.decoder_forward(&tokens_t, &audio_features, i == 0)?;
    let logits = model.decoder_final_linear(&ys.i((.., ys.dim(1)? - 1))?)?
        .i(0)?.i(0)?;
    let next = logits.argmax(0)?.to_scalar::<u32>()?;   // Greedy
    if next == m::EOT_TOKEN { break; }
    tokens.push(next);
}

// 4. Decode token IDs to text via `tokenizers` crate:
let text = tokenizer.decode(&tokens, true)?;
```

**Greedy sampling** (`argmax` on logits) matches the
`whisper-rs::SamplingStrategy::Greedy { best_of: 1 }` we use today.
Candle also exposes a temperature-fallback chain via
`m::TEMPERATURES` — out of scope for v1; BeamSearch parity is
Phase 92.x.

## Constants we'll lean on

| Constant | Value | Notes |
|---|---|---|
| `m::SAMPLE_RATE` | `16_000` | matches our existing `TranscribeConfig::target_sample_rate` |
| `m::N_FFT` | `400` | STFT window size |
| `m::HOP_LENGTH` | `160` | 10 ms hop @ 16 kHz |
| `m::CHUNK_LENGTH` | `30` (seconds) | max audio chunk per inference |
| `m::N_FRAMES` | `3000` | mel frames per 30-second chunk |
| `m::N_SAMPLES` | `480_000` | 30 s × 16 kHz |
| `m::DTYPE` | `DType::F32` | weights dtype |
| `m::SOT_TOKEN`, `m::EOT_TOKEN` | — | start / end of transcript |
| `m::TRANSCRIBE_TOKEN`, `m::TRANSLATE_TOKEN` | — | task selector |
| `m::NO_TIMESTAMPS_TOKEN` | — | suppress segment-level timestamps |
| `m::NO_SPEECH_TOKEN`, `m::NO_SPEECH_TOKENS` | — | silence detection |

## Model assets — `openai/whisper-tiny` (v1 default)

Hugging Face Hub repo: <https://huggingface.co/openai/whisper-tiny>.

Files Candle needs:

| File | Size | Purpose |
|---|---|---|
| `model.safetensors` | 151 MB | model weights |
| `tokenizer.json` | 2.48 MB | tokenizer (BPE) |
| `config.json` | 2 KB | `m::Config` deserialization source |

Optional ship-time alternative: `whisper-tiny.en` (English-only,
same size). v1 picks the multilingual default to keep parity with
the current `whisper-rs` `lang_hint` behaviour.

**Effective replacement for `ggml-tiny-q5_1.bin` (40 MB on disk)**:
the SafeTensors file is larger (~150 MB vs 40 MB) because we're
not yet using a quantized variant. Quantized GGUF Whisper-tiny via
`candle-transformers::quantized_model` is a follow-up — drops to
~75 MB at int8 with negligible WER cost. Sub-phase 91.10 GPU
features land alongside it.

## Open decisions for sub-phases 91.2–91.7

1. **Mel filter banks** — ship pre-computed `mel_filters_80.bytes`
   asset alongside the model dir, OR compute once at boot from
   Whisper constants. Decision: **compute once at boot**
   (deterministic, eliminates an asset to ship + download).
2. **Tokenizer load path** — eager at `transcribe_file` first call
   vs `OnceCell` cached across invocations. Decision: **OnceCell
   cache keyed on `tokenizer_path`** mirroring whisper-rs
   `WhisperContext` cache.
3. **Device selection** — `Device::Cpu` only for v1. GPU
   (Metal/CUDA/WGPU) opt-in via 91.10 feature flags. Compile-time
   `#[cfg(feature = "stt-candle-metal")]` etc. drives device
   construction.
4. **Language hint mapping** — `cfg.lang_hint = Some("es")` →
   `tokenizer.token_to_id("<|es|>")`. Candle's tokenizer ships
   these language tag tokens; we just need the mapping table.
   None → omit the language token (Whisper auto-detects).
5. **Audio chunking for clips > 30 s** — current
   `whisper-rs::full` accepts arbitrary length, internally chunks.
   Candle requires manual chunking. Decision: enforce 30 s ceiling
   v1 (matches the WhatsApp / Telegram voice-note use case);
   clip-too-long → `SttError::Decode("audio > 30s — split before
   transcribing")`. Long-form support is Phase 92.x.

## Sources

- [huggingface/candle — main repo](https://github.com/huggingface/candle)
- [candle whisper example main.rs](https://github.com/huggingface/candle/blob/main/candle-examples/examples/whisper/main.rs)
- [docs.rs candle_transformers::models::whisper](https://docs.rs/candle-transformers/latest/candle_transformers/models/whisper/index.html)
- [openai/whisper-tiny model card](https://huggingface.co/openai/whisper-tiny)
- [openai/whisper-tiny tree](https://huggingface.co/openai/whisper-tiny/tree/main)
- [wavey-ai/mel-spec — whisper.cpp-compatible mel spectrogram (reference)](https://github.com/wavey-ai/mel-spec)
- [cool-japan/oxiwhisper — pure-Rust alternative (reference)](https://github.com/cool-japan/oxiwhisper)
