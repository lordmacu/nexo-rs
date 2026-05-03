# Publishing a plugin (CI workflow)

Phase 31.2. Operators install plugins via:

```bash
nexo plugin install <owner>/<repo>[@<tag>]
```

The CLI hits the GitHub Releases API of `<owner>/<repo>` and
expects a fixed asset naming convention. This page documents the
convention so plugin authors can publish releases that the
operator-side install path consumes without translation.

The reference Rust plugin template
[`extensions/template-plugin-rust/`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-rust)
ships a drop-in workflow plus helper scripts. Copy them to your
own plugin repo and you are done.

## Asset naming convention

For every release tag `v<semver>` (e.g. `v0.2.0`) the workflow
uploads the following assets to the GitHub Release:

| Asset | Required | Contents |
|-------|----------|----------|
| `nexo-plugin.toml` | ✅ | The plugin manifest. Operator's CLI fetches first to learn `plugin.id`. |
| `<id>-<version>-<target>.tar.gz` | ✅ | One per supported target. Layout: `bin/<id>` + `nexo-plugin.toml` at the root, no top-level wrapping dir. Binary mode `0755` on Unix. |
| `<id>-<version>-<target>.tar.gz.sha256` | ✅ | Single line of lowercase hex (64 chars). |
| `<id>-<version>-<target>.tar.gz.sig` | ⬜ | Cosign keyless signature blob. |
| `<id>-<version>-<target>.tar.gz.pem` | ⬜ | Cosign certificate. |
| `<id>-<version>-<target>.tar.gz.bundle` | ⬜ | Cosign Sigstore bundle. |

Targets follow Rust's standard target triple notation
(`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, etc.).

## Publish workflow shape

The shipped workflow has four jobs:

1. **`validate-tag`** — checks tag format
   `^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$`, asserts the tag
   matches the `version` declared in `nexo-plugin.toml`. Hard
   fails on mismatch (no partial release).
2. **`build`** — matrix over targets. For each:
   - `cargo zigbuild --release --target <target>` for linux musl
     entries (cross-compiled from `ubuntu-latest`).
   - `cargo build --release --target <target>` for darwin entries
     (run on `macos-latest`).
   - `bash scripts/pack-tarball.sh <target>` produces the tarball
     + sha256 sidecar following the convention above.
3. **`sign`** (optional, gated on repo variable
   `COSIGN_ENABLED == "true"`) — keyless cosign signs each
   tarball using the workflow's OIDC token, producing
   `.sig` / `.pem` / `.bundle` per asset.
4. **`release`** — creates the GitHub Release if missing,
   uploads all artifacts including `nexo-plugin.toml`. Uses
   `--clobber` so re-runs of the same tag overwrite stale assets.

## Required permissions

```yaml
permissions:
  contents: write   # gh release upload
  id-token: write   # cosign keyless OIDC
```

`GITHUB_TOKEN` is auto-provided. No additional secrets required
for the unsigned path. Cosign keyless does not need any secret
either — it uses Sigstore/Fulcio with the workflow's OIDC token.

## Enabling cosign signing

```bash
gh variable set COSIGN_ENABLED --body true
```

After signing is enabled, every tag push produces signing
material that operators with `config/extensions/trusted_keys.toml`
(Phase 31.3) can verify against your GitHub identity.

## Constraint: cargo bin name = plugin id

Cargo's `[[bin]] name` MUST equal `nexo-plugin.toml` `[plugin] id`.
The convention is `bin/<id>` inside the tarball, and
`pack-tarball.sh` looks for the binary at
`target/<target>/release/<id>`. Mismatch fails the pack step
(`built binary missing at target/...`).

## Local validation

Before pushing a tag, dry-run the pack step:

```bash
cargo build --release --target x86_64-unknown-linux-gnu
bash scripts/pack-tarball.sh x86_64-unknown-linux-gnu
ls dist/
# my_plugin-0.2.0-x86_64-unknown-linux-gnu.tar.gz
# my_plugin-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The Rust integration test `tests/pack_tarball.rs` covers this
end to end against a synthetic binary; copy it when you fork the
template to keep the convention regression-tested.

## Troubleshooting

- **`tag 'X' does not match v<semver>`** — the workflow rejects
  any tag that does not start with `v` and parse as semver.
  Examples: `v0.2.0`, `v1.0.0-beta.3`. Reject: `0.2.0` (missing
  `v`), `v0.2`, `v01.0.0` (leading zero).
- **`nexo-plugin.toml version <X> != tag <Y>`** — the workflow
  enforces that the tag and the manifest version match. Update
  one before retagging.
- **`built binary missing at target/...`** — `cargo` produced a
  binary at a path other than what `pack-tarball.sh` expected.
  Check `[[bin]] name` in `Cargo.toml` matches `[plugin] id` in
  `nexo-plugin.toml`.
- **Operator hits `TargetNotFound`** — your matrix did not build
  for the operator's target triple. Re-enable the matrix entry
  and re-run; operator can also pass `--target` to override.

## See also

- [Plugin contract (out-of-tree)](./contract.md) — the wire
  format the binary speaks once the operator runs it.
- [`crates/ext-installer/README.md`](https://github.com/lordmacu/nexo-rs/tree/main/crates/ext-installer)
  — operator-side install pipeline that consumes these assets.
