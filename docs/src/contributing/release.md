# Releases

Two complementary tools own the release pipeline:

| Tool | Owns |
| ---- | ---- |
| [`release-plz`](https://release-plz.ieni.dev/) | version bumps, git tags, `crates.io` publish, per-crate `CHANGELOG.md` |
| [`cargo-dist`](https://opensource.axo.dev/cargo-dist/) | cross-target binary tarballs, `curl \| sh` / PowerShell installers, sha256 sidecars |

They run on the same tag (`nexo-rs-v<version>`) and stay independent
— no overlapping config. Phase 27 brings both online; Phase 27.2
wires the GitHub Actions workflow that combines them on tag push.

## What ships

The `nexo` binary is the only artifact in release tarballs. Every
other binary in the workspace (driver subsystem, dispatch tools,
companion-tui, mock MCP server) carries
`[package.metadata.dist] dist = false` so cargo-dist excludes it.
Dev / smoke programs (`browser-test`, `integration-browser-check`,
`llm_smoke`) live as `[[example]]` entries under `examples/` for
the same reason.

## Build provenance — `nexo version`

`build.rs` injects four stamps captured at compile time:

- `NEXO_BUILD_GIT_SHA` — short git SHA of the build commit (or
  `unknown` outside a git checkout)
- `NEXO_BUILD_TARGET_TRIPLE` — full Rust target triple
- `NEXO_BUILD_CHANNEL` — opaque channel marker; defaults to
  `source`. The release workflow overrides via
  `NEXO_BUILD_CHANNEL=apt-musl` (etc.) so support tickets carry
  install-channel provenance.
- `NEXO_BUILD_TIMESTAMP` — UTC ISO8601 timestamp of the build

Operators see them with:

```bash
nexo version
# nexo 0.1.1
#   git-sha:   abc1234
#   target:    x86_64-unknown-linux-musl
#   channel:   apt-musl
#   built-at:  2026-04-27T12:34:56Z
```

`nexo --version` (without `--verbose` or the subcommand) prints the
short form `nexo <version>`.

## Local validation

```bash
make dist-check
```

Builds the host-target tarball via `dist build --target $(rustc -vV
| sed -n 's|host: ||p')` and runs
[`scripts/release-check.sh`](https://github.com/lordmacu/nexo-rs/blob/main/scripts/release-check.sh).
The smoke gate verifies every present tarball contains the bin +
`LICENSE-*` + `README.md` and that the host-native `--version`
output matches the workspace version. Targets the local toolchain
can't satisfy emit `[release-check] WARN` lines instead of failing.

Full setup notes (cargo-dist, cargo-zigbuild, zig, rustup targets):
[`packaging/README.md`](https://github.com/lordmacu/nexo-rs/blob/main/packaging/README.md).

## What's automatic vs manual

| Step | Today (Phase 27.1) | After Phase 27.2 |
| ---- | ------------------ | ---------------- |
| Bump version + open release PR | `release-plz` (CI on push to main) | unchanged |
| Tag commit + `crates.io` publish | `release-plz` (on PR merge) | unchanged |
| Build cross-target tarballs | local `make dist-check` only | GH Actions workflow on tag |
| Upload tarballs to GH release | n/a | GH Actions |
| Sign tarballs (cosign keyless) | n/a | Phase 27.3 workflow |
| Generate SBOMs | n/a | Phase 27.9 workflow |

## Adding a new bin to the release

1. Declare the `[[bin]]` in the appropriate crate's `Cargo.toml`.
2. If the crate hosts the bin via `[package.metadata.dist] dist = false`,
   either remove that opt-out or move the bin to a new crate that
   doesn't carry it.
3. Re-run `make dist-check` and confirm the new bin shows up under
   `[bin]` in the dist plan output.
4. Update [`scripts/release-check.sh`](https://github.com/lordmacu/nexo-rs/blob/main/scripts/release-check.sh)'s
   per-archive content check if the new bin should be required.

## Adding a new target

1. Append the target triple to `targets = […]` in
   [`dist-workspace.toml`](https://github.com/lordmacu/nexo-rs/blob/main/dist-workspace.toml).
2. Append the matching tarball name to `EXPECTED_TARBALLS` in the
   smoke gate.
3. Land the toolchain story in the GH Actions release workflow
   (Phase 27.2) — without that, the target builds locally only.
