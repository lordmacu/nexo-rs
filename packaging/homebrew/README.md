# Homebrew tap

Source of truth for the Homebrew formula. Lives here so it stays in
lockstep with the workspace; gets mirrored to the tap repo
(`lordmacu/homebrew-nexo`) by the release workflow.

## Install one-liner (for users)

```bash
brew tap lordmacu/nexo
brew install nexo-rs
```

Or in one shot (auto-taps):

```bash
brew install lordmacu/nexo/nexo-rs
```

## Files

- `nexo-rs.rb` — the formula. Builds from source via `cargo install`.

## Updating the tap on each release

The release workflow (Phase 27.2) handles this. On every `v*` tag:

1. Build the source tarball + compute its sha256.
2. Open a PR against `lordmacu/homebrew-nexo` that rewrites three
   lines in `Formula/nexo-rs.rb`:
   ```ruby
   url     "https://github.com/lordmacu/nexo-rs/archive/refs/tags/vX.Y.Z.tar.gz"
   version "X.Y.Z"
   sha256  "<new-sha256>"
   ```
3. Run `brew test-bot` against the formula to verify it still
   installs cleanly on macOS.
4. Auto-merge if green.

The user-facing experience: `brew upgrade nexo-rs` pulls the new
version within minutes of the GitHub release going public.

## Local testing

```bash
brew install --build-from-source ./packaging/homebrew/nexo-rs.rb
brew test  ./packaging/homebrew/nexo-rs.rb
brew audit ./packaging/homebrew/nexo-rs.rb --strict --online
```

`brew audit --strict` catches the most common formula mistakes
(dead URLs, missing license, sha256 placeholder, etc.). Run it
before committing any change to the .rb.

## Bottles (deferred)

Today `brew install nexo-rs` compiles from source on the user's
Mac (~2-3 min on M-series, longer on Intel). A follow-up adds
**bottles** — pre-built binaries for:

- `arm64_sequoia` (macOS 15+, Apple silicon)
- `arm64_sonoma`  (macOS 14, Apple silicon)
- `arm64_ventura` (macOS 13, Apple silicon)
- `monterey`      (macOS 12, Intel — last x86 macOS we support)

`brew install nexo-rs` then becomes a sub-30s download. Bottles
need:
1. A macOS CI runner (Ruby builds via `brew test-bot`).
2. The release workflow uploads each `*.bottle.tar.gz` to the
   GitHub release.
3. The formula gains a `bottle do … end` block listing the sha256
   per arch.

Tracked under Phase 27.6 follow-ups — wait for the source-build
formula to be in steady state first.

## Why `head` is set

The `head` URL points at `main`. Adventurous users can run:

```bash
brew install --HEAD nexo-rs
```

to get the very latest commit, useful for testing release
candidates before they're tagged.
