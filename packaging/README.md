# Packaging — operator notes

This directory holds the release-time recipes that turn a tagged
commit into installable artifacts. Each subdirectory targets a
distribution channel:

| Subdir | Channel | Phase |
| ------ | ------- | ----- |
| `debian/` | `apt install nexo-rs` (.deb) | 27.4 |
| `rpm/`    | `dnf install nexo-rs` (.rpm) | 27.4 |
| `homebrew/` | `brew install nexo-rs` formula | 27.6 |
| `termux/` | `pkg install nexo-rs` (.deb under Termux `$PREFIX`) | 27.8 |

The cross-target binary tarballs that feed those channels are produced
by [`cargo-dist`](https://opensource.axo.dev/cargo-dist/) — config
lives in [`dist-workspace.toml`](../dist-workspace.toml). This file
documents how to rebuild them locally.

## Building release artifacts locally

The full Phase 27 matrix is:

- `x86_64-unknown-linux-gnu` — local-host fallback (always builds)
- `x86_64-unknown-linux-musl` — shippable Linux glibc-free build
- `aarch64-unknown-linux-musl` — shippable Linux ARM build
- `x86_64-apple-darwin` / `aarch64-apple-darwin` — macOS Intel / Apple Silicon
- `x86_64-pc-windows-msvc` — Windows tier-2 (bin only, runtime plugins not validated)

A stock developer Linux box will only build the gnu fallback out of
the box — the other targets need extra toolchains. Phase 27.2 wires
the GH-Actions release workflow that runs the full matrix on
purpose-built CI runners; locally it's enough to validate the gnu
target end-to-end and trust CI for the rest.

### One-time setup

```bash
# 1. cargo-dist itself (binary is named `dist`, not `cargo-dist`).
cargo install cargo-dist --locked --version 0.31.0

# 2. cargo-zigbuild — needed for Linux musl cross builds. Optional
#    for the gnu fallback; required if you want to validate musl
#    locally.
cargo install cargo-zigbuild --locked

# 3. zig itself — cargo-zigbuild dispatches to a `zig` on PATH.
#    Recommended on a developer box: pipx (avoids PEP-668 conflicts
#    with system Python).
pipx install ziglang
ln -sf "$(which python-zig)" ~/.cargo/bin/zig

# 4. The rustup target.
rustup target add x86_64-unknown-linux-gnu      # always
rustup target add x86_64-unknown-linux-musl     # for musl validation
rustup target add aarch64-unknown-linux-musl    # for ARM validation
```

Note: the `python-zig` shipped via `ziglang` 0.16.0 is NOT compatible
with `cargo-zigbuild` 0.22.x — the triple parsing changed in zig 0.16
and cargo-zigbuild hasn't caught up. Until that thaws, full musl
validation is CI-only. Track upstream:
<https://github.com/rust-cross/cargo-zigbuild/issues>.

### Run the local smoke gate

```bash
make dist-check
```

This invokes `dist build --artifacts=local --tag nexo-rs-v$VERSION
--target $(rustc -vV | sed -n 's|host: ||p')` followed by the
[`scripts/release-check.sh`](../scripts/release-check.sh) smoke gate,
which verifies:

1. each tarball that was actually built contains `nexo` (the bin),
   both `LICENSE-*` files, and `README.md`
2. each tarball's sha256 matches its sidecar (when emitted)
3. the host-native tarball extracts and `./nexo --version` prints
   `nexo $VERSION`

Targets the local toolchain can't satisfy emit `[release-check] WARN`
lines instead of failing the gate. Phase 27.2 CI enforces the full
matrix.

### Force a specific target

```bash
HOST_TARGET=aarch64-unknown-linux-musl make dist-check
```

The `HOST_TARGET` Makefile variable overrides what `dist build
--target` is invoked with.

## How this interacts with `release-plz`

Two complementary tools:

- **`release-plz`** owns version bumps, tag creation, `crates.io`
  publish, and per-crate `CHANGELOG.md` regeneration. Configured in
  [`release-plz.toml`](../release-plz.toml).
- **`cargo-dist`** owns binary tarball production for the targets
  above plus generating the `curl | sh` / PowerShell installers. It
  fires on the same tags `release-plz` creates (`nexo-rs-v<version>`)
  and uploads tarballs to the same GH release.

There is no overlap — adding `git-cliff` or `release-please` would
duplicate `release-plz` and is intentionally avoided.
