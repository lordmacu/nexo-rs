# Installation

Pick the channel that matches your environment. Every channel
produces the same `nexo` binary; the differences are in how it
gets onto your machine and which dependencies come bundled.

## Channel matrix

| Channel | When to pick it | Time to first run | Bundled runtime tools |
|---|---|---|---|
| **[Docker (GHCR)](../ops/docker.md)** | Production, CI, "just works" | ~30 s | Chrome, Chromium, cloudflared, ffmpeg, tesseract, yt-dlp |
| **[Nix flake](./install-nix.md)** | NixOS, reproducible dev shell | ~3-5 min cold | None (system-level) |
| **[Native (no Docker)](./install-native.md)** | Bare-metal Linux / macOS, full control | ~10-15 min | None (apt / brew / pacman) |
| **[Termux](./install-termux.md)** | Phone-hosted personal agent | ~15-20 min | None (`pkg install`) |
| **From source** | Contributors | ~5 min after toolchain | None |

## Quickest path — Docker

```bash
docker pull ghcr.io/lordmacu/nexo-rs:latest
docker run --rm \
  -v $(pwd)/config:/app/config:ro \
  -v $(pwd)/data:/app/data \
  -p 8080:8080 -p 9090:9090 \
  ghcr.io/lordmacu/nexo-rs:latest --help
```

The image is multi-arch (`linux/amd64` + `linux/arm64`), built
fresh on every push to `main` and every `v*` tag, with SBOM and
SLSA provenance attestations. Full guide: [Docker](../ops/docker.md).

## Build from source

For contributors and operators who want to track `main` directly:

```bash
git clone https://github.com/lordmacu/nexo-rs
cd nexo-rs
cargo build --release --bin nexo
./target/release/nexo --help
```

The workspace compiles 22 crates and produces the `nexo` binary
plus a few smoke-test bins (`browser-test`,
`integration-browser-check`, `llm_smoke`). Toolchain is pinned to
Rust 1.80 (MSRV) via `rust-toolchain.toml` — no manual channel
selection needed.

### Prerequisites

- **Rust** 1.80+ (`rustup` recommended)
- **NATS** running locally or reachable over the network — for
  development:
  ```bash
  docker run -p 4222:4222 nats:2.10-alpine
  ```
  Production setup: see [broker.yaml](../config/broker.md).
- **Git** (the memory subsystem uses per-agent workspace-git)
- **Chrome / Chromium** (only if you plan to use the browser plugin)

### Verification

```bash
./target/release/nexo --version
cargo test --workspace --lib
```

`nexo --version` prints the build provenance line (commit + build
timestamp) so a bug report carries enough context to reproduce.

## Bootstrap script

For native or Termux installs, `./scripts/bootstrap.sh` automates
the whole process — installs the system deps, downloads NATS if
not present, scaffolds `config/`, and runs the setup wizard.

```bash
./scripts/bootstrap.sh           # interactive
./scripts/bootstrap.sh --yes     # accept all defaults
```

The script auto-detects Termux (`$PREFIX` set) and switches to
`pkg install` + `broker.type: local` so you don't need root or
NATS on a phone.

## Next steps

- [Quick start](./quickstart.md) — first agent running in five minutes
- [Setup wizard](./setup-wizard.md) — pair channels and wire secrets
- [Docker](../ops/docker.md) — compose stack, secrets, GHCR pulls
- [Nix flake](./install-nix.md) — `nix run`, dev shell
- [Native install](./install-native.md) — detailed no-Docker setup
- [Termux install](./install-termux.md) — phone-hosted personal agent
