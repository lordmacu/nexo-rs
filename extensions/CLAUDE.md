# Extensions — Project Guide

Two kinds of subdirectories live here:

- **Workspace members** — currently `template-mcp-server`,
  `template-microapp-rust`, `template-plugin-rust`,
  `sample-channel-server`. Listed under `[workspace].members` in
  the proyecto root `Cargo.toml`; they share `target/` + lockfile
  + profiles with the daemon. They MUST NOT define their own
  `[profile.*]` blocks — Cargo ignores them and emits the
  "profiles for the non root package will be ignored" warning at
  every build. Profiles live exclusively in the workspace root.
- **Independent workspaces** — every other subdirectory
  (`template-rust`, `brave-search`, `cloudflare`, the language
  SDKs, etc.). Each has its own `Cargo.toml` + `target/` and is
  excluded from the proyecto workspace. These DO define their
  own profiles, including `[profile.release-fast]`.

Root architecture + retry policy:
[`/home/familia/chat/CLAUDE.md`](../../CLAUDE.md). Phase tracker:
[`../CLAUDE.md`](../CLAUDE.md).

## Commands (any extension)

```bash
cd <extension-dir>
cargo build                          # dev (mold + sccache active globally)
cargo build --profile release-fast   # release-grade, no LTO — ~50% faster
cargo build --release                # publish/dist binary
cargo nextest run                    # parallel tests
```

## Build toolchain (machine-wide, in `~/.cargo/config.toml`)

Every Rust workspace on this box shares:

- **Linker:** `mold` via `clang` — every link step is ~5–10× faster
  than GNU ld.
- **Compile cache:** `sccache` as `rustc-wrapper` — caches rustc
  invocations across every workspace. Inspect with
  `sccache --show-stats`; reset with `sccache --zero-stats`. Big
  win here: shared deps (tokio, serde, reqwest, …) compile once
  and hit on every other extension.
- **Dev profile:** `debug = "line-tables-only"` globally — keeps
  file:line in panics, smaller `target/`, faster IO.

## `[profile.release-fast]` policy

- **Independent extensions** ship their own
  `[profile.release-fast]` block: `inherits = "release"`,
  `lto = false`, `codegen-units = 16`. ~50 % faster build for ~5 %
  runtime cost; reserve plain `--release` for publish.
  `bash proyecto/scripts/add-release-fast.sh` appends the block
  idempotently and skips extensions that already have it.
- **Workspace-member extensions** inherit `release-fast` from the
  proyecto root `Cargo.toml` automatically. Adding a `[profile.*]`
  block in a member's `Cargo.toml` is a no-op + warning — strip
  it instead.

If `cargo build --workspace` emits "profiles for the non root
package will be ignored", that is the bug — go strip the offending
member's profile block.

## Rules when modifying these crates

- Never bump a dep without checking the agent framework's lockfile
  first — a mismatch silently re-builds half the workspace.
- Don't touch `target/` paths in scripts; sccache + mold assume
  the cargo defaults.
- Code identifiers + comments + repo Markdown in **English**.
- Conversations in **Spanish**; code artifacts always English.

## What NOT to do

- Don't disable `sccache` per-extension via `RUSTC_WRAPPER=`.
  If something is non-cacheable, fix the cause; don't blanket-disable.
- Don't add `[profile.release]` overrides that contradict the
  parent workspace — keep customization in `release-fast`.
