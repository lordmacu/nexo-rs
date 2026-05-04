# Signing & publishing your plugin

Phase 31.9. End-to-end tutorial: take a freshly scaffolded
plugin from `nexo plugin new`, ship it as a public GitHub
release that operators can install signed, and confirm an
operator with `--require-signature` accepts it.

This page is the **how-to**. For reference material:

- [Publishing a plugin](./publishing.md) — asset naming
  convention + workflow-job shape.
- [Plugin trust (`trusted_keys.toml`)](../ops/plugin-trust.md) —
  operator-side verification policy and troubleshooting.

## Read this when

- You finished a plugin and want to publish your first release.
- You want operators on `--require-signature` to trust your
  releases via cosign keyless signing.
- You want a concrete checklist before tagging `v0.1.0`.

## Prerequisites

- A GitHub repo containing the plugin scaffolded by
  `nexo plugin new <id> --lang <lang>`. Repo must use the
  shipped `.github/workflows/release.yml` from the matching
  `extensions/template-plugin-<lang>/` template (the scaffolder
  copies it for you).
- `gh` CLI authenticated against the repo
  (`gh auth status`).
- `git` configured to push tags to `origin`.
- (Optional, for signing) `cosign` is **not** required on your
  host — keyless cosign runs inside GitHub Actions using the
  workflow's OIDC token.

## 1. Publish your first release (unsigned)

The shortest path. Tag, push, watch CI.

```bash
# Pick a semver tag matching plugin.version in nexo-plugin.toml.
# The validate-tag job will reject any mismatch.
git tag v0.1.0
git push origin v0.1.0
```

The shipped workflow runs three jobs by default
(validate-tag → build → release; sign is gated and stays
inactive until you opt in):

```bash
gh run watch                # tail the latest run
gh release view v0.1.0      # confirm assets uploaded
```

Expected assets per `<target>`:

```
nexo-plugin.toml
my_plugin-0.1.0-x86_64-unknown-linux-gnu.tar.gz
my_plugin-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

Operators can already install at this point with default trust
mode (`warn`):

```bash
nexo plugin install your-handle/my_plugin@v0.1.0
```

The CLI prints `! No signature in release; trust mode is
'warn' — proceeding unverified.` and extracts the plugin.

## 2. Add cosign keyless signing

Cosign keyless does not need any secret on your end — it uses
Sigstore + Fulcio with the GitHub Actions OIDC token. Enable
it with one command:

```bash
gh variable set COSIGN_ENABLED --body true
```

Re-tag (or move the existing tag) and re-run the workflow:

```bash
git tag -d v0.1.0
git tag v0.1.0
git push --force origin v0.1.0
```

The `sign` job now runs and produces three extra assets per
tarball:

```
my_plugin-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sig
my_plugin-0.1.0-x86_64-unknown-linux-gnu.tar.gz.pem
my_plugin-0.1.0-x86_64-unknown-linux-gnu.tar.gz.bundle
```

The certificate's Subject Alternative Name (SAN) encodes the
workflow URL plus the ref:

```
https://github.com/your-handle/my_plugin/.github/workflows/release.yml@refs/tags/v0.1.0
```

Operators with `--require-signature` will allowlist this SAN
shape via a regex — that's what step 3 is about.

## 3. Operator-side trust setup

Operators who want to enforce signatures add an `[[authors]]`
entry to `<config_dir>/extensions/trusted_keys.toml`:

```toml
schema_version = "1.0"
default = "warn"

[[authors]]
owner = "your-handle"
identity_regexp = "^https://github\\.com/your-handle/[^/]+/\\.github/workflows/release\\.yml@.*$"
oidc_issuer = "https://token.actions.githubusercontent.com"
mode = "require"
```

Notes for the operator (link this paragraph from your plugin's
README):

- `owner` matches the `<owner>` segment of `nexo plugin
  install <owner>/<repo>` invocations.
- `identity_regexp` should be **specific to your owner** and
  **loose on tag** so it survives release-tag bumps. The
  example above accepts every repo under `your-handle/` that
  ships `release.yml` from its default workflow path.
- Anchored `^…$` is intentional — leaving anchors off makes
  the regex match substrings of unrelated SANs.

The full sample with comments lives at
`config/extensions/trusted_keys.toml.example` in the nexo-rs
repo.

## 4. Verify the round trip

On a host with `cosign` installed, an operator runs:

```bash
nexo plugin install your-handle/my_plugin@v0.1.0 --require-signature
```

Expected human output:

```
→ Resolving your-handle/my_plugin@v0.1.0 (target: x86_64-unknown-linux-gnu)
✓ Found release v0.1.0 (x86_64-unknown-linux-gnu, 4.1 MB, sha256 ab12cd34ef56…)
→ Downloading
✓ sha256 verified
→ Verifying signature against trusted_keys.toml
✓ Signature verified (identity: https://github.com/your-handle/my_plugin/.github/workflows/release.yml@refs/tags/v0.1.0)
→ Extracting to /var/lib/nexo/plugins
✓ Plugin installed at /var/lib/nexo/plugins/my_plugin-0.1.0
✓ Lifecycle event emitted (broker)
```

JSON output (`--json`) carries the full report including
`signature_verified`, `signature_identity`, `signature_issuer`,
`trust_mode`, and `trust_policy_matched`:

```bash
nexo plugin install your-handle/my_plugin@v0.1.0 --require-signature --json
```

```json
{
  "ok": true,
  "id": "my_plugin",
  "version": "0.1.0",
  "target": "x86_64-unknown-linux-gnu",
  "plugin_dir": "/var/lib/nexo/plugins/my_plugin-0.1.0",
  "binary_path": "/var/lib/nexo/plugins/my_plugin-0.1.0/bin/my_plugin",
  "sha256": "ab12cd34ef56...",
  "size_bytes": 4194304,
  "was_already_present": false,
  "lifecycle_event_emitted": true,
  "signature_verified": true,
  "signature_identity": "https://github.com/your-handle/my_plugin/.github/workflows/release.yml@refs/tags/v0.1.0",
  "signature_issuer": "https://token.actions.githubusercontent.com",
  "trust_mode": "require",
  "trust_policy_matched": "your-handle"
}
```

## 5. Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `CosignNotFound` | Operator host lacks `cosign` binary. | Install via `brew install cosign`, `apt install cosign`, or download from <https://github.com/sigstore/cosign/releases>. |
| `PolicyRequiresSig` | Trust mode is `require` but release has no `.sig` / `.cert`. | Re-run the workflow after `gh variable set COSIGN_ENABLED --body true`. |
| `CosignFailed` | Cert SAN does not match `identity_regexp`. | Compare the SAN reported in the error against the regex. Common cause: regex too tight on tag (`v0\.1\.0` instead of `.*`). |
| `Sha256Mismatch` | Tarball corrupted in transit or rebuilt out-of-band. | Re-tag and re-run; uploads are reproducible from the same commit. |
| `TargetNotFound` | Operator's host triple has no matching tarball. | Add the missing entry to the `build` matrix in `release.yml` and re-tag. |

For full operator-side troubleshooting (cosign discovery
fallbacks, identity_regexp examples, manual `cosign
verify-blob` invocation), see
[Plugin trust](../ops/plugin-trust.md).

## See also

- [Plugin authoring overview](./authoring.md) — picks a
  language and gets you to a running plugin in 5 minutes.
- [Publishing a plugin](./publishing.md) — asset naming
  reference and the 4-job CI workflow anatomy.
- [Plugin trust (`trusted_keys.toml`)](../ops/plugin-trust.md)
  — operator-side cosign verification policy.
- [Plugin contract](./contract.md) — wire format the binary
  speaks once installed.
- [Verifying releases](../getting-started/verify.md) — same
  Sigstore keyless flow used for nexo-rs's own release
  signing.
