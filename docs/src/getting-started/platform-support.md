# Platform support

Honest matrix of what runs on what, plus the prerequisites each
operating system needs for the optional voice / browser / WhatsApp
features.

## Daemon binary (`nexo`)

The core daemon — the agent loop, NATS bus, plugin supervisor,
admin API, MCP client/server, memory layer, taskflow runtime —
ships as a single static binary. It compiles against pure-Rust TLS
(`rustls`) and a bundled SQLite C source, so no system OpenSSL or
libsqlite is required at runtime.

| Platform | Arch | Daemon | How to install |
|---|---|---|---|
| Linux (any glibc / musl distro) | x86_64 | ✅ | `curl …/nexo-rs-installer.sh \| sh` |
| Linux (any glibc / musl distro) | aarch64 | ✅ | `curl …/nexo-rs-installer.sh \| sh` |
| macOS | x86_64 (Intel) | ✅ | `curl …/nexo-rs-installer.sh \| sh` |
| macOS | aarch64 (Apple Silicon) | ✅ | `curl …/nexo-rs-installer.sh \| sh` |
| Windows | x86_64 | ✅ | Download `.zip` from [Releases](https://github.com/lordmacu/nexo-rs/releases) |
| Windows (WSL) | x86_64 | ✅ | Same Linux installer works inside WSL |
| **Docker** (any host) | amd64 + arm64 | ✅ | `docker pull ghcr.io/lordmacu/nexo-rs:latest` |
| Android (Termux) | aarch64 | ⚠️ build locally | `pkg install rust && cargo install --git ...` (CI .deb temporarily disabled — see [FOLLOWUPS](https://github.com/lordmacu/nexo-rs/blob/main/FOLLOWUPS.md)) |

The shell installer auto-detects platform + arch and downloads the
right tarball. Apple Silicon laptops, Intel macs, and Linux servers
share the same one-liner.

Native Windows (cmd.exe / PowerShell, no WSL) is not yet covered by
the curl installer because the script is bash-only — install via
the GH Releases zip download for now. PowerShell + MSI installers
are tracked under [`FOLLOWUPS.md`](https://github.com/lordmacu/nexo-rs/blob/main/FOLLOWUPS.md).

## Optional features — what compiles per OS

The daemon's default feature set works on every platform above. A
microapp built on top of [`nexo-microapp-sdk`](../microapps/getting-started.md)
can opt into extra features that pull additional system
dependencies; this is what changes per OS.

| Feature | What it enables | Linux | macOS | Windows | Termux |
|---|---|---|---|---|---|
| `stt-candle` | **Default-track** — inbound voice-note transcription via HuggingFace Candle (pure Rust) | ✅ | ✅ | ✅ | ✅ |
| `stt` | **Legacy** — same surface via whisper.cpp C++ binding (`whisper-rs`) | ✅ | ✅ | ⚠️ needs VS Build Tools 2022 + CMake | ⚠️ needs `cmake` + `clang` packages |
| `stt-cloud` | Cloud STT (**native** variant) — `SttProvider` trait + OpenAI Whisper-1 + Groq Whisper-large-v3 (REST). `CompositeProvider` fallback chain. Pulls reqwest with `rustls-tls` | ✅ | ✅ | ✅ | ✅ |
| `stt-cloud-wasm` | Cloud STT (**wasm32** variant) — same trait + REST providers as `stt-cloud`, but reqwest pulled without `rustls-tls` (browser fetch API handles TLS). Use this for `wasm32-unknown-unknown` microapps | — (use `stt-cloud`) | — (use `stt-cloud`) | — (use `stt-cloud`) | — (use `stt-cloud`) |
| `stt-cloud-anthropic` | Adds Anthropic `voice_stream` WebSocket leg on top of `stt-cloud` (Claude.ai OAuth-gated; conversation engine + Deepgram Nova 3) | ✅ | ✅ | ✅ | ✅ |
| `stt-cloud-local-candle` | Bridge — `LocalCandleProvider` so the Candle backend joins a `CompositeProvider` chain as the offline fallback leg + `*_then_candle` convenience constructors | ✅ | ✅ | ✅ | ✅ |
| `voice` | Outbound voice replies via Microsoft Edge TTS + pure-Rust opus encoder | ✅ | ✅ | ✅ | ✅ |
| `wizard` | First-run LLM key probe via `reqwest` (rustls-tls only) | ✅ | ✅ | ✅ | ✅ |
| `enrichment` | Disposable-domain classifier + tenant-keyed cache | ✅ | ✅ | ✅ | ✅ |
| `tracking` | HMAC-signed message + link tokens | ✅ | ✅ | ✅ | ✅ |
| `email-template` | Block-based email composer + render + asset store | ✅ | ✅ | ✅ | ✅ |

### STT backend choice (`stt-candle` vs `stt`)

Phase 91 introduced the pure-Rust **Candle** backend
(`stt-candle`) as the default track. The legacy whisper-rs path
(`stt`) is retained for one stability window — Phase 91.12 drops
it once telemetry confirms the migration.

Pick the right one:

- **`stt-candle` (recommended for every target)** — HuggingFace
  Candle ML framework, no C++ build chain. Works out of the box
  on Linux, macOS, Windows, Termux / Android NDK. Model format
  is HuggingFace SafeTensors (`openai/whisper-tiny` and friends);
  the SDK auto-fetches the weights + tokenizer + config from HF
  Hub on first call when `TranscribeConfig::model_id` is set, or
  loads from a local directory pinned via
  `TranscribeConfig::model_path` (air-gapped deployments).
- **`stt` (legacy)** — `whisper-rs` binding to whisper.cpp.
  Slightly faster on CPU, but the C++ build chain requires a
  per-target toolchain and breaks Android NDK / WASM
  cross-compile entirely. Keep it only if you've already shipped
  GGML `.bin` models you can't easily migrate yet.

Both backends share the audio-decode pipeline (ogg-opus → s16
PCM → f32) and the public `TranscribeConfig` / `transcribe_file`
signature, so swapping is a Cargo feature change with no code
edits at consumer sites.

#### GPU acceleration (opt-in, `stt-candle-*` sub-features)

The default `stt-candle` build is CPU-only pure-Rust so it
cross-compiles to every target the workspace ships. Hardware
acceleration is opt-in per build target:

| Cargo feature | Backend | Platform |
|---|---|---|
| `stt-candle-metal` | Apple Metal | macOS / iOS |
| `stt-candle-cuda` | NVIDIA CUDA | Linux + Windows |
| `stt-candle-accelerate` | Apple Accelerate (BLAS) | macOS |

Mix at most one per build. The audio decode + tokenizer
pipeline stays identical — only the Tensor backend swaps.

#### Migration from a `stt` (whisper-rs) deployment

If you already ship a GGML `.bin` file and want to switch to
`stt-candle`:

```bash
# 1. Download the equivalent SafeTensors model from HF Hub.
huggingface-cli download openai/whisper-tiny \
  --local-dir ./data/whisper-tiny

# 2. Point your microapp config at the new directory.
#    Either:
#      TranscribeConfig.model_path = "./data/whisper-tiny"
#    or, to auto-fetch on first call (HF Hub cache):
#      TranscribeConfig.model_id   = Some("openai/whisper-tiny")

# 3. Flip the Cargo feature.
#    Before: nexo-microapp-sdk = { features = ["stt"] }
#    After:  nexo-microapp-sdk = { features = ["stt-candle"] }
```

The whisper-rs path keeps working unchanged during the
transition. Do not enable both features at once in a production
build — `stt-candle` wins the public re-export when both are on,
so the legacy path becomes effectively unreachable through the
default API.

### `stt` (legacy) — when you still need the C++ toolchain

If you stay on the `stt` feature, the original platform caveats
still apply:

- **Linux**: `apt install clang cmake` (or your distro's
  equivalent). Most dev machines already have it.
- **macOS**: Xcode Command Line Tools — `xcode-select --install`.
  Provides clang + cmake.
- **Windows**: Visual Studio Build Tools 2022 (the "Desktop
  development with C++" workload, or just MSVC + CMake from the
  individual components page) — no full Visual Studio IDE
  required. Plus `cmake` from <https://cmake.org/download/>.
  After install, open a "Developer Command Prompt for VS 2022"
  the first time so `cl.exe` is on PATH.
- **Termux**: `pkg install cmake clang` from inside the Termux
  shell. Note that whisper.cpp performance on Android / Termux is
  noticeably lower than desktop CPUs; for production STT in
  Termux, consider `stt-candle` (which compiles trivially in
  Termux) or routing transcription to an upstream daemon.

Once the C++ build succeeds the first time, subsequent rebuilds
are cached — operators usually pay this cost once during initial
setup and never again.

### Cloud STT (`stt-cloud*`) — REST + WebSocket backends

For deployments where on-device inference isn't a good fit
(SaaS hot path, WASM browser microapps, metered cellular
devices) the SDK ships a cloud STT path. Three providers, one
fallback chain primitive, three one-line convenience
constructors:

| Cargo feature | What it adds |
|---|---|
| `stt-cloud` | `SttProvider` trait + `CompositeProvider` fallback chain + `OpenAiProvider` (Whisper-1 REST) + `GroqProvider` (Whisper-large-v3 REST) + `transcribe_file_with_chain` helper |
| `stt-cloud-anthropic` | Adds `AnthropicVoiceStream` — full WebSocket client for `wss://api.anthropic.com/api/ws/speech_to_text/voice_stream` (OAuth-gated; the same conversation engine + Deepgram Nova 3 stack Claude Code itself uses for voice input) |
| `stt-cloud-local-candle` | Adds `LocalCandleProvider` so the local Candle backend joins fallback chains as the offline-backup leg, plus `anthropic_then_candle` / `openai_then_candle` / `groq_then_candle` convenience constructors |

#### Cloud-first with local fallback — one line

When `stt-cloud-local-candle` is on, compose any cloud primary
with a local Candle backup in one call:

```rust
use std::sync::Arc;
use nexo_microapp_sdk::stt::{TranscribeConfig, cloud};

let candle_cfg = Arc::new(TranscribeConfig {
    model_id: Some("openai/whisper-tiny".into()),
    lang_hint: Some("es".into()),
    ..Default::default()
});

// Anthropic voice_stream → Candle fallback:
let chain = cloud::anthropic_then_candle(oauth_token, candle_cfg.clone());

// Or OpenAI / Groq REST → Candle fallback:
// let chain = cloud::openai_then_candle(api_key, candle_cfg.clone());
// let chain = cloud::groq_then_candle(api_key, candle_cfg);

let transcript = cloud::transcribe_file_with_chain(
    std::path::Path::new("/tmp/voice-note.ogg"),
    &chain,
    Some("es"),
).await?;
```

The fallback fires on transport errors (HTTP 5xx, network
unreachable, WebSocket disconnect). Hard audio errors
(`EmptyAudio`, `UnsupportedFormat`, `Decode`) short-circuit —
the next leg would hit the same problem on the same bytes.

#### Anthropic `voice_stream` — Claude.ai OAuth required

`AnthropicVoiceStream` connects to the same endpoint Claude
Code uses internally:
`wss://api.anthropic.com/api/ws/speech_to_text/voice_stream`.
Requires a Claude.ai subscriber OAuth token (not a regular
Anthropic API key — different auth surface).

Wire format (linear16 PCM @ 16 kHz mono, JSON control frames,
binary audio frames). The SDK collapses the streaming
endpoint to a one-shot call: open WS, send buffer, send
`{"type":"CloseStream"}`, drain until the 4-trigger finalize
state machine resolves (PostCloseStreamEndpoint @ ~300 ms /
NoDataTimeout @ 1.5 s / SafetyTimeout @ 5 s / WsClose). Live
push-to-talk streaming is a deferred follow-up — see
[`FOLLOWUPS.md`](https://github.com/lordmacu/nexo-rs/blob/main/FOLLOWUPS.md)
`91.x.wasm.phase-4b.streaming`.

#### WASM (`wasm32-unknown-unknown`) — REST cloud works, voice_stream deferred

The pure-Rust local backends (`stt-candle` Candle + `stt`
whisper-rs) don't compile for `wasm32-unknown-unknown` today
— the inference stack depends on crates that need kernel
networking (`mio`) or aren't WASM-clean (`opus-wave`,
`tokenizers` with `onig`, Candle's GEMM kernels).

**REST cloud STT works on wasm32.** Enable `stt-cloud-wasm`
(the wasm-clean sibling of `stt-cloud` — reqwest pulled
without `rustls-tls`, browser fetch API handles TLS). OpenAI
Whisper-1 + Groq Whisper-large-v3 + the `CompositeProvider`
fallback chain are fully supported in browser microapps.
`SttProvider` trait drops `Send + Sync` bounds + uses
`async_trait(?Send)` on wasm32 because the wasm-bindgen
fetch backend returns futures holding js-sys types that
aren't Send (single-threaded execution model — the bounds
were a native-only thing anyway).

Cross-target microapps select the right feature per-target
in their own Cargo.toml:

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
nexo-microapp-sdk = { workspace = true, features = ["stt-cloud-wasm"] }

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
nexo-microapp-sdk = { workspace = true, features = ["stt-cloud", "stt-cloud-anthropic", "stt-cloud-local-candle"] }
```

`stt-cloud-anthropic` (voice_stream WebSocket) is still
native-only — tokio-tungstenite drags TCP types absent on
wasm32. Browser microapps
wanting voice_stream would need a `web-sys::WebSocket`-based
swap-in (filed as `91.x.wasm.phase-4c`).

### Voice (TTS) is portable everywhere

The `voice` feature uses pure-Rust crates (`opus-wave`, `symphonia`,
`ogg`) plus a websocket call to Microsoft Edge's TTS endpoint. No
C/C++ build, no system audio framework — works the same on Linux,
macOS, Windows, and Termux.

## Channels — what Rust + the host OS support

Channels (WhatsApp / Telegram / browser / email) ship as
[standalone subprocess plugins](../plugins/whatsapp.md). Each plugin is
its own Rust binary and inherits the same OS support matrix as the
daemon:

| Channel | Linux | macOS | Windows | Termux | Notes |
|---|---|---|---|---|---|
| WhatsApp | ✅ | ✅ | ✅ | ✅ | Uses Signal Protocol via the `wa-agent` upstream crate; pure Rust, all-platform |
| Telegram | ✅ | ✅ | ✅ | ✅ | Bot API long-poll; pure Rust |
| Browser | ✅ | ✅ | ✅ | ⚠️ Chrome must be in `PATH`; Termux needs `pkg install chromium` |
| Email | ✅ | ✅ | ✅ | ✅ | IMAP poll + lettre SMTP; rustls-tls everywhere |

### Browser channel caveat — Chromium availability

The browser plugin spawns a Chromium instance via Chrome DevTools
Protocol. The plugin doesn't bundle Chromium; it shells out to
whatever Chrome / Chromium / Edge is in `PATH`:

- **macOS**: `brew install --cask google-chrome` or use an existing
  Chrome install (`/Applications/Google Chrome.app/...` path is
  auto-detected).
- **Windows**: install Chrome from <https://www.google.com/chrome/>
  and let the plugin auto-detect at default install path.
- **Linux servers (headless)**: install via your distro
  (`apt install chromium`) — the plugin runs Chromium headless by
  default.
- **Termux**: `pkg install chromium` — note that Termux's chromium
  package is significantly older than upstream and some CDP
  features may misbehave.

## What's intentionally NOT in scope today

| Wanted by users? | Why deferred |
|---|---|
| Homebrew formula (`brew install nexo-rs`) | Requires the macOS targets to land first + a release of the binary on those targets. The [tap repo](https://github.com/lordmacu/homebrew-nexo-rs) is created; the formula auto-publish will turn on as part of the [Phase 27.2 follow-up](https://github.com/lordmacu/nexo-rs/blob/main/FOLLOWUPS.md). |
| `npm install -g @nexo-rs/cli` | The [`@nexo-rs/cli` npm scope](https://www.npmjs.com/package/@nexo-rs/cli) is reserved with a placeholder; the real CLI shim ships when `cargo dist` re-enables npm in `dist-workspace.toml` `installers`. |
| Native Windows MSI / PowerShell installer | Same dist-workspace dependency. The `.zip` from GH Releases works in the meantime. |
| Apple Silicon / Intel Mac via Homebrew | Tap exists, formula not auto-pushed yet. Curl installer covers both Intel + Apple Silicon directly. |

## Reporting platform-specific issues

If `nexo --version` runs but a particular feature breaks on your
OS, file an issue with the version line + the relevant build
channel (printed by `nexo version` in verbose mode):

```bash
nexo version | head -5
# nexo 0.1.6
# git_sha:  …
# channel:  tarball-x86_64-apple-darwin
# target:   x86_64-apple-darwin
```

Tag the issue with `os:macos`, `os:windows`, `os:termux`, etc., so
we can track per-platform regressions across releases.
