# Phase 91 — STT pure-Rust migration via Candle

**Goal:** Replace the `whisper-rs` (whisper.cpp C++ binding) backend
in `nexo-microapp-sdk::stt` with HuggingFace's pure-Rust Candle ML
framework. Eliminates the Visual Studio Build Tools 2022 + CMake
requirement on Windows and unblocks trivial Android NDK / WASM
cross-compile (precondition for the Flutter Android FFI embed
goal). The audio decode pipeline (ogg-opus → s16 PCM → f32 samples)
is untouched; only the inference layer swaps.

**Strategic context:** Phase 27.2 multi-platform release surfaced
Windows STT as the only feature still requiring an out-of-band C++
toolchain. Migrating to Candle removes that build chain entirely,
aligns the workspace with its existing pure-Rust + rustls posture
([`project_android_flutter_target.md`](../../.claude/projects/-home-familia-chat-proyecto/memory/project_android_flutter_target.md)),
and prepares STT for the Android Flutter embed without NDK + CMake
suffering at that point.

**Status:** ⬜ planned, **P3 — POST-CRITICAL**. Not blocking 27.2
release; Windows operators today install VS Build Tools per the
documented workaround. Pull when 27.2 ships GA + microapp UI work
pauses.

**Trigger:** any of —

- 27.2 GA shipped + matrix green across the 5 targets (so this
  phase's cross-compile validation can ride the same CI lane).
- A microapp ships voice-note transcription on a Windows or
  Android target where the C++ build chain is a hard block.
- Operator complaint about VS Build Tools on Windows install.

**Owner:** TBD — pull P0 tag once started.

## Mining

### OpenClaw — `research/`

| Path:line | Pattern |
|---|---|
| `research/CHANGELOG.md:1218,1551` | Telegram voice-note `.ogg` transcode → `whisper-cli` local fallback. Confirms the ogg-opus → whisper pipeline shape we already lifted. |
| `research/CHANGELOG.md:1754` | `openai-whisper-api` skill install recipe — cloud STT path lives alongside local. Validates the dual-backend design (Path B in the brainstorm). |
| `research/CHANGELOG.md:2990` | `api.runtime.stt.transcribeAudioFile(...)` plugin runtime API — STT abstracted behind a service interface so plugins can swap providers. Mirrors our `transcribe_file(path, cfg)` shape: provider-agnostic surface + pluggable backend. |
| `research/CHANGELOG.md:3063` | Discord audio attachment detection via `content_type` + preflight transcription gating. Audio detection logic stays at the dispatcher layer; the inference layer is platform-agnostic. |

### claude-code-leak

| Path:line | Pattern |
|---|---|
| `claude-code-leak/src/services/voiceStreamSTT.ts:1-15` | Anthropic `voice_stream` WebSocket STT client for push-to-talk: JSON control messages (`KeepAlive`, `CloseStream`) + binary audio frames; server replies with `TranscriptText` + `TranscriptEndpoint`. Confirms cloud-streaming STT is a real production pattern (out-of-scope v1, but a Phase 92.x follow-up could add `nexo-stt-cloud` mirroring this wire shape). |
| `claude-code-leak/src/hooks/useVoice.ts` | Voice mode behind a feature flag (`feature('VOICE_MODE')`) — same pattern as our `stt = ["dep:whisper-rs"]` Cargo feature. Validates the feature-gate split at SDK boundary (consumer opts in, default off). |

### What we don't take from references

OpenClaw's `whisper-cli` shell-out is the same C++ build chain we're
trying to escape — it imports from the system instead of vendoring,
but the upstream toolchain requirement persists. Claude Code's
`voice_stream` WebSocket client is OAuth-bound to Anthropic; no
generic equivalent. Both confirm the design space; neither maps
1:1 to a pure-Rust local backend.

## Sub-phases

### 91.1 — Candle Whisper API research + spec lock-in   ⬜

**Done when:**

- [ ] Read `candle-transformers::models::whisper::Whisper` source
  + the `proj-airi/candle-examples/whisper` reference.
- [ ] Document the input shape (mel spectrogram tensor — frames ×
  80 mel bins, log-magnitude scale).
- [ ] Document the model weights format (SafeTensors) + which
  HuggingFace Hub repo each Whisper size lives in
  (`openai/whisper-tiny`, `whisper-base`, `whisper-small`,
  `whisper-medium`, `whisper-large-v3`).
- [ ] Confirm Greedy sampling is exposed (whisper-rs parity);
  note absence if not (BeamSearch is a stretch goal).
- [ ] Decide the v1 model size pin (recommendation:
  `whisper-tiny` ~75 MB at int8 — same footprint as
  `ggml-tiny-q5_1.bin` we ship today).

**Effort:** 2 h.

### 91.2 — Cargo.toml deps + feature scaffold   ⬜

**Done when:**

- [ ] `crates/microapp-sdk/Cargo.toml` adds:
  ```toml
  [features]
  stt              = ["dep:opus-wave", "dep:ogg"]
  stt-whisper-cpp  = ["stt", "dep:whisper-rs"]                    # legacy
  stt-candle       = ["stt", "dep:candle-core", "dep:candle-nn",
                      "dep:candle-transformers", "dep:tokenizers",
                      "dep:hf-hub", "dep:rustfft"]                # new default-track

  [dependencies]
  candle-core         = { version = "0.8", optional = true, default-features = false }
  candle-nn           = { version = "0.8", optional = true, default-features = false }
  candle-transformers = { version = "0.8", optional = true, default-features = false }
  tokenizers          = { version = "0.20", optional = true, default-features = false, features = ["onig"] }
  hf-hub              = { version = "0.3", optional = true, default-features = false, features = ["tokio", "rustls-tls"] }
  rustfft             = { version = "6", optional = true }
  ```
- [ ] `cargo check --features stt-candle` clean.
- [ ] `cargo check --features stt-whisper-cpp` clean (legacy still
  builds).
- [ ] Compile-time `compile_error!` if both features enabled
  simultaneously (mutually exclusive backends).

**Effort:** 30 min.

**Risk:** `tokenizers` crate ~30 s primer build. `sccache` covers
re-builds.

### 91.3 — Mel spectrogram computation   ⬜

**Done when:**

- [ ] `crates/microapp-sdk/src/stt/mel.rs` implements
  `compute_log_mel_spectrogram(samples: &[f32], sample_rate: u32)
  -> Result<Vec<f32>>` returning a flattened
  `(n_frames × n_mel)` row-major buffer.
- [ ] Whisper-spec parameters baked in: 80 mel bins, 25 ms FFT
  window, 10 ms hop, log-mel scale, max 30 s clip (480 000
  samples at 16 kHz).
- [ ] `rustfft = "6"` for the STFT; mel filterbank generated at
  build-time (constant 80×201 matrix or `OnceLock` initialized).
- [ ] Unit tests asserting:
  - Output shape matches expected `(n_frames, 80)`.
  - 30 s ceiling clamps over-long inputs (no panic).
  - Empty input returns `EmptyAudio` error consistent with
    `whisper-rs` path.

**Effort:** 1 day.

**Risk:** Numerical drift vs whisper.cpp's mel implementation
could shift transcripts. Mitigation: golden-test with a 1 kHz
sine reference + spot-check log-mel buffer matches Candle's own
example output to within 1e-4 magnitude.

### 91.4 — Candle Whisper inference   ⬜

**Done when:**

- [ ] `crates/microapp-sdk/src/stt/transcribe_candle.rs` implements
  `transcribe_file(path, cfg) -> Result<String>` mirroring the
  existing `transcribe::transcribe_file` API surface byte-for-byte.
- [ ] Audio decode reuses
  `super::transcribe::decode_to_pcm_mono` (no duplication; the
  pure-Rust ogg-opus chain stays).
- [ ] Inference path:
  ```
  pcm s16 → f32 → log-mel → Tensor → model.forward() → token IDs
                                                        → tokenizer.decode → String
  ```
- [ ] Model loaded once, cached in a process-wide `OnceCell` keyed
  on `model_path` (mirror `whisper-rs` `WhisperContext` cache).
- [ ] Greedy sampling; language hint applied via the Whisper
  conditioning prompt (start-of-transcript token + `<|lang|>`
  token if `cfg.lang_hint = Some("es")`, etc.).

**Effort:** 2 days.

**Risk:** Greedy sampling vs `whisper-rs`'s tunable sampling —
documented as v1 limitation; BeamSearch parity is a 92.x
follow-up.

### 91.5 — Tokenizer + token decode   ⬜

**Done when:**

- [ ] `tokenizers::Tokenizer` loaded from `tokenizer.json`
  shipped alongside the SafeTensors model.
- [ ] Special tokens stripped (`<|startoftranscript|>`,
  `<|endoftext|>`, language tag, `<|notimestamps|>`).
- [ ] Whitespace normalisation applied to match the trimmed
  output format `whisper-rs` produces today (consumers grep for
  trimmed transcripts in the chat-turn pipeline).

**Effort:** half day.

### 91.6 — Feature flag + dispatch wiring   ⬜

**Done when:**

- [ ] `crates/microapp-sdk/src/stt/mod.rs` re-exports the right
  `transcribe_file` based on enabled feature:
  ```rust
  #[cfg(feature = "stt-whisper-cpp")]
  pub use transcribe::transcribe_file;

  #[cfg(all(feature = "stt-candle", not(feature = "stt-whisper-cpp")))]
  pub use transcribe_candle::transcribe_file;

  #[cfg(all(feature = "stt-candle", feature = "stt-whisper-cpp"))]
  compile_error!("stt-candle and stt-whisper-cpp are mutually exclusive — pick one backend");
  ```
- [ ] Public API surface (`SttError`, `TranscribeConfig`,
  `transcribe_file`, `InboundTransformHandler`) is **identical**
  between backends — zero downstream microapp / agent breaking
  changes.
- [ ] `cargo check --features stt-candle` builds with the new
  default-track backend; existing `stt-whisper-cpp` consumers
  continue to compile unchanged.

**Effort:** 30 min.

### 91.7 — Model format + HF Hub auto-fetch   ⬜

**Done when:**

- [ ] `TranscribeConfig` gains an optional `model_id:
  Option<String>` field (e.g. `Some("openai/whisper-tiny")`).
  When `model_path` is empty AND `model_id` is set, the SDK
  fetches via `hf-hub` to `~/.cache/huggingface/hub/...` on
  first use.
- [ ] When `model_path` is set (existing semantics), the SDK
  loads the SafeTensors directory directly — air-gapped envs
  keep working.
- [ ] Migration guide added to the SDK README: one-liner to
  download the SafeTensors equivalent of an existing
  `ggml-tiny-q5_1.bin` deployment.
- [ ] Defensive: no auto-fetch when both fields empty — fail
  fast with `SttError::ModelMissing` quoting both fields so the
  operator knows which knob to set.

**Effort:** half day.

**Risk:** Operators with existing GGML `.bin` deployments need
a migration path. Mitigation: `stt-whisper-cpp` legacy feature
stays available for one release cycle; CHANGELOG documents the
hf-hub one-liner.

### 91.8 — Parity tests vs whisper-rs baseline   ⬜

**Done when:**

- [ ] `crates/microapp-sdk/tests/stt_candle_parity.rs` runs both
  backends side-by-side on 3 fixture audio files (ES, EN, mixed)
  when both features are enabled together (`--features
  stt-whisper-cpp,stt-candle` — temporary parity-test build only,
  not a supported runtime config).
- [ ] Word Error Rate (WER) helper computed from a tiny
  Levenshtein distance impl (no extra dep). Fail when WER > 5 %
  on any fixture.
- [ ] Latency benchmark (`cargo bench` or a `#[ignore]` test)
  records p50 / p95 per backend × per fixture; results pasted
  into the phase recap. Acceptable: Candle CPU baseline within
  2× whisper.cpp throughput (heavier path is acceptable for the
  Windows / Android unblock).

**Effort:** half day.

**Risk:** Spurious failures from sampling determinism — Greedy
should be deterministic per backend, but cross-backend tiny
deltas might push WER over 5 %. Mitigation: tighten the
fixtures to clean speech with low background; mark as
`#[ignore]` if flake rate exceeds 2 % after first CI run, file
follow-up.

### 91.9 — Cross-compile matrix validation   ⬜

**Done when:**

- [ ] `cargo check --target <target> --features stt-candle`
  green on every target in this matrix:
  | Target | Constraint |
  |---|---|
  | `x86_64-unknown-linux-gnu` | host smoke |
  | `x86_64-unknown-linux-musl` | musl static |
  | `aarch64-unknown-linux-musl` | aarch64 musl |
  | `x86_64-pc-windows-msvc` | via `cargo-xwin` — no VS Build Tools required |
  | `aarch64-apple-darwin` | macOS Apple Silicon |
  | `aarch64-linux-android` | NDK target — Flutter precondition |
  | `wasm32-unknown-unknown` | browser target — bonus |
- [ ] `nexo-plugin-browser` style cross-platform regression: for
  each target, `cargo check` time stays within 1.5× the
  `stt-whisper-cpp` baseline.

**Effort:** half day on the dev box once the local toolchain
matches what 27.2 already validated.

**Risk:** A target reveals a transitive C dep we missed (e.g.
`tokenizers` `onig` regex feature drags `oniguruma`). Mitigation:
swap `onig` → `unicode` feature (pure Rust, slower regex path
but acceptable for STT).

### 91.10 — GPU acceleration features (opt-in)   ⬜

**Done when:**

- [ ] `crates/microapp-sdk/Cargo.toml` adds opt-in features:
  ```toml
  stt-candle-metal      = ["stt-candle", "candle-core/metal", "candle-nn/metal"]
  stt-candle-cuda       = ["stt-candle", "candle-core/cuda", "candle-nn/cuda"]
  stt-candle-accelerate = ["stt-candle", "candle-core/accelerate"]  # macOS BLAS
  ```
- [ ] Default `stt-candle` is CPU-only pure-Rust (works on every
  target). Operator opts into acceleration per build target.
- [ ] CHANGELOG documents the opt-in matrix + which target each
  feature unlocks.

**Effort:** half day.

**Risk:** GPU features may bring back C/C++ build deps for the
backend layer (CUDA toolkit, etc.). Acceptable — they're opt-in;
the default cross-platform path stays pure-Rust.

### 91.11 — Docs + CHANGELOG   ⬜

**Done when:**

- [ ] `docs/src/getting-started/platform-support.md` — drop
  the "Windows: Visual Studio Build Tools 2022 + CMake required
  for stt" caveat. Update the per-OS matrix to ✅ across the
  board for the `stt` row.
- [ ] `crates/microapp-sdk/README.md` — new § STT backend
  selection documenting the `stt-candle` (default-track) vs
  `stt-whisper-cpp` (legacy) feature split + GPU opt-ins.
- [ ] CHANGELOG `[Unreleased]` block flags the model format
  migration as a behaviour change (operators with GGML `.bin`
  files need to switch to SafeTensors directories or stay on
  `stt-whisper-cpp` for one release cycle).
- [ ] `mdbook build docs` clean.

**Effort:** half day.

### 91.12 — DEFER: deprecate `whisper-rs` feature   ⏭

**Trigger:** at least 2 release cycles after 91.11 lands and
microapp telemetry confirms zero `stt-whisper-cpp` consumers
remain.

**Done when:**

- [ ] `stt-whisper-cpp` feature removed from
  `crates/microapp-sdk/Cargo.toml`.
- [ ] `crates/microapp-sdk/src/stt/transcribe.rs` (the
  whisper-rs path) deleted.
- [ ] `whisper-rs` removed from workspace `Cargo.toml`.
- [ ] CHANGELOG `[Unreleased]` flags the removal.

**Effort:** 1 hour, scheduled.

## Done criteria — Phase 91 ships when

- [ ] 91.1 → 91.11 all green (12 sub-phases, 91.12 deferred to
  future cycle).
- [ ] Default-track is `stt-candle`; `stt-whisper-cpp` retained
  for one release cycle as the legacy fallback.
- [ ] Cross-compile matrix shows ✅ on Windows MSVC, Android NDK,
  macOS Apple Silicon, Linux musl × 2, WASM.
- [ ] Parity WER < 5 % vs whisper-rs baseline on 3 fixtures.
- [ ] `docs/src/getting-started/platform-support.md` no longer
  documents a VS Build Tools dependency.
- [ ] CHANGELOG entry under `[Unreleased]` with the model format
  migration note.

## Risks + global mitigations

| Risk | Mitigation |
|---|---|
| Candle CPU throughput < whisper.cpp by > 2× | Ship `whisper-tiny` int8 model as default (smaller + faster than `base`/`small`); document GPU opt-ins for high-volume deployments |
| Operator GGML `.bin` files orphaned by format swap | `stt-whisper-cpp` legacy feature retained for one release cycle; CHANGELOG hf-hub migration one-liner |
| HuggingFace Hub network dep introduces flake | `model_path` direct-load path stays untouched; hf-hub is opt-in (only triggers on `model_id` set + `model_path` empty); air-gapped envs documented |
| `tokenizers` crate `onig` feature drags `oniguruma` (C lib) | Swap to `unicode` feature (pure Rust regex) at parity-validation time if the C dep surfaces on Windows / Android |
| Greedy sampling parity gap vs `whisper-rs` BeamSearch | Documented v1 limitation in CHANGELOG; BeamSearch parity is a Phase 92.x follow-up |
| GPU feature flags re-introduce C/C++ build chain | Opt-in only; default stays pure-Rust; covered in 91.10 docs |

## Out of scope (Phase 92.x candidates)

- **Cloud STT backend** (Groq Whisper-large-v3, OpenAI Whisper API,
  Azure Speech) — Path B from the brainstorm. Useful for
  high-throughput SaaS where latency / cost favour cloud over
  local. Mining cite: `claude-code-leak/src/services/voiceStreamSTT.ts`
  shows the WebSocket streaming wire shape.
- **BeamSearch sampling parity** — `whisper-rs` exposes it; Candle
  v1 does not. File as Phase 92.x once Candle upstream lands.
- **Streaming / push-to-talk** — incremental token output during
  recording. Out of scope: current consumers feed complete audio
  files only.
- **Multi-language detection** — auto-detect when no `lang_hint`
  is set. Whisper supports it; ship as 91.x follow-up if a
  multi-lingual microapp surfaces.

## Phase tracker entries

When work begins:

1. Mark CLAUDE.md table row 91 status `🔄`.
2. Open sub-phase under `### Phase 91 — STT pure-Rust migration via
   Candle` in PHASES.md.
3. Tag P0 in PHASES-curated.md while in flight.
