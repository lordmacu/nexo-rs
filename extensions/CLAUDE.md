# Extensions — Project Guide

Each subdirectory is an **independent Rust workspace** (its own
`Cargo.toml`, its own `target/`). Touch them as needed when the
agent framework asks for new tools, channels, plugins, or
microapps.

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

## `[profile.release-fast]` is in every extension's `Cargo.toml`

Same opt-level as `release` but `lto = false`, `codegen-units = 16`.
~50% faster build, ~5% runtime cost. Reserve plain `--release` for
publish. If you add a brand-new extension, append the same block —
or run `bash proyecto/scripts/add-release-fast.sh` (idempotent;
skips extensions that already have it).

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
