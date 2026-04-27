# Packaging — operator notes

This directory holds the release-time recipes that turn a tagged
commit into installable artifacts. Each subdirectory targets a
distribution channel:

| Subdir      | Channel                                                    | Phase |
| ----------- | ---------------------------------------------------------- | ----- |
| `debian/`   | `apt install nexo-rs` (.deb)                               | 27.4  |
| `rpm/`      | `dnf install nexo-rs` (.rpm)                               | 27.4  |
| `termux/`   | `pkg install nexo-rs` (.deb under Termux `$PREFIX`)        | 27.8  |
| `homebrew/` | `brew install nexo-rs` formula — **PARKED** (Phase 27.2 spec) | 27.6  |

Phase 27.2 reduced the supported binary matrix to **Linux + Termux**.
Apple (`x86_64`/`aarch64-apple-darwin`) and Windows
(`x86_64-pc-windows-msvc`) targets are deliberately not shipped today.
Reviving them: add the targets back to
[`dist-workspace.toml`](../dist-workspace.toml), restore the matrix
entries in [`.github/workflows/release.yml`](../.github/workflows/release.yml),
and revive `packaging/homebrew/`.

The cross-target binary tarballs are produced by
[`cargo-dist`](https://opensource.axo.dev/cargo-dist/) — config lives
in [`dist-workspace.toml`](../dist-workspace.toml). The Termux `.deb`
is produced by [`packaging/termux/build.sh`](./termux/build.sh) and
runs alongside cargo-dist as a separate job in the release workflow.

## Building release artifacts locally

The Phase 27.2 shippable matrix:

| Target                          | Tool                                       | Output                                                  |
| ------------------------------- | ------------------------------------------ | ------------------------------------------------------- |
| `x86_64-unknown-linux-musl`     | `cargo-dist` + `cargo-zigbuild`            | `nexo-rs-x86_64-unknown-linux-musl.tar.xz` (+ sha256)   |
| `aarch64-unknown-linux-musl`    | `cargo-dist` + `cargo-zigbuild`            | `nexo-rs-aarch64-unknown-linux-musl.tar.xz` (+ sha256)  |
| `aarch64-linux-android` (Termux) | `packaging/termux/build.sh` + `cargo-zigbuild` | `nexo-rs_<version>_aarch64.deb` (+ sha256)              |

### One-time setup

```bash
# 1. cargo-dist itself (the binary is named `dist`, not `cargo-dist`).
cargo install cargo-dist --locked --version 0.31.0

# 2. cargo-zigbuild — needed for every musl + Termux build.
cargo install cargo-zigbuild --locked --version 0.22.3

# 3. zig — cargo-zigbuild dispatches to a `zig` on PATH. PIN to 0.13.0:
#    cargo-zigbuild 0.22.x is incompatible with zig 0.14+ / 0.16
#    (target-triple parsing changed upstream).
pipx install ziglang==0.13.0
ln -sf "$(which python-zig)" ~/.cargo/bin/zig

# 4. rustup targets.
rustup target add x86_64-unknown-linux-musl
rustup target add aarch64-unknown-linux-musl
rustup target add aarch64-linux-android       # Termux (bionic libc)
```

The CI runners in [`.github/workflows/release.yml`](../.github/workflows/release.yml)
use the same pins (zig 0.13.0, cargo-zigbuild 0.22.3, cargo-dist 0.31.0)
— if a local build works, the CI build works.

### Run the local smoke gate

```bash
make dist-check
```

Invokes `dist build --artifacts=local --tag nexo-rs-v$VERSION --target
$HOST_TARGET` (default `x86_64-unknown-linux-musl`) followed by
[`scripts/release-check.sh`](../scripts/release-check.sh), which:

1. verifies each tarball contains `nexo`, both `LICENSE-*` files, and `README.md`
2. checks each tarball's sha256 against its sidecar
3. extracts the host-native tarball and confirms `./nexo --version`
   prints `nexo $VERSION`
4. validates the Termux `.deb` sha256 sidecar (when present; the
   bionic-libc binary cannot be smoke-run on this host)

Targets the local toolchain can't satisfy emit `[release-check] WARN`
lines instead of failing the gate.

### Force a specific target

```bash
HOST_TARGET=aarch64-unknown-linux-musl make dist-check
```

### Build only the Termux `.deb`

```bash
bash packaging/termux/build.sh
# → dist/nexo-rs_<version>_aarch64.deb + .sha256
```

The script cross-compiles via `cargo zigbuild --target aarch64-linux-android`,
stages the deb tree under `data/data/com.termux/files/usr/`, and emits
the sha256 sidecar consumed by the release workflow.

## How this interacts with `release-plz`

Two complementary tools, no overlap:

- **`release-plz`** ([`release-plz.toml`](../release-plz.toml)) owns
  version bumps, git-tag creation (`nexo-rs-v<version>`), `crates.io`
  publish, and per-crate `CHANGELOG.md` regeneration.
- **`cargo-dist` + `release.yml`** owns binary tarball production +
  Termux `.deb` build + uploading them to the GH release that
  release-plz already created. Triggered by the same
  `nexo-rs-v<version>` tag push.

Adding `git-cliff` or `release-please` would duplicate `release-plz`
and is intentionally avoided.
