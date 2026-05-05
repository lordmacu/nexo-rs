# Voice mode (inbound STT + outbound TTS)

End-to-end voice support across the framework + the agent-creator
microapp. Uses the auto-discovered `*_reply_transform` and
`*_inbound_transform` tool conventions exposed by `nexo-core` so any
microapp can plug in its own engine without touching the framework.

## What ships

| Direction | Engine | Where the model lives |
|---|---|---|
| Outbound (text → audio) | Microsoft Edge Read-Aloud (online, no key) | crate, no model file |
| Inbound (audio → text) | whisper.cpp via `whisper-rs` | local file you pre-download |

The chat UI's mic toggle (next to the attachment button) flips the
outbound side per-conversation. The inbound side runs unconditionally
on every incoming voice note — every voice note becomes text the LLM
can read.

## Build prerequisites

The microapp links whisper.cpp from source. On a fresh Ubuntu/Debian
host install:

```bash
sudo apt install cmake build-essential ffmpeg
```

`ffmpeg` is required at runtime for the audio pipeline (transcode
mp3 ↔ ogg/opus on outbound, decode any container → 16 kHz PCM on
inbound). `cmake` + `build-essential` are needed only at compile
time.

## Pre-downloading the whisper model

We default to `tiny-q5_1` (~31 MB, decent Spanish accuracy, runs
in real time on CPU). Download it once:

```bash
mkdir -p .dev-state/data/whisper
curl -L -o .dev-state/data/whisper/ggml-tiny-q5_1.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny-q5_1.bin
```

The microapp resolves the file via:

1. `NEXO_WHISPER_MODEL_PATH` (absolute path, takes precedence).
2. `<NEXO_EXTENSION_STATE_ROOT>/data/whisper/<NEXO_WHISPER_MODEL_FILE>`.
3. `<NEXO_EXTENSION_STATE_ROOT>/data/whisper/ggml-tiny-q5_1.bin`.

To upgrade to a bigger model:

```bash
# Better quality (~85% on Spanish), 2× CPU latency.
curl -L -o .dev-state/data/whisper/ggml-base.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin
export NEXO_WHISPER_MODEL_FILE=ggml-base.bin

# Production-grade (~92%), needs a beefy CPU or GPU.
curl -L -o .dev-state/data/whisper/ggml-small.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin
export NEXO_WHISPER_MODEL_FILE=ggml-small.bin
```

The model loads lazily on the first inbound voice note — boot stays
cheap when nobody sends audio.

## How it wires into the agent loop

```
Inbound:  WhatsApp PTT
       → plugin downloads OGG/Opus to <state_root>/data/whatsapp/media/
       → InboundMessage.media populated (kind=audio_voice, path, mime)
       → nexo-core auto-discovers `audio_stt_inbound_transform`
       → microapp transcodes via ffmpeg → 16 kHz mono PCM
       → whisper-rs returns the transcript
       → LLM sees the transcript as `msg.text`

Outbound: LLM reply text
       → nexo-core auto-discovers `voice_mode_reply_transform`
       → microapp checks per-conversation `voice_mode` flag
       → if ON: Edge TTS synthesizes mp3 → ffmpeg transcodes to ogg/opus
              → OutboundReplyKind::VoiceNote
              → whatsapp plugin sends as PTT via send_voice_note
       → if OFF: passthrough, plugin sends as text
```

## Operator UX

- **In the chat:** click the mic icon next to the paperclip on the
  conversation you want to flip to voice. State is per-conversation
  and persisted in `<state_root>/firehose.db.voice_mode` table.
- **Default voice:** `es-MX-DaliaNeural`. List others with
  `msedge-tts list_voices` (we expose an extension already in
  `proyecto/extensions/msedge-tts/`).

## Failure behaviour

- `ffmpeg` missing → outbound falls back to text reply.
- whisper model missing → inbound falls through with empty text +
  warn log; LLM gets the original (probably empty) `msg.text`.
- TTS WebSocket flaky → first failure also falls back to text.

Every fallback fires a `tracing::warn!` so operators see the reason
in `daemon.log`.

## Tradeoffs we picked

- **Inbound STT runs synchronously** in the bridge handler — adds
  ~1-3 s to inbound audio messages on `tiny-q5_0` CPU. Image / video
  / document downloads stay async (don't gate the agent).
- **whisper-rs default-features = false** — skips OpenBLAS to keep
  the build portable; if you have CUDA / OpenBLAS / Metal available
  flip the corresponding feature for ~3-10× speedup.
