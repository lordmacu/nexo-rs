# Plugin trust (cosign + `trusted_keys.toml`)

Phase 31.3. Operators control which plugin authors are trusted by
maintaining `<config_dir>/extensions/trusted_keys.toml`. The
`nexo plugin install` CLI reads this file before extracting any
tarball; cosign verification of `.sig` + `.cert` (+ optional
`.bundle`) assets gates the install.

The framework's own release signing precedent — see
[Verifying releases](../getting-started/verify.md) — uses the
same Sigstore keyless flow. Plugin trust applies that flow per
author, with operator-side allowlisting.

## Trust modes

| Mode | What happens |
|------|--------------|
| `ignore` | Skip cosign verification entirely. Useful for dev / CI / installing a plugin you built locally. |
| `warn` (default) | Verify when `.sig` + `.cert` are present in the release; if absent, log a stderr warning and proceed unverified. |
| `require` | Reject any install whose tarball does not produce a valid allowlisted signature. |

Mode resolution precedence on each install:

1. CLI flag (`--require-signature` / `--skip-signature-verify`).
2. Per-author `[[authors]]` `mode` field, when the install's
   owner matches.
3. Global `default` field.
4. Built-in fallback (`warn`).

Mutually exclusive flags `--require-signature` +
`--skip-signature-verify` fail the install at parse time.

## Sample `trusted_keys.toml`

```toml
schema_version = "1.0"
default = "warn"

# Optional override; falls back to $PATH walk + well-known
# locations (/usr/local/bin/cosign, /opt/homebrew/bin/cosign,
# ~/go/bin/cosign).
# cosign_binary = "/usr/local/bin/cosign"

[[authors]]
owner = "lordmacu"
identity_regexp = "^https://github.com/lordmacu/[^/]+/\\.github/workflows/release\\.yml@.*$"
oidc_issuer = "https://token.actions.githubusercontent.com"
mode = "require"
```

A copy with comments lives at
`config/extensions/trusted_keys.toml.example` in the repo root.

## How `identity_regexp` is matched

Every cosign keyless signature carries a Subject Alternative
Name (SAN) on its certificate. In GitHub Actions flow the SAN
encodes the workflow URL plus the ref:

```
https://github.com/<owner>/<repo>/.github/workflows/release.yml@refs/tags/v0.2.0
```

The operator regex must match that string. Make it specific
enough to lock in the workflow path but loose enough to tolerate
ref / repo additions. Examples:

| Goal | Regex |
|------|-------|
| Trust everything from this owner via `release.yml` | `^https://github\.com/lordmacu/[^/]+/\.github/workflows/release\.yml@.*$` |
| Trust a specific repo only | `^https://github\.com/lordmacu/nexo-plugin-slack/\.github/workflows/release\.yml@.*$` |
| Trust any owner-prefix workflow path | `^https://github\.com/lordmacu/.*$` |

## Required prerequisite: `cosign` on the host

The verifier shells out to `cosign verify-blob`. Install before
using any non-`ignore` trust mode:

```bash
brew install cosign           # macOS
sudo apt install cosign       # Debian/Ubuntu
sudo dnf install cosign       # Fedora/RHEL
```

The framework pins to **cosign 2.4.1** (matching its own
release-signing workflow). Any ≥ 2.4 should work; older versions
predate the keyless argv shape used here.

## CLI flags

```bash
# Use the trusted_keys.toml default for this install:
nexo plugin install lordmacu/nexo-plugin-slack@v0.2.0

# Force `Require` for this call regardless of config:
nexo plugin install lordmacu/nexo-plugin-slack@v0.2.0 --require-signature

# Force `Ignore` (skip verification) for this call:
nexo plugin install lordmacu/nexo-plugin-slack@v0.2.0 --skip-signature-verify
```

## JSON output additions

Every install report (`--json`) now includes:

| Field | Value |
|-------|-------|
| `signature_verified` | `true` when cosign verification succeeded. |
| `signature_identity` | SAN string parsed from cosign output (`Subject:` line). Omitted when verification was skipped. |
| `signature_issuer` | OIDC issuer the cert was minted by. |
| `trust_mode` | `"ignore"` / `"warn"` / `"require"` — the effective mode used. |
| `trust_policy_matched` | Repo owner that matched a `[[authors]]` entry, or omitted. |

The error report (`PluginInstallErrorReport`) gains five new
`kind` values: `CosignNotFound`, `CosignFailed`, `VerifyIo`,
`PolicyRequiresSig`, `AssetIncomplete`, `TrustedKeysParse`,
`IdentityRegexpInvalid`. Plus the parse-time conflict
`FlagsConflict` (mutually-exclusive flags).

## Troubleshooting

- **`cosign binary not found`** — install cosign. Or set
  `cosign_binary` in your trust file. Or pass
  `--skip-signature-verify` for a one-off install of trusted
  bytes you already vetted.
- **`trust policy requires signature for <owner>`** — your
  `mode = "require"` rejected an unsigned plugin. Ask the
  author to enable `COSIGN_ENABLED=true` on their publish
  workflow (see
  [Publishing a plugin](../plugins/publishing.md)), or relax
  the per-author `mode` to `warn`.
- **`cosign verify-blob exited non-zero`** — the cert SAN did
  not match your `identity_regexp`. Check the publisher's
  workflow URL (it appears in their release's actions log) and
  update the regex. Capture the full cosign stderr from the
  error message for the exact mismatch.
- **`identity_regexp ... invalid`** — your regex did not
  compile. Common cause: forgetting to escape `.` or `/`. The
  Rust `regex` crate's syntax docs are
  [here](https://docs.rs/regex/latest/regex/#syntax).

## See also

- [Publishing a plugin](../plugins/publishing.md) — author side
  of the cosign signing chain.
- [Verifying releases](../getting-started/verify.md) — same
  Sigstore flow, applied to the framework's own release
  artifacts.
