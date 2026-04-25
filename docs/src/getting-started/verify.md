# Verifying releases

Every Nexo release artifact is signed with [Sigstore Cosign](https://docs.sigstore.dev/)
using **keyless OIDC** — no long-lived private key, no PGP key
management, no out-of-band trust establishment. The signature is
tied to the GitHub Actions workflow run that produced the artifact,
and a public record lives in the [Rekor transparency log](https://search.sigstore.dev/).

## Why keyless

Traditional signing requires a long-lived signing key. If it leaks,
every past release becomes suspect. Keyless signing instead anchors
each signature to:

1. The **GitHub Actions OIDC identity** of the workflow run
   (`https://token.actions.githubusercontent.com`)
2. The **specific repo + workflow file** that ran
   (`https://github.com/lordmacu/nexo-rs/.github/workflows/...`)
3. The **commit + ref** the workflow built from

A short-lived certificate (10 min validity) is issued by Sigstore's
`fulcio` CA, the artifact is signed with it, and the whole bundle
is recorded in `rekor` (immutable). To forge a signature, an
attacker would need to compromise GitHub's OIDC infra **and** the
exact workflow path — and even then the forgery shows up in the
public log.

## Install Cosign

```bash
# macOS:
brew install cosign

# Linux (Debian/Ubuntu):
curl -L "https://github.com/sigstore/cosign/releases/latest/download/cosign-linux-amd64" \
  -o /usr/local/bin/cosign
chmod +x /usr/local/bin/cosign

# Linux (Fedora/RHEL):
sudo dnf install cosign

# Verify the install:
cosign version
```

## Verify a Docker image

Every image at `ghcr.io/lordmacu/nexo-rs` is cosign-signed by the
`docker.yml` workflow. Verify any tag with:

```bash
cosign verify ghcr.io/lordmacu/nexo-rs:latest \
  --certificate-identity-regexp 'https://github.com/lordmacu/nexo-rs/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

A successful verification prints the full certificate + the Rekor
entry URL. Anything else (signature missing, identity mismatch,
broken cert chain) means **don't trust this image** — check the
release notes, file an issue.

## Verify a downloaded binary / .deb / .rpm / .tar.gz

The `sign-artifacts.yml` workflow attaches three files next to
every release asset:

- `<asset>.sig` — the raw signature
- `<asset>.pem` — the leaf certificate
- `<asset>.bundle` — combined Sigstore bundle (preferred; carries
  the inclusion proof)

Verify with the bundle (recommended, single command):

```bash
cosign verify-blob \
  --bundle nexo-rs_0.1.1_amd64.deb.bundle \
  --certificate-identity-regexp 'https://github.com/lordmacu/nexo-rs/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  nexo-rs_0.1.1_amd64.deb
```

Or with the standalone `.sig` + `.pem` if you prefer:

```bash
cosign verify-blob \
  --signature nexo-rs_0.1.1_amd64.deb.sig \
  --certificate nexo-rs_0.1.1_amd64.deb.pem \
  --certificate-identity-regexp 'https://github.com/lordmacu/nexo-rs/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  nexo-rs_0.1.1_amd64.deb
```

## Verify in CI / scripted contexts

Drop this in a deploy pipeline:

```bash
#!/usr/bin/env bash
set -euo pipefail

ASSET="${1:?usage: $0 <asset-path>}"
BUNDLE="${ASSET}.bundle"

if [ ! -f "$BUNDLE" ]; then
    echo "ERROR: $BUNDLE missing — refusing to deploy unsigned artifact" >&2
    exit 1
fi

cosign verify-blob \
  --bundle "$BUNDLE" \
  --certificate-identity-regexp 'https://github.com/lordmacu/nexo-rs/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "$ASSET" \
  || { echo "ERROR: signature verification failed for $ASSET" >&2; exit 2; }
```

## Inspecting the transparency log

Every signature is searchable on [Rekor](https://search.sigstore.dev/):

```bash
# Search by artifact sha256:
cosign tree ghcr.io/lordmacu/nexo-rs:latest
```

The output shows every cosign-related artifact attached to the
image (signatures, attestations, SBOMs) plus the Rekor log index
where each was recorded.

## What if verification fails

1. **Identity regex doesn't match** — the asset may have been
   built from a fork / unofficial workflow. Re-download from the
   GitHub release page directly.
2. **`bundle` file missing** — older releases (pre-Phase 27.3)
   don't have signatures. Tag `v0.1.1` is the first signed
   release.
3. **Cert chain expired / revoked** — Sigstore's `fulcio` root CA
   has a long lifespan, but the leaf cert is short-lived.
   `cosign` automatically fetches the right TUF root; if you see
   chain errors run `cosign initialize` to refresh local trust
   roots.
4. **Network errors talking to Rekor / Fulcio** — both have CDN
   in front. Retry, or use `--insecure-ignore-tlog` for local
   verification (drops the transparency log check — only safe in
   air-gapped trust contexts).

## Out of scope (for now)

- Long-lived PGP keys for the apt / yum repos — needs Phase 27.4
  signed-repo work to consume them on the user side. Until that
  ships, .deb / .rpm signatures live in the Cosign world only.
- A Homebrew bottle-signing path that lets `brew` validate without
  the OIDC chain — Phase 27.6 follow-up.
