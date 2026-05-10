# Phase 27.2 multi-platform release — session handoff

> **Status as of session pause (2026-05-10 16:30 UTC):** RC validation
> in iteration 5. Apple Silicon proven solid; Linux musl + Windows
> still blocking. Termux deferred to a separate cross-repo migration.
> This doc captures every blocker + fix-path so a fresh session
> picks up without repeating the diagnosis cycle.

## Goal

Ship `nexo-rs-v0.1.6` as the first multi-platform binary release.
Five build targets driven by `cargo dist`:

- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Plus the existing Termux `.deb` (out of scope this iteration —
separate `aws-lc-rs → ring` cross-repo migration).

The `release.yml` workflow fires on `nexo-rs-v*` tag push and
populates a GH Release with the tarballs. cargo-dist's universal
shell installer auto-detects platform.

## Confirmed working

| Target | Last result | Notes |
|---|---|---|
| `aarch64-apple-darwin` | ✅ green twice (rc1 take 3 + take 4) | Reliably passes; this is the rock |
| `x86_64-apple-darwin` | ⏳ never reached the runner — queued in every take | `macos-13` runner pool is congested. Pipeline is identical to Apple Silicon, expected to pass once allocated |
| `validate-tag` | ✅ green every take | Tag-version-vs-Cargo.toml-version match enforced |
| Termux build | ⏭️ `if: false` since take 4 | Intentional skip; tracked separately in FOLLOWUPS.md |

## Validation history — 5 takes, what each surfaced

### Take 1 (release run `25630912504`)

**Cause:** `cargo dist build` rejected the tag because Cargo.toml
said `0.1.6` but the tag claimed `0.1.6-rc1`.

**Fix shipped:** Cargo.toml `[package] version = "0.1.6-rc1"` so
the tag matches.

### Take 2 (recreated tag)

Same root cause as take 1; the version bump propagated
correctly but the workflow run was cancelled before it
finished.

### Take 3 (run `25631196811`)

**Cause:** Termux `aarch64-linux-android` build pulled `aws-lc-sys`
transitively (rustls 0.23 → hyper-rustls → reqwest with default
features). cargo-zigbuild's Android shim doesn't expose
`sys/types.h` to the C compiler aws-lc-sys's `cc-rs` invokes:

```
fatal error: 'sys/types.h' file not found
```

**Fix shipped:** Disabled the `build-termux` job (`if: false`) +
removed it from `publish.needs:`. Tracked the multi-repo fix
under `FOLLOWUPS.md` "Phase 27.2 follow-up.b".

### Take 4 (run `25632704164`)

Two new failures surfaced once Termux stopped masking them:

**a) Linux musl** — both x86_64 and aarch64:

```
ring v0.17.14: Compiler family detection failed:
  failed to find tool "x86_64-linux-musl-gcc"
```

`ring`'s `cc-rs` build script requires the literal target gcc
binary even when cargo-zigbuild handles linking.

**Fix shipped:** Added `apt install musl-tools` step to the
`build-musl` job in `release.yml`.

**b) Windows MSVC**:

```
unresolved imports tokio::net::UnixListener, tokio::net::UnixStream
```

`crates/driver-loop/src/socket.rs` used Unix-domain sockets
unconditionally; Windows doesn't ship them.

**Fix shipped:** `#[cfg(unix)]` gated the entire `socket` module
+ its `pub use` re-export + the orchestrator's bind/spawn
block. Windows substitutes a no-op `tokio::spawn(async {
Ok(()) })` for the socket handle so shutdown path works
without changes. Trade-off documented: the
`DriverSocketServer` permission-prompt forwarder is unavailable
on Windows; users go through WSL or wait for a named-pipe
follow-up.

**c) Docker** workflow (separate run `25631524914`):

```
"/admin-ui": not found
```

Dockerfile referenced `admin-ui/` that was archived in commit
`575cb78` (admin-ui became its own microapp).

**Fix shipped:** Dropped the `admin-ui-builder` Node stage from
the Dockerfile.

### Take 5 (run `25633713871` — currently in progress at session pause)

Three new failures showed up once the take-4 fixes landed:

**a) Linux musl** — `sqlite-vec.c`:

```
sqlite-vec.c:68:9: error: unknown type name 'u_int8_t'
sqlite-vec.c:69:9: error: unknown type name 'u_int16_t'
sqlite-vec.c:70:9: error: unknown type name 'u_int64_t'
```

BSD typedefs that musl libc hides without `_DEFAULT_SOURCE`.

**Fix shipped (in take 5 push):** Set
`CFLAGS_x86_64_unknown_linux_musl=-D_DEFAULT_SOURCE -D_GNU_SOURCE`
+ same for aarch64 in `release.yml`.

**Status:** Linux musl jobs failed again in take 5 — fix may
not have been picked up correctly, or there's another C
typedef issue further in. **NEEDS INVESTIGATION** in the next
session (see "Open blockers" below).

**b) Windows** — `nix` crate:

```
unresolved import nix::sys
unresolved import nix::unistd
```

`crates/dream/src/consolidation_lock.rs` used `nix` for `kill(0)`
PID-alive check. `nix` is Unix-only.

**Fix shipped:** `#[cfg(unix)]` gated the imports; Windows
`is_pid_running()` shells out to
`tasklist /FI "PID eq <pid>" /FO CSV` and matches the captured
pid.

**c) Windows** — cargo-dist artifact lookup:

```
failed to find bin mock_subprocess_plugin.exe for path+file:///D:/a/...
```

cargo-dist tried to package the test fixture bin even though
`tests/fixtures/mock_subprocess_plugin.rs` is marked
`test = false`.

**Fix shipped:** Added `required-features = ["dev-bins"]` on the
`[[bin]]` entry + a new `dev-bins` workspace feature
(off by default). cargo-dist skips bins whose required features
aren't enabled.

## Open blockers (priority order)

### 1. Linux musl still failing in take 5 [HIGH PRIORITY]

`run 25633713871` shows both `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl` failed at 16:29 UTC, ~7 min into
the run. Need to:

```bash
gh -R lordmacu/nexo-rs run view --job <ID> --log-failed | tail -50
```

… once the run completes (it's still in progress at session
pause). Likely culprits:

- The `_DEFAULT_SOURCE` CFLAGS env var not being picked up by
  `cargo-zigbuild` (zigbuild may override CFLAGS instead of
  augmenting). Try `BINDGEN_EXTRA_CLANG_ARGS=-D_DEFAULT_SOURCE`
  as a fallback, or add a `.cargo/config.toml` with
  `[env]` block setting CFLAGS.
- Another C-FFI dep (whisper-rs's vendored whisper.cpp,
  perhaps?) hitting the same musl typedef issue.
- `musl-tools` apt step racing with the cargo step (unlikely
  but possible — verify the step runs BEFORE `dist build`).

**Fix path:**
1. Read the actual error from the take-5 run logs.
2. If it's still sqlite-vec: try the `.cargo/config.toml`
   `[env]` approach or patch sqlite-vec via `[patch.crates-io]`
   to a forked version with the BSD typedefs replaced.
3. If it's a different C dep: same pattern.

### 2. Windows MSVC — unknown if take 5 fixed it [MEDIUM PRIORITY]

`x86_64-pc-windows-msvc` was still `in_progress` at session
pause (Windows is the slowest build, ~15-20 min cold). Once
the run completes:

```bash
gh -R lordmacu/nexo-rs run view --job <WINDOWS_ID> --log-failed
```

If green: **the Windows fixes worked**, take 5 was a Windows
success.

If failed: read the new error. Likely candidates if more
issues remain:
- Another crate using `nix`, `signal-hook`, `caps`, or any
  Unix-only dep without `cfg(unix)` guard.
- A `Path::new("/foo/bar")` or `chmod` somewhere that doesn't
  compile on Windows.
- Linker errors from a C dep that needs Windows-specific build
  flags.

**Fix pattern:** Same as take 4 — find the offending crate,
add `#[cfg(unix)]` or a Windows-specific shim.

### 3. Apple Intel never reaches a runner [LOW PRIORITY]

`x86_64-apple-darwin` queued in every take. Macos-13 runner
pool is congested in `lordmacu/nexo-rs`. Either:

- Wait longer (queue eventually drains) — runs cap at 6h, the
  wait is usually <30 min in off-peak hours.
- Or accept Apple Silicon as the macOS proof + ship Apple
  Intel later. The pipeline is identical, so a green Apple
  Silicon strongly implies Apple Intel will pass.

### 4. Termux re-enable [DOCUMENTED — NOT THIS SESSION]

Tracked under "Phase 27.2 follow-up.b" in `FOLLOWUPS.md`.
Multi-repo: needs `rustls-tls-no-provider` swap in:

- `nexo-rs-plugin-browser` Cargo.toml
- `nexo-rs-plugin-whatsapp` Cargo.toml
- `nexo-rs-plugin-telegram` Cargo.toml
- proyecto `[workspace.dependencies]` reqwest

Plus version bumps + crates.io publish for each plugin repo +
proyecto bumping plugin deps to the new versions. ~half-day of
multi-repo coordination.

## Files touched in this session (proyecto/)

```
M Cargo.toml                                        — version bump 0.1.6→0.1.6-rc1; dev-bins feature; required-features on mock_subprocess_plugin
M Cargo.lock                                        — auto-regen
M Dockerfile                                        — drop admin-ui-builder stage
M FOLLOWUPS.md                                      — Phase 27.2 follow-up.b (Termux aws-lc-rs)
M README.md                                         — install snippet now multi-channel
M dist-workspace.toml                               — 5 targets (added apple-x2, windows)
M docs-site/index.html                              — landing rewrite (OpenClaw-first), logo slot, Docker install
A docs-site/README.md                               — landing edit guide
A docs-site/assets/logo.svg                         — placeholder logo (replace this file to swap branding)
M docs/src/SUMMARY.md                               — added platform-support page
A docs/src/getting-started/platform-support.md      — per-OS prereq matrix + feature support
M docs/src/plugins/telegram.md                      — fixed broken PHASES.md link
M docs/src/plugins/whatsapp.md                      — fixed broken PHASES.md link
M .github/workflows/docker.yml                      — auto-publish enabled, nexo-rs-v* tag pattern
M .github/workflows/release.yml                     — apple/windows jobs, musl-tools, _DEFAULT_SOURCE CFLAGS, build-termux disabled
M crates/driver-loop/src/lib.rs                     — #[cfg(unix)] socket module
M crates/driver-loop/src/orchestrator.rs            — #[cfg(unix)] socket bind, no-op handle on Windows
M crates/dream/src/consolidation_lock.rs            — #[cfg(unix)] nix imports, tasklist fallback on Windows
```

## Resume checklist (for next session)

```bash
# 1. Catch up on take 5's final state
cd /home/familia/chat/proyecto
gh run list -w release.yml -L 1
gh run view <RUN_ID> --json jobs | python3 -c "import json,sys; ..."

# 2. Read failed logs
gh run view --job <FAILING_JOB_ID> --log-failed | tail -80

# 3. Apply fix per "Open blockers" section above

# 4. Push fix, recreate rc tag
git add <files>
git commit -m "fix(release): ..."
git push
gh release delete nexo-rs-v0.1.6-rc1 --yes --cleanup-tag
gh release create nexo-rs-v0.1.6-rc1 --prerelease --target main \
  --title "v0.1.6-rc1 — multi-platform RC (take N)" \
  --notes "..."

# 5. Repeat until all 4 platforms green
#    (Linux musl x2, Apple x2, Windows)

# 6. Tag GA
gh release create nexo-rs-v0.1.6 --target main \
  --title "v0.1.6 — first multi-platform release" \
  --notes "..."
```

## Distribution channels — current state

| Channel | Status | Where |
|---|---|---|
| Curl shell installer | ⏳ Will publish when GA tag fires | Generated by cargo-dist's `installers = ["shell"]` |
| GH Releases tarballs | ⏳ Same | Per-target `.tar.xz` |
| Homebrew tap | ✅ Repo created (placeholder) | `lordmacu/homebrew-nexo-rs` — formula auto-publishes when GA tag fires + macOS builds work |
| npm scope | ✅ Reserved | `@nexo-rs/cli` v0.0.1-placeholder published; real CLI shim pending cargo-dist npm installer enable |
| Docker (ghcr.io) | ✅ Auto-publish enabled | `ghcr.io/lordmacu/nexo-rs:edge` on every main push; `:latest` + `:vX.Y.Z` on GA tag |
| crates.io | ✅ Already published | release-plz handles per-crate publish |

## Out-of-scope reminders

Do NOT in this Phase 27.2 effort:

- Re-enable Termux release (separate cross-repo follow-up)
- Add MSI / PowerShell installers (need Windows green first)
- Add a named-pipe Windows alternative for the
  permission-prompt forwarder (architectural, not a release
  blocker)
- Touch the lifted SDK modules (sanitize, media,
  email_template, module_state, compose_*, db_migrate) —
  those landed earlier today, are working, and should not
  block the release matrix

## Session metadata

- **Session length:** ~6h, multiple context windows
- **Commits to proyecto in this session:** 12 (search log
  for `447b5ca` through `24b8db2`)
- **Other artifacts created out-of-tree:**
  `lordmacu/homebrew-nexo-rs` repo + `@nexo-rs/cli` npm package
- **Tag iterations:** 5 (`nexo-rs-v0.1.6-rc1` deleted +
  recreated each time)
- **Last failed run:** `25633713871` (take 5)

> **Key insight from this session:** the project's binary
> never actually shipped multi-platform end-to-end before.
> Sub-crates released via release-plz, but the daemon `nexo`
> bin's first true multi-platform validation was today. Each
> rc surfaced previously latent platform-specific bugs in
> the workspace dep graph. Once these blockers clear, the
> first GA tag is durable — subsequent releases just need
> cargo-dist to re-run the same matrix.
