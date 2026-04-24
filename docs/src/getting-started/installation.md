# Installation

## Prerequisites

- **Rust** 2021 edition (toolchain `stable`, components `rustfmt` + `clippy`)
- **NATS** running locally or reachable over the network
- **Chrome / Chromium** (only if you plan to use the browser plugin)
- **Git** (the memory subsystem uses per-agent workspace-git)

### Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add rustfmt clippy
```

The repo pins the toolchain via `rust-toolchain.toml`, so no manual
channel selection is needed.

### NATS

For development:

```bash
docker run -p 4222:4222 nats:2.10-alpine
```

For production, see [broker.yaml](../config/broker.md) — clustering,
TLS, and per-user/token auth are supported.

## Build

```bash
git clone git@github.com:lordmacu/nexo-rs.git
cd nexo-rs
cargo build --release
```

The workspace compiles 18 crates and 4 binaries: `agent`,
`browser-test`, `integration-browser-check`, `llm_smoke`.

## Verification

```bash
./target/release/agent --help
cargo test --workspace --lib
```

## Native install (no Docker)

If you'd rather skip Docker entirely — run the agent as a plain
process under systemd / launchd, or just local dev — see the full
walkthrough and use the bootstrap script:

```bash
./scripts/bootstrap.sh
```

Details: [Native install](./install-native.md).

## Next

- [Quick start](./quickstart.md) — first agent running in five minutes
- [Setup wizard](./setup-wizard.md) — pair channels and wire secrets
- [Native install](./install-native.md) — detailed no-Docker setup
