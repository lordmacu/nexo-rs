# Phase 27.2 multi-platform release — session handoff

> **Status as of 2026-05-10 17:15 UTC:** Multi-platform RC pivot
> blocked by deep cross-repo + upstream issues. Apple Silicon
> alone is durably green. Linux musl + Windows each need
> half-day-to-day of multi-repo coordination. **This document
> is the canonical resume point** — everything learned across
> 6 RC iterations, the local-repro debug path, and concrete
> next steps. A fresh session reads only this file.

## TL;DR

Goal: ship `nexo-rs-v0.1.6` GA across Linux musl + macOS +
Windows + Termux. After 6 RC validations:

- **Apple Silicon** (`aarch64-apple-darwin`): ✅ green twice,
  rock solid
- **Apple Intel** (`x86_64-apple-darwin`): ⏳ runner-pool
  congestion every take, never reaches build, but pipeline is
  identical to Silicon (expected to pass when allocated)
- **Linux musl x2**: ❌ multiple deep blockers (sqlite-vec C
  source incompatible with musl libc + openssl-sys via
  wa-agent upstream — both cross-repo)
- **Windows MSVC**: ❌ similar profile (multiple Unix-only
  deps, possibly more under what we've cfg-gated)
- **Termux**: ⏭️ deferred (rustls aws-lc-rs cross-repo
  migration tracked separately)

**Pragmatic recommendation for next session**: ship `0.1.6`
GA with Apple-only matrix. Linux musl + Windows + Termux as
multi-day cross-repo follow-ups.

## Iteration history (6 takes)

| Take | Run ID | Failure | Lesson |
|---|---|---|---|
| 1 | `25630912504` | cargo-dist tag mismatch (Cargo.toml said 0.1.6, tag said 0.1.6-rc1) | Bump root crate to `0.1.6-rc1` so versions match |
| 2 | (same tag, abandoned) | — | — |
| 3 | `25631196811` | Termux aws-lc-sys cross-compile fail | Disabled build-termux, removed from publish.needs |
| 4 | `25631524914` (docker) + `25632704164` (release) | Docker: archived `admin-ui/` referenced. Release: ring's musl-gcc lookup; Windows Unix sockets in nexo-driver-loop | Dockerfile cleaned. musl-tools apt-installed. driver-loop/socket cfg(unix). |
| 5 | `25633713871` | Linux musl: sqlite-vec u_int8_t typedef. Windows: nix crate in nexo-dream. Apple+Windows: cargo-dist failed to find mock_subprocess_plugin.exe | _DEFAULT_SOURCE CFLAGS (didn't help — root cause is sqlite-vec C bug). nix cfg(unix) in dream. required-features="dev-bins" (cargo-dist 0.31 IGNORES required-features) |
| 6 | `25634671603` (cancelled) | Malformed `binaries = ["nexo"]` (sequence not map per cargo-dist schema) | binaries config field expects map shape, not array — needs different syntax |

## Confirmed blockers (root-cause analysis)

### A. Linux musl: sqlite-vec C source incompatible

`sqlite-vec-0.1.9/sqlite-vec.c` lines 64-72:

```c
#ifndef _WIN32
#ifndef __EMSCRIPTEN__
#ifndef __COSMOPOLITAN__
#ifndef __wasi__
typedef u_int8_t uint8_t;
typedef u_int16_t uint16_t;
typedef u_int64_t uint64_t;
#endif
#endif
#endif
#endif
```

This block triggers on Linux musl (no `_WIN32`, no
`__wasi__`, etc.) but musl libc deliberately omits BSD typedefs
`u_int8_t/16/64_t`. They're not POSIX — they're 4.4BSD legacy.

**Tried and didn't work:**
- `CFLAGS_x86_64_unknown_linux_musl=-D_DEFAULT_SOURCE -D_GNU_SOURCE`
  — these expose more glibc surface but don't add BSD typedefs
  to musl

**Possible fixes (untested in CI):**
1. `CFLAGS_..._musl="-Du_int8_t=uint8_t -Du_int16_t=uint16_t -Du_int64_t=uint64_t"`
   — preprocessor substitution makes the typedef self-referential
   (valid in C99 if `uint8_t` already defined via stdint.h)
2. `[patch.crates-io] sqlite-vec = { git = "https://github.com/<fork>/sqlite-vec", branch = "musl-fix" }`
   — fork upstream, fix the typedef block to wrap with
   `#ifdef __GLIBC__`, push, point Cargo.toml at the fork
3. Bump to `sqlite-vec = "0.1.10-alpha.3"` — alpha may have
   the fix, untested

**Recommended path:** option 2 (fork). Long-term: file PR
upstream at https://github.com/asg017/sqlite-vec.

### B. Linux musl: openssl-sys via wa-agent (cross-repo)

`cargo tree -p nexo-rs -i openssl-sys --target x86_64-unknown-linux-musl`:

```
openssl-sys
└─ native-tls
   └─ tokio-native-tls
      └─ tokio-tungstenite v0.24.0
         └─ wa-agent v0.1.6
            └─ nexo-plugin-whatsapp v0.1.3
               └─ nexo-rs
```

`wa-agent` (upstream WhatsApp client) uses `tokio-tungstenite`
with default features which enables `native-tls`. That pulls
`openssl-sys` which doesn't cross-compile to musl without a
musl-built openssl in the linker path.

**Fix path** (multi-repo):

1. PR to upstream `wa-agent` (`whatsapp-rs/Cargo.toml`):
   ```toml
   tokio-tungstenite = { version = "0.24",
       default-features = false,
       features = ["rustls-tls-webpki-roots"] }
   ```
2. wa-agent publishes new version
3. `nexo-rs-plugin-whatsapp` Cargo.toml bumps wa-agent to new
   version
4. plugin-whatsapp publishes
5. proyecto Cargo.toml bumps `nexo-plugin-whatsapp` dep

This is the SAME shape as the Termux aws-lc-rs migration
already tracked under FOLLOWUPS.md as 27.2-follow-up.b. Could
batch both fixes into one cross-repo wave.

### C. Windows: cargo-dist binaries map (this session's last fix attempt)

Take 6 push (commit `73eabd2`) added:
```toml
[package.metadata.dist]
binaries = ["nexo"]
```

That format is wrong — cargo-dist expects a map, not an array.
The schema is at <https://axodotdev.github.io/cargo-dist/book/reference/config.html>.

The CORRECT mechanism per cargo-dist docs is **either**:

a) `bin-aliases` at workspace level (map: alias → bin name):
```toml
[workspace.metadata.dist]
bin-aliases = { nexo = ["nexo"] }
```

b) Convert `mock_subprocess_plugin` to a separate test crate
   under `crates/test-fixtures/mock-subprocess-plugin/` with
   `[package.metadata.dist] dist = false` so cargo-dist ignores
   it entirely.

**Recommended path:** option (b) is more idiomatic and
avoids fighting cargo-dist's manifest planner. The
`dev-bins` feature flag introduced in commit `24b8db2` is
also redundant once the bin moves to its own crate.

### D. Windows: more Unix-only deps likely under nix cfg-gate

The take 4-5 surfaces:
- `tokio::net::UnixListener` in `nexo-driver-loop/src/socket.rs` (FIXED)
- `nix::sys::signal` + `nix::unistd::Pid` in `nexo-dream/src/consolidation_lock.rs` (FIXED)

Once Linux musl unblocks and Windows compiles further, more
Unix-only deps will probably surface. Likely candidates to
audit pre-emptively:

- `caps` crate (Linux capabilities) — search `grep -rn "use caps" crates/`
- `signal-hook` — same
- File mode setting (`PermissionsExt`, `Mode`) — should already be cfg-gated but worth checking
- POSIX-specific paths (`/run/secrets`, `/etc/`)

```bash
# Audit command:
cd /home/familia/chat/proyecto
grep -rn "use nix\|use caps\|signal_hook\|use std::os::unix" crates/*/src 2>&1 | grep -v "cfg(unix)"
```

## Local-repro debug path (the productive iteration loop)

Instead of waiting 10-25 min per CI run, reproduce locally
with the same toolchain. Total cycle: ~2 min compile + see
error.

### One-time setup

```bash
# Install zig 0.13.0 (CI uses this version; local has 0.16 which
# may behave differently — pin to 0.13 for fidelity)
mkdir -p ~/.local/share/zig-0.13
curl -L https://ziglang.org/download/0.13.0/zig-linux-x86_64-0.13.0.tar.xz \
  | tar -xJ -C ~/.local/share/zig-0.13 --strip-components=1
export PATH="$HOME/.local/share/zig-0.13:$PATH"
zig version  # should print 0.13.0

# Tools (already installed on dev box per cargo-dist version match)
cargo install cargo-zigbuild --locked --version 0.22.3
cargo install cargo-dist     --locked --version 0.31.0

# musl headers/libs (Linux only)
sudo apt-get install -y musl-tools

# Add Rust target
rustup target add x86_64-unknown-linux-musl
rustup target add aarch64-unknown-linux-musl
```

### Reproduce CI musl build

```bash
cd /home/familia/chat/proyecto

# Match the env vars CI sets
export CFLAGS_x86_64_unknown_linux_musl="-D_DEFAULT_SOURCE -D_GNU_SOURCE"
export CFLAGS_aarch64_unknown_linux_musl="-D_DEFAULT_SOURCE -D_GNU_SOURCE"
export NEXO_BUILD_CHANNEL=tarball-x86_64-unknown-linux-musl
export NEXO_BUILD_GIT_SHA=local-test

# Direct cargo path (skips cargo-dist's bin-discovery layer)
cargo zigbuild --release --bin nexo --target x86_64-unknown-linux-musl 2>&1 | tee /tmp/musl-build.log

# Or full cargo-dist path (matches CI exactly)
dist build \
  --artifacts=local \
  --tag nexo-rs-v0.1.6-rc1 \
  --target x86_64-unknown-linux-musl
```

### Why local repro caught more than CI

In ~10 minutes of local iteration we saw:
1. sqlite-vec u_int8_t typedef (CI take 5 surfaced this same
   error)
2. openssl-sys via wa-agent (CI never even reached this
   because sqlite-vec failed first; local got past sqlite-vec
   with the macro hack)
3. cargo-dist's `binaries` field expects a map (CI take 6
   surfaced this; local saw it as `Malformed metadata.dist`
   with a clearer error message)

**The local debug loop is mandatory for next session's
progress** — CI iteration is too slow + opaque to be the
primary feedback channel.

### Reproduce Apple build (if a Mac is available)

```bash
# On a macOS host:
rustup target add aarch64-apple-darwin
cargo install cargo-dist --locked --version 0.31.0
NEXO_BUILD_CHANNEL=tarball-aarch64-apple-darwin \
  dist build --artifacts=local --tag nexo-rs-v0.1.6-rc1 \
  --target aarch64-apple-darwin
```

We KNOW this works (CI proved it twice). Local Mac repro is
useful only if Apple regresses.

### Reproduce Windows build

```bash
# On a Windows host (or Win11 in a VM, or via WSL with cross):
rustup target add x86_64-pc-windows-msvc
cargo build --release --bin nexo --target x86_64-pc-windows-msvc
```

WSL won't give a true Windows binary (it'll be Linux). Need
real Windows for this. If you don't have a Win box, the only
option is iterating via CI.

## Resume checklist for next session

```bash
# 1. Catch up — read this doc + the tail of the failed runs
cd /home/familia/chat/proyecto
gh run list -w release.yml -L 3

# 2. Decide the scope:
#    A. Mac-only 0.1.6 GA (fastest)
#    B. Mac + Linux musl after fixing sqlite-vec + wa-agent
#       (multi-repo)
#    C. Full matrix (multi-day)

# 3. If option A:
sed -i 's|"x86_64-unknown-linux-musl",||; s|"aarch64-unknown-linux-musl",||; s|"x86_64-pc-windows-msvc",||' \
  dist-workspace.toml
# Edit release.yml — comment out build-musl + build-windows jobs.
# Tag GA:
gh release create nexo-rs-v0.1.6 --target main \
  --title "v0.1.6 — Apple-only first multi-platform release" \
  --notes "..."

# 4. If option B (the cross-repo path):
#    a. Fork sqlite-vec, fix u_int8_t block, point [patch.crates-io]
#       at fork. Test local repro green.
#    b. PR to wa-agent (whatsapp-rs repo) to swap to rustls-tls.
#    c. Wait for wa-agent publish.
#    d. Bump nexo-rs-plugin-whatsapp.
#    e. Bump proyecto deps to new versions.
#    f. Local repro musl green.
#    g. Push + tag rc1 + iterate from there.

# 5. If option C: schedule a multi-day push, NOT in a single
#    session.
```

## Repo state at handoff

### Local-only changes (uncommitted at handoff)

```
M Cargo.toml — `binaries = ["nexo"]` line REMOVED locally
              (commit 73eabd2 has the malformed line; needs
              another commit to revert OR replace with
              `bin-aliases` map per option (a) above)
```

### Files committed in this session (proyecto/)

```
M Cargo.toml                                        — version 0.1.6→0.1.6-rc1; dev-bins feature; required-features on mock_subprocess_plugin; (broken `binaries=[…]` in HEAD)
M Cargo.lock                                        — auto-regen
M Dockerfile                                        — drop admin-ui-builder stage
M FOLLOWUPS.md                                      — Phase 27.2 follow-up.b (Termux aws-lc-rs)
M README.md                                         — install snippet now multi-channel
M dist-workspace.toml                               — 5 targets (added apple-x2, windows)
M docs-site/index.html                              — landing rewrite (OpenClaw-first), logo slot, Docker install
A docs-site/README.md                               — landing edit guide
A docs-site/assets/logo.svg                         — placeholder logo (replace this file to swap branding)
M docs/src/SUMMARY.md                               — added platform-support page
A docs/src/getting-started/platform-support.md      — per-OS prereq matrix
M docs/src/plugins/telegram.md                      — fixed broken PHASES.md link
M docs/src/plugins/whatsapp.md                      — fixed broken PHASES.md link
M .github/workflows/docker.yml                      — auto-publish enabled, nexo-rs-v* tag pattern
M .github/workflows/release.yml                     — apple/windows jobs, musl-tools, _DEFAULT_SOURCE CFLAGS, build-termux disabled
M crates/driver-loop/src/lib.rs                     — #[cfg(unix)] socket module
M crates/driver-loop/src/orchestrator.rs            — #[cfg(unix)] socket bind, no-op handle on Windows
M crates/dream/src/consolidation_lock.rs            — #[cfg(unix)] nix imports, tasklist fallback on Windows
A PHASE-27-2-MULTIPLATFORM-RELEASE-HANDOFF.md       — this doc
```

### Out-of-tree artifacts created

- `lordmacu/homebrew-nexo-rs` repo — placeholder Homebrew tap
- `@nexo-rs/cli` npm package — placeholder reservation
  (v0.0.1-placeholder.0)
- npm scope `@nexo-rs` org created
- `NPM_TOKEN` GH secret set in lordmacu/nexo-rs

## Distribution channels — current state

| Channel | Status | Where |
|---|---|---|
| Curl shell installer | ⏳ Apple Silicon-only would work today | cargo-dist `installers = ["shell"]` |
| GH Releases tarballs | ⏳ Same | Per-target `.tar.xz` |
| Homebrew tap | ✅ Repo exists, formula auto-publish wired | `lordmacu/homebrew-nexo-rs` |
| npm scope | ✅ Reserved with placeholder | `@nexo-rs/cli` |
| Docker (ghcr.io) | ✅ Auto-publish enabled | `:edge` per push to main; `:latest` per GA tag |
| crates.io | ✅ Already publishing | release-plz handles per-crate |
| GH Pages landing + docs | ✅ Live | `lordmacu.github.io/nexo-rs/` (landing) + `/docs/` (mdBook) |

## Out-of-scope reminders for next session

DO NOT in this Phase 27.2 effort:

- Re-enable Termux release without first fixing wa-agent's
  TLS backend (will surface aws-lc-sys + openssl-sys
  simultaneously)
- Add MSI / PowerShell installers (need Windows green first)
- Add a named-pipe Windows alternative for the
  permission-prompt forwarder (architectural, not a release
  blocker)
- Touch the lifted SDK modules (sanitize, media,
  email_template, module_state, compose_*, db_migrate) — they
  landed in this session, are independently working, and
  should not be entangled with the release blockers

## Session metadata

- **Session length:** ~7h
- **Commits to proyecto:** 14 (search log for `447b5ca`
  through `73eabd2`)
- **Other artifacts:** homebrew tap repo + @nexo-rs/cli npm
  package
- **RC tag iterations:** 6 (`nexo-rs-v0.1.6-rc1` deleted +
  recreated)
- **Last failed run before handoff:** `25634671603` (take 6,
  cancelled)
- **Local repro confirmed working tools:** cargo-zigbuild
  0.22.3, cargo-dist 0.31.0, musl-tools, x86_64-unknown-linux-musl
  Rust target

> **Key insight from this session:** the project's binary
> never shipped multi-platform end-to-end before. Each rc
> surfaced previously latent platform-specific bugs in the
> workspace dep graph + sibling repos. The fixes are real
> engineering work (cross-repo coordination, upstream
> patches, sqlite-vec fork) — NOT something one focused
> session can clear. **Local repro is the productive
> iteration loop**; CI takes 10-25 min per cycle and obscures
> the actual error. The next session should default to local
> validation first, only push to CI when local is green.
