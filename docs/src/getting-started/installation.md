# Installation

Pick the channel that matches your environment. Every channel
produces the same `nexo` binary; the differences are in how it
gets onto your machine and which dependencies come bundled.

## Channel matrix

| Channel | When to pick it | Time to first run | Needs Rust? |
|---|---|---|---|
| **Pre-built binary — Linux/macOS (`install.sh`)** | Just want `nexo` on PATH, fast | ~10 s | No |
| **Pre-built binary — Windows (`install.ps1`)** | Native Windows, PowerShell | ~10 s | No |
| **crates.io (`cargo install nexo-rs`)** | You already have a Rust toolchain | ~3-5 min build | Yes |
| **[Debian / Ubuntu (`.deb`)](./install-deb.md)** | systemd host, `apt` integration | ~10 s | No |
| **[Fedora / RHEL / Rocky (`.rpm`)](./install-rpm.md)** | systemd host, `dnf` integration | ~10 s | No |
| **[Termux (`.deb`, aarch64)](./install-termux.md)** | Phone-hosted personal agent | ~10 s | No |
| **[Docker (GHCR)](../ops/docker.md)** | Production, CI, "just works" + bundled Chrome/ffmpeg/… | ~30 s | No |
| **[Nix flake](./install-nix.md)** | NixOS, reproducible dev shell | ~3-5 min cold | (Nix) |
| **[Native (no Docker), from source](./install-native.md)** | Track `main`, full control | ~10-15 min | Yes |

## Quickest path — pre-built binary

**Linux / macOS** (also Windows from Git Bash):

```bash
curl -fsSL https://lordmacu.github.io/nexo-rs/install.sh | bash
```

**Windows** (PowerShell):

```powershell
irm https://lordmacu.github.io/nexo-rs/install.ps1 | iex
```

Detects your OS + arch (Linux x86_64 / aarch64 static-musl,
macOS Intel / Apple Silicon, Windows x86_64-MSVC), downloads the
matching release artifact from [GitHub Releases](https://github.com/lordmacu/nexo-rs/releases/latest),
verifies its sha256, drops `nexo` (`nexo.exe` on Windows) onto your
PATH, then installs the bundled channel plugins + a persona. Falls
back to `cargo install nexo-rs` → `cargo install --git` if there's
no pre-built binary for your platform. Every release artifact is
cosign-signed.

Then:

```bash
nexo                 # boots the daemon — zero config required
```

Windows users: download the `.zip` from
[Releases](https://github.com/lordmacu/nexo-rs/releases/latest), or
run the installer under WSL (it then sees Linux).

## From crates.io

If you already have a Rust toolchain:

```bash
cargo install nexo-rs
```

Builds + installs the `nexo` binary into `$CARGO_HOME/bin`. The
whole `nexo-*` workspace ships to crates.io, so this resolves
cleanly without a git checkout.

## From source

For contributors and operators who want to track `main` directly:

```bash
git clone https://github.com/lordmacu/nexo-rs
cd nexo-rs
cargo build --release --bin nexo
./target/release/nexo --help
```

The workspace compiles ~45 crates and produces the `nexo` binary
plus two example smoke-test bins (`integration-browser-check`,
`llm_smoke`). Toolchain is pinned to Rust 1.80 (MSRV) via
`rust-toolchain.toml` — no manual channel selection needed. For
faster iterative builds use `cargo build --profile release-fast`
(same opt-level, no LTO, ~50 % quicker).

### Prerequisites

- **Rust** 1.80+ (`rustup` recommended)
- **NATS** is *optional* — the daemon defaults to `broker.type:
  local` (an in-process stdio bridge, no external server). Run a
  NATS server only for multi-host clusters:
  ```bash
  docker run -p 4222:4222 nats:2.10-alpine
  nexo set-broker nats --url nats://localhost:4222
  ```
  See [broker shapes](../architecture/broker-shapes.md) /
  [broker.yaml](../config/broker.md).
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
