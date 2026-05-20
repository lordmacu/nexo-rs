# nexo-plugin-index

Curated public index of nexo-rs plugins. The daemon's plugin
discovery layer (Phase 98) fetches `index.json` from this repo to
surface a "Trusted catalogue" tier alongside crates.io + GitHub
topic search.

## How discovery uses this repo

The daemon (via `nexo-plugin-discovery::CuratedIndexSource`) GETs
the **raw** `index.json` from `main`:

```
https://raw.githubusercontent.com/lordmacu/nexo-plugin-index/main/index.json
```

Each entry promotes its plugin to `TrustTier::CommunityIndexed` (or
`Official` when the `owner` also appears in the daemon's
`trusted_keys.toml` allowlist). The catalogue then renders the
plugin with an "indexed" source badge on `/m/plugins` and in
`agent plugin search`.

## Schema

```json
{
  "schema_version": 1,
  "updated_at": "ISO 8601 UTC timestamp",
  "plugins": [
    {
      "name": "<crate name on crates.io>",
      "owner": "<github org / crates.io login>",
      "repo": "<org>/<repo>",
      "manifest_url": "<raw URL to nexo-plugin.toml>",
      "category": "channel | poller | tool | webhook | persona",
      "tags": ["<short keyword>"],
      "description": "<one-line plugin description>",
      "icon_url": "<optional URL>"
    }
  ]
}
```

`schema_version` is bumped MAJOR-style — daemons that don't
recognise the value MUST refuse to consume the index (the
discovery client emits a `SourceError` rather than risk silently
dropping fields). Add fields in a backwards-compatible way
(optional, `#[serde(default)]`) so the same `schema_version = 1`
keeps working across daemon versions.

## How to add a plugin

1. Publish your plugin to crates.io (e.g.
   `cargo publish -p nexo-plugin-my-thing`).
2. Push a release with the daemon-compatible binary artefact
   (cargo-dist convention: tag `vX.Y.Z`, asset
   `<crate>-<target>.tar.gz`).
3. Open a PR on this repo with a new entry in `index.json`.
4. CI verifies the manifest URL resolves (200) + parses cleanly
   via `nexo-plugin-manifest`.
5. Maintainer merges after reviewing.

## Trust model

The curated index is **operator-curated**, not cryptographically
verified. Listing a plugin here does NOT vouch for its security —
the daemon still routes every install through Phase 97.1's cosign
signature pipeline if `trusted_keys.toml` declares a policy.

The badge UI in `nexo-rs-plugin-admin` exposes this clearly:

- **`official`** (green) — owner is in the daemon's
  `trusted_keys.toml`. Index membership doesn't grant this; only
  the operator's local allowlist does.
- **`community_indexed`** (blue) — listed here but not in the
  operator's allowlist. Manifest fetched + parsed; reasonable
  baseline trust for community plugins.
- **`unverified`** (gray) — neither. The plugin came from
  crates.io or GitHub topic alone.

## Out of scope

- **Cryptographic signatures on index entries** — deferred until
  typosquat becomes a real concern. The current model relies on
  GitHub's commit identity for the index repo + cosign on
  individual plugin releases.
- **Stats** (install counts, ratings) — out of scope; rely on
  `cargo download-stats` for objective usage data.
- **Auto-update from upstream** — `index.json` is hand-edited via
  PR; no scraping job.

## License

MIT OR Apache-2.0 (matching the `nexo-rs` workspace).
