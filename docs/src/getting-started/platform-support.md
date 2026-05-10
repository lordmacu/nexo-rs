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
| `stt` | Inbound voice-note transcription via vendored whisper.cpp | ✅ | ✅ | ⚠️ needs VS Build Tools 2022 + CMake | ⚠️ needs `cmake` + `clang` packages |
| `voice` | Outbound voice replies via Microsoft Edge TTS + pure-Rust opus encoder | ✅ | ✅ | ✅ | ✅ |
| `wizard` | First-run LLM key probe via `reqwest` (rustls-tls only) | ✅ | ✅ | ✅ | ✅ |
| `enrichment` | Disposable-domain classifier + tenant-keyed cache | ✅ | ✅ | ✅ | ✅ |
| `tracking` | HMAC-signed message + link tokens | ✅ | ✅ | ✅ | ✅ |
| `email-template` | Block-based email composer + render + asset store | ✅ | ✅ | ✅ | ✅ |

### Why STT is the only feature with platform caveats

`whisper-rs` builds whisper.cpp from a vendored C++ source on each
target. That C++ build needs a C++ compiler + CMake on the build
machine:

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
  Termux, consider routing transcription to an upstream daemon
  rather than running whisper-cpp on-device.

Once the C++ build succeeds the first time, subsequent rebuilds
are cached — operators usually pay this cost once during initial
setup and never again.

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
