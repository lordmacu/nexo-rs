# Publishing the SDK crates

This page documents the publish sequence for the framework's
microapp-author-facing crates. Operators run these in order
when cutting a 0.x release.

## Publishable crates (Tier A)

The framework publishes four crates microapp authors consume:

| Crate | Phase | Depends on |
|---|---|---|
| `nexo-tool-meta` | 82.2.b | (no nexo deps) |
| `nexo-plugin-manifest` | 81 | (no nexo deps) |
| `nexo-compliance-primitives` | 83.5 | (no nexo deps) |
| `nexo-microapp-sdk` | 83.4 | `nexo-tool-meta` |

Other framework crates (`nexo-core`, `nexo-config`,
`nexo-broker`, etc.) are NOT publishable — they are
daemon-internal and microapp authors should not import them.

## Publish order

Because `nexo-microapp-sdk` depends on `nexo-tool-meta`, the
publish sequence is:

```bash
# 1. Standalone crates (any order)
cargo publish -p nexo-tool-meta
cargo publish -p nexo-plugin-manifest
cargo publish -p nexo-compliance-primitives

# 2. Dependent crate (after tool-meta lands on crates.io)
cargo publish -p nexo-microapp-sdk
```

Verify each step with a dry-run first:

```bash
cargo publish -p nexo-tool-meta --dry-run
```

A clean dry-run prints
`Uploading … warning: aborting upload due to dry run` and
exits 0. Anything else (path-dep complaint, missing license,
etc.) aborts before upload.

## Path-dep elision

The workspace's root `Cargo.toml` declares each crate with
both `version = "..."` AND `path = "..."`. Cargo strips the
`path` segment when packaging for crates.io, so the published
artifact contains version-only deps. **Operators do not need
to edit `Cargo.toml` between local dev and publish.**

## Versioning policy

Pre-1.0 (current state):

- Breaking changes ALLOWED with a minor bump (`0.1` → `0.2`).
- Per-crate CHANGELOG.md describes the migration.
- One release of grace before removing a deprecated symbol —
  i.e. release N marks the symbol `#[deprecated]`, release
  N+1 removes it.
- Wire-format changes propagate together: bumping
  `nexo-tool-meta` triggers a coordinated bump on
  `nexo-microapp-sdk`.

Post-1.0 (future):

- Strict semver. Breaking changes require a major bump.
- Wire format changes require coordinated multi-language
  release (Rust SDK + any future Python/TS SDK).

## Out-of-tree microapp migration

After publish, out-of-tree microapps swap their `Cargo.toml`:

```diff
 [dependencies]
-nexo-microapp-sdk        = { path = "../nexo-rs/crates/microapp-sdk" }
-nexo-tool-meta           = { path = "../nexo-rs/crates/tool-meta" }
-nexo-compliance-primitives = { path = "../nexo-rs/crates/compliance-primitives" }
+nexo-microapp-sdk        = "0.1"
+nexo-tool-meta           = "0.1"
+nexo-compliance-primitives = "0.1"
```

A microapp that depends only on published versions can build
without any nexo-rs source on disk — strictly the published
artifact.

## CI integration (deferred)

Auto-publish on tag via release-plz is the operator-side
deliverable per Phase 83.14:

- `.github/workflows/publish.yml` runs on `v*.*.*` tags.
- Reads `CARGO_REGISTRY_TOKEN` from secrets.
- Calls `cargo publish` per crate in the order documented
  above.
- Sequencing: publishes `nexo-tool-meta` first, waits for
  crates.io index propagation (typically <60 s), then
  publishes the rest.

The release-plz integration itself lands when the operator
actually tags v0.1.0; until then the per-crate
`Cargo.toml` + CHANGELOG.md is publish-ready and the dry-run
checks pass.
