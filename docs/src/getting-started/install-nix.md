# Install — Nix

Nexo ships a [Nix flake](https://nixos.wiki/wiki/Flakes) that pins
the toolchain (Rust 1.80, MSRV) and the native build deps so a
contributor or operator can go from clean shell to working binary
without touching the host system.

## Run without installing

```bash
nix run github:lordmacu/nexo-rs -- --help
```

First invocation builds from source (~3-5 min on cold cache);
subsequent runs hit the local Nix store.

## Build a local binary

```bash
nix build github:lordmacu/nexo-rs
./result/bin/nexo --help
```

The binary is the same `nexo` produced by `cargo build --release
--bin nexo`. Outputs a `result/` symlink the operator can link
into `/usr/local/bin/` or copy elsewhere.

## Contributor dev shell

```bash
git clone https://github.com/lordmacu/nexo-rs
cd nexo-rs
nix develop
```

Drops you into a shell with:

- `rustc` 1.80 + `cargo` + `clippy` + `rustfmt` + `rust-src`
- `cargo-edit`, `cargo-watch`, `cargo-nextest`, `cargo-deny`
- `mdbook` + `mdbook-mermaid` (for `mdbook build docs`)
- `sqlite`, `pkg-config`, `openssl`, `libgit2` (build deps)

`RUST_LOG=info` is exported by default. The toolchain version is
pinned in `flake.nix` — bump in lockstep with
`[workspace.package].rust-version` in `Cargo.toml`.

## What the flake does NOT install

The `nexo` binary alone is not enough for full functionality.
Runtime tools the channel plugins shell out to live at the system
level, not in the flake:

- **Chrome / Chromium** — required by the browser plugin
- **`cloudflared`** — used by the tunnel plugin
- **`ffmpeg`** — media transcoding for WhatsApp voice notes
- **`tesseract-ocr`** — OCR skill
- **`yt-dlp`** — the `yt-dlp` extension

Operators install these via their distro's package manager. The
[native install guide](./install-native.md) lists the apt / pacman
/ brew commands. The [Docker image](../ops/docker.md) bundles all
of them — that's the path of least friction for a "just works"
deploy.

## Pinning a release

Once `v*` tags are published, pin to a specific release:

```bash
nix run github:lordmacu/nexo-rs/v0.1.1 -- --help
```

Or in a flake input:

```nix
{
  inputs.nexo-rs.url = "github:lordmacu/nexo-rs/v0.1.1";
}
```

## Verifying the build

```bash
nix flake check
```

Runs `nix flake check` — verifies the flake metadata, evaluates
all outputs (`packages`, `apps`, `devShells`, `formatter`) without
actually building. Useful in CI to catch flake regressions early.

## Troubleshooting

- **"experimental feature 'flakes' is disabled"** — add to
  `~/.config/nix/nix.conf`:
  ```
  experimental-features = nix-command flakes
  ```
- **First build is very slow** — the build re-fetches and re-compiles
  every cargo dependency in the sandbox. Subsequent builds are
  cached. A future Phase 27.x will publish a `cachix` cache so
  `nix build` pulls the binary directly.
- **Build fails on macOS arm64** — `git2-rs` occasionally lags on
  Apple silicon. Workaround: build the binary inside the Docker
  image instead (see [Docker](../ops/docker.md)).
