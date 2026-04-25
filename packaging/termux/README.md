# Termux packaging

Recipe for building a `.deb` that `pkg install` can consume on a
Termux Android shell. Targets `aarch64-linux-android` (the only
supported Termux arch).

## Files

- `build.sh` — produces `dist/nexo-rs_<version>_aarch64.deb`
- `pkg/` — placeholder for the Termux pkg index served at
  `https://lordmacu.github.io/nexo-rs/termux/` once the release
  pipeline (Phase 27.2) wires the upload

## Local build

Cross-compile path (host = Linux x86_64 typically):

```bash
cargo install cargo-zigbuild
pip install ziglang
rustup target add aarch64-linux-android
packaging/termux/build.sh
```

Output: `dist/nexo-rs_0.1.1_aarch64.deb`.

Native build path (run inside Termux on the phone itself):

```bash
pkg install rust git
git clone https://github.com/lordmacu/nexo-rs
cd nexo-rs
cargo build --release --bin nexo
packaging/termux/build.sh --binary target/release/nexo
```

## Install on a phone

Once the deb is on the device:

```bash
pkg install ./nexo-rs_0.1.1_aarch64.deb
```

The `postinst` scaffolds `~/.nexo/{data,secret}` on first install
and prints next-steps (`nexo --help`, `nexo setup`, `nexo doctor`).

## Why Termux gets its own deb

Termux libc is **bionic**, not glibc, and the runtime layout is
`/data/data/com.termux/files/usr/`, not `/usr/`. A standard Debian
deb won't run. Cross-compiling with `cargo-zigbuild` produces a
binary linked against bionic via the Android NDK target and lays
out the staging tree under the Termux `$PREFIX`.

Termux pre-ships the runtime tools `nexo` shells out to (sqlite,
openssl, git, ffmpeg, tesseract, python, yt-dlp). The `Depends:`
field pulls the hard ones; `Recommends:` covers the optional skill
deps so a minimal install still works.

## Defaults adjusted for phones

The Termux package keeps the same `nexo` binary as desktop, but
the bundled `config/` recommends `broker.type: local` (no NATS
daemon needed on a phone). Operators can switch to `broker.type:
nats` later when they wire a server.

## Pkg index (future)

`.github/workflows/release.yml` (Phase 27.2) uploads the deb to
the GitHub release. A follow-up will publish a Termux-formatted
pkg index at `lordmacu.github.io/nexo-rs/termux/` so users can
add it as a repo:

```bash
echo "deb https://lordmacu.github.io/nexo-rs/termux/ stable main" \
  > $PREFIX/etc/apt/sources.list.d/nexo-rs.list
pkg update
pkg install nexo-rs
```

That side is tracked under the Phase 27.2 deliverable; today the
.deb is uploaded as a single artifact per release.

## Limitations

- **No browser plugin** — Chrome / Chromium is not on Termux. The
  browser plugin is automatically disabled on `aarch64-linux-android`
  builds via cfg-gating (Phase 4 work).
- **No `cloudflared`** — same story; tunnel plugin needs a
  desktop / VPS host.
- **NAT / battery** — phones sleep, NATs drop. The runtime
  reconnects but expect noisier logs than a server install.
