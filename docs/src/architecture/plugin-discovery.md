# Plugin discovery — architecture

Phase 98 layer that sits *above* the Phase 97.1 install pipeline.
Discovery answers "what's available to install"; install answers
"go install this specific thing". The two are loosely coupled by a
single shared wire shape, [`PluginsInstallParams`].

## Flow

```text
                           Operator
                              │
              ┌───────────────┴────────────────┐
              ▼                                ▼
         CLI subcmd                       Admin UI tab
   agent plugin search          /m/plugins → Available
              │                                │
              │ direct call                    │ admin RPC
              ▼                                ▼
  ┌─────────────────────────┐    ┌────────────────────────────┐
  │ DefaultDiscoveryClient  │    │ nexo/admin/plugins/search  │
  │ (nexo-plugin-discovery) │◄───┤ DefaultDiscoveryAdapter    │
  └─────────────┬───────────┘    └────────────────────────────┘
                │
                │ cold path
                ▼
       ┌────────────────────┐
       │ DiskCache (24h)    │──── stale?
       └────────┬───────────┘     │
                │ miss            │
                ▼                 │
       ┌──────────────────────┐   │
       │ sources::run_all     │   │
       │  ├── CratesIoSource  │   │
       │  ├── GithubTopic..   │   │
       │  └── CuratedIndex..  │   │
       └────────┬─────────────┘   │
                │                 │
                ▼                 │
       ┌──────────────────────┐   │
       │ manifest_fetcher     │   │
       │ (raw nexo-plugin.toml│   │
       │  per item, parallel) │   │
       └────────┬─────────────┘   │
                ▼                 │
       ┌──────────────────────┐   │
       │ compat + category    │   │
       │ derivation per item  │   │
       └────────┬─────────────┘   │
                ▼                 │
       ┌──────────────────────┐   │
       │ merge: dedup by name │   │
       │ + trust promotion    │   │
       └────────┬─────────────┘   │
                ▼                 │
       ┌──────────────────────┐   │
       │ write_atomic cache   │◄──┘
       └────────┬─────────────┘
                ▼
        Vec<DiscoveredPlugin>
```

## Why three sources

Each source compensates for the others' blind spots:

- **crates.io** is exhaustive but lacks curation — every yanked /
  abandoned `nexo-plugin-*` shows up. The manifest fetch + compat
  gate filters the noise client-side.
- **GitHub topic** catches plugins not yet published to crates.io
  (pre-release tags, forks, internal mirrors with the topic
  applied). Authors opt in explicitly by adding the topic.
- **Curated index** is the operator-curated trust signal. Listing
  here doesn't vouch for security — Phase 97.1's cosign pipeline
  still gates the install — but it gives the UI a `CommunityIndexed`
  badge tier above raw `Unverified`.

## Source order matters

`build_sources()` orders the sources `CuratedIndex → CratesIo →
GithubTopic`. The merge step uses "first non-None wins" for
metadata tiebreakers; with curated first, an explicit
`manifest_url` from `index.json` wins over the hardcoded
`raw.githubusercontent.com/.../main/...` that `GithubTopicSource`
constructs from a topic search.

Without this ordering, plugins that ship under a non-default
branch (`master`) silently fail the manifest fetch because the
github_topic source assumed `main`.

## Concurrency invariants

- **Source-level**: each source runs its HTTP fetch in a separate
  `tokio` task wrapped in a per-source circuit breaker (3 failures
  → 60s backoff → exponential up to 600s). One slow source NEVER
  stalls the others — `futures::future::join_all` lets faster
  sources land first.
- **Manifest-level**: per-item manifest fetches also fan out via
  `join_all` so the cold path's wall-clock stays bounded by the
  slowest single manifest, not the sum.
- **Cache-level**: writer uses `fs::write(tmp) + fs::rename(tmp,
  final)` so readers either see the previous snapshot or the new
  one — never a half-written file. Parse failures on read surface
  as cache miss (no error), so a malformed cache file degrades
  gracefully instead of bricking the catalogue.

## Trust tier resolution

```text
              ┌──────────────────────────┐
              │ Owner ∈ official_owners? │
              └────────────┬─────────────┘
                  yes      │     no
              ┌────────────┘
              ▼
       TrustTier::Official
                            ┌──────────────────────────────┐
                            │ Name ∈ curated_index? OR     │
                            │ source includes CuratedIndex │
                            └────────────┬─────────────────┘
                                yes      │       no
                            ┌────────────┘
                            ▼
                  TrustTier::CommunityIndexed
                                              │
                                              ▼
                                  TrustTier::Unverified
```

`official_owners` defaults to `["lordmacu", "nexo-rs"]` but
production deployments override at boot. The daemon binary seeds
the allowlist from `TrustedKeysConfig.authors` in `trusted_keys.toml`
(Phase 97.1) so trust tier surfaces match what cosign actually
enforces at install time.

## Race conditions surfaced + fixed

Phase 98.3 wrote concurrent-install regression tests against
`extract_verified_tarball` (the Phase 97.1 install path) and
surfaced two real races:

1. **`cleanup_stale_staging` reaped in-flight neighbours** —
   unconditional `STAGING_PREFIX` cleanup deleted the other task's
   staging dir mid-extract. Fixed by age-gating to 1 hour
   (production) / `Duration::ZERO` (tests).
2. **`fs::rename` not atomic-replace for non-empty dirs** —
   concurrent winner populates `final_dir`, loser's rename fails
   with `ENOTEMPTY`. Fixed by race-tolerant rename: on failure,
   re-validate `final_dir/MANIFEST_FILE` against `expected`; if
   matches → loser returns `was_already_present=true`.

See the
[Phase 98 audit memo](./plugin-install-audit-2026-05-19.md) for
full details + a list of "already solid" patterns NOT to touch.

## Wire contracts

All discovery types live in `nexo-tool-meta::admin::plugin_discovery`
with `ts-export` derives so the admin frontend re-uses them
verbatim. Adding fields is backwards-compatible via
`#[serde(default)]`; removing them is a semver-major change.

`DiscoveredPlugin.install_params` carries a pre-filled
[`PluginsInstallParams`] (the Phase 97.1 install wire shape).
The admin UI threads this directly into the existing
`<InstallPluginModal>` — clicking Install on a catalogue card
opens the same modal as the header's "Install plugin" button,
just with the fields populated. Zero new install paths, zero
wire duplication.

## Out of scope

- **Hosting our own marketplace** — crates.io + GitHub + a curated
  index repo are enough; no shipping infrastructure.
- **Ratings / reviews** — relies on `cargo download-stats` for
  objective usage data.
- **Live push** — 24h polling is the only refresh; no SSE or
  webhook from the index repo.
- **Plugin signing per index entry** — deferred until typosquat
  becomes a real concern. Cosign on individual releases is
  Phase 97.1's job.

[`PluginsInstallParams`]: ../plugins/manifest-unified.md
