# Phase 91.9 — Candle STT backend cross-compile matrix

Local validation results for `cargo check`/`build` against
`nexo-microapp-sdk --features stt-candle --no-default-features
--lib` on every target Phase 91.9 calls out.

Validation host: Linux x86_64-unknown-linux-gnu, `2026-05-10`.

| Target | Build cmd | Status | Notes |
|---|---|---|---|
| `x86_64-unknown-linux-gnu` | `cargo check` | ✅ 1.2 s | host smoke |
| `x86_64-unknown-linux-musl` | `cargo zigbuild --target …-musl` | ✅ 50 s | CFLAGS macro hack (`-Du_int8_t=uint8_t` etc. per Phase 27.2) |
| `aarch64-unknown-linux-musl` | `cargo zigbuild --target …` | ✅ 48 s | needs `RUSTFLAGS="-C target-feature=+fp16,+fhm"` |
| `x86_64-pc-windows-msvc` | `cargo xwin build --target …-msvc` | ✅ 1 m 12 s | cargo-xwin 0.22.0 + LLVM r27 + lld-link |
| `aarch64-linux-android` | `cargo ndk --target arm64-v8a --platform 21 build` | ✅ 1 m 51 s | NDK r27c + `RUSTFLAGS="-C target-feature=+fp16,+fhm"` |
| `aarch64-apple-darwin` | `cargo check --target …-darwin` | ⏳ CI-only | no Mac on the dev box; validated via the macos-14 runner in `release.yml` |
| `wasm32-unknown-unknown` | `cargo check --target …` | ❌ | blocked by `mio` (hf-hub → tokio net). Phase 91.x follow-up. |

## Required cargo invocations

### Linux x86_64-musl

```bash
export CFLAGS_x86_64_unknown_linux_musl="-Du_int8_t=uint8_t \
  -Du_int16_t=uint16_t -Du_int64_t=uint64_t"
cargo zigbuild -p nexo-microapp-sdk \
  --features stt-candle --no-default-features \
  --target x86_64-unknown-linux-musl --lib
```

### Linux aarch64-musl

```bash
export CFLAGS_aarch64_unknown_linux_musl="-Du_int8_t=uint8_t \
  -Du_int16_t=uint16_t -Du_int64_t=uint64_t"
export RUSTFLAGS="-C target-feature=+fp16,+fhm"
cargo zigbuild -p nexo-microapp-sdk \
  --features stt-candle --no-default-features \
  --target aarch64-unknown-linux-musl --lib
```

### Windows MSVC

```bash
cargo xwin build -p nexo-microapp-sdk \
  --features stt-candle --no-default-features \
  --target x86_64-pc-windows-msvc --lib
```

### Android NDK (Termux, Flutter FFI)

```bash
export ANDROID_NDK_HOME="$HOME/.local/share/android-ndk-r27c"
export RUSTFLAGS="-C target-feature=+fp16,+fhm"
cargo ndk --target arm64-v8a --platform 21 build \
  -p nexo-microapp-sdk --features stt-candle --no-default-features --lib
```

## Why `+fp16,+fhm`

Candle's matrix-multiply backend (`gemm-f16` crate) emits ARMv8.2-A
FP16 + Floating-point Half-precision Multiply (FHM) intrinsics
unconditionally on aarch64. The default LLVM target arch for
`aarch64-unknown-linux-musl` and `aarch64-linux-android` is
ARMv8.0-A (broad device compatibility), so the assembler rejects
the instructions with:

```
error: instruction requires: fullfp16
```

Setting `RUSTFLAGS="-C target-feature=+fp16,+fhm"` raises the
effective baseline to ARMv8.2+, which every Android device from
roughly 2018 onward supports. WhatsApp / Telegram clients run on
hardware several years newer than that, so the baseline shift is
safe for the targeted operator base.

If future hardware support for older ARMv8.0-only devices becomes
a requirement, the fix is upstream in `gemm`/`candle-core` (either
runtime CPU feature detection or compile-time scalar fallback) —
not in `nexo-microapp-sdk`.

## WASM blocker (deferred Phase 91.x follow-up)

The `wasm32-unknown-unknown` build fails inside the `mio` crate
(`Tokio`'s underlying I/O reactor):

```
error: This wasm target is unsupported by mio. If using Tokio,
       disable the net feature.
```

Root: `hf-hub 0.4` requires `tokio` with the `net` feature for its
async HTTP client. WASM has no kernel networking primitives;
fetching from HuggingFace Hub there needs a `wasm-bindgen`-flavoured
HTTP client (e.g. `gloo-net` / `reqwest`'s wasm path).

**Fix paths** (all out of scope for Phase 91):

1. Gate `hf-hub` behind a `stt-candle-hub` sub-feature so WASM
   consumers can build with `stt-candle` minus `hf-hub`. They ship
   the SafeTensors bundled in the WASM artifact and call into the
   Candle backend with `model_path` set explicitly.
2. Upstream PR to `hf-hub` swapping `reqwest` to a WASM-aware
   transport.
3. Drop WASM from the Phase 91 cross-compile matrix entirely
   (current de facto state — it stays a backlog target).

File as `91.x.wasm` if WASM-side STT becomes a real demand.

## Sources / inputs

- Local dev box: Linux 6.8.0-111-generic, Rust 1.95 stable, zig
  0.13.0 (pinned via `~/.local/share/zig-0.13/`), NDK r27c at
  `~/.local/share/android-ndk-r27c/`.
- cargo-zigbuild 0.22.3, cargo-xwin 0.22.0, cargo-ndk 4.1.2.
