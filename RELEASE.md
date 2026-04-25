# Release process

Workspace publishes to crates.io as a set of cooperating crates. Order
matters: each crate must already be on the registry before crates that
depend on it are published.

## Quick reference

```bash
# Print the current topological order:
scripts/publish-order.sh

# Verify each crate packages cleanly (does not upload):
scripts/publish-order.sh --dry-run

# Real publish (requires `cargo login`; sleeps 30s between crates so
# the crates.io index has time to propagate):
scripts/publish-order.sh --publish
```

The script ignores `[dev-dependencies]` when computing order — those
are stripped on publish if path-only.

## Current layer order

Generated from runtime deps in each `crates/*/Cargo.toml`:

```
L1: nexo-config nexo-pairing nexo-plugin-email nexo-resilience nexo-taskflow nexo-tunnel
L2: nexo-auth nexo-broker nexo-llm nexo-web-search
L3: nexo-extensions nexo-memory
L4: nexo-mcp
L5: nexo-core
L6: nexo-plugin-browser nexo-plugin-google nexo-plugin-telegram nexo-plugin-whatsapp
L7: nexo-poller nexo-setup
L8: nexo-poller-ext nexo-poller-tools
```

The top-level `nexo-rs` bin (workspace root) is published last, after
every internal dep. Run `cargo publish` from the repo root for that one.

## Bumping versions

Workspace pins one version in `[workspace.package].version` in the root
`Cargo.toml`. Path deps in each `crates/*/Cargo.toml` carry a hard-coded
`version = "X.Y.Z"` next to the `path = "..."` — bump those in lockstep
or `cargo publish` rejects the upload.

`scripts/publish-order.sh` does not bump versions. Use a sed sweep:

```bash
old="0.1.1"; new="0.1.2"
sed -i "s/version *= *\"$old\"/version = \"$new\"/" Cargo.toml \
    crates/*/Cargo.toml crates/plugins/*/Cargo.toml
```

Confirm with `cargo check --workspace` before publishing.

## Pre-publish checklist

- [ ] `cargo check --workspace` clean
- [ ] `cargo test --workspace` green
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `mdbook build docs` clean (no broken links)
- [ ] `CHANGELOG.md` entry for the new version
- [ ] Git tree clean, tag `vX.Y.Z` created and signed
- [ ] Phase 27 release-pipeline sub-phases that apply (cosign, SBOM,
      etc.) executed — see `proyecto/PHASES.md` Phase 27

## Known constraints

- `nexo-mcp` ↔ `nexo-extensions` looks circular but is not: extensions
  pulls mcp only via `[dev-dependencies]`. The publish-order script
  strips dev-deps so the cycle does not appear in L3/L4.
- `cargo publish --dry-run` resolves deps against the live crates.io
  index, so a crate in layer N+ cannot dry-run cleanly until layer N
  is actually published. Use the script's `list` mode to validate
  ordering, and `--dry-run` only on layers whose deps already exist
  on crates.io.
