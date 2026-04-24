# MSEdge TTS Rust Extension

Rust stdio extension for `proyecto` that exposes free Microsoft Edge Read Aloud
text-to-speech as agent tools.

## What it adds

- `ext_msedge-tts_status`
- `ext_msedge-tts_list_voices`
- `ext_msedge-tts_synthesize`

The extension uses the Rust crate [`msedge_tts`](https://docs.rs/msedge-tts),
which wraps the same Edge Read Aloud service family that OpenClaw used through
Node/Python tooling.

## Build

This extension is a standalone Rust crate. Build it inside this directory:

```bash
cd extensions/msedge-tts
cargo build --release
```

The manifest points at `./target/release/msedge-tts`, so no manual copy step is
needed after a successful build.

## Linux prerequisites

The upstream crate uses native TLS. On Debian/Ubuntu hosts you may need:

```bash
sudo apt-get update
sudo apt-get install libssl-dev pkg-config
```

## Notes

- Output files default to the system temp dir under `agent-msedge-tts/`.
- You can override the output path by passing `output_path` to `synthesize`.
- The extension is discovered automatically because it lives under
  `proyecto/extensions/`.
