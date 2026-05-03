# nexo-ext-installer

Phase 31.1 — building block for `nexo plugin install <owner>/<repo>@<tag>`.

**Decentralized GitHub Releases architecture (Option B)**: there
is NO central catalog. Plugin authors publish to their own
GitHub repo as Releases, following the asset naming convention
below. The install CLI hits the GitHub Releases API directly to
resolve coords into a verified tarball.

## Plugin author publishing convention

Tag a release `v<version>` (e.g. `v0.2.0`) and attach these
assets:

| Asset | Required | Contents |
|-------|----------|----------|
| `nexo-plugin.toml` | ✅ | Manifest. CLI fetches first to learn `plugin.id` |
| `<id>-<version>-<target>.tar.gz` | ✅ | Binary + manifest tarball, one per supported target triple |
| `<id>-<version>-<target>.tar.gz.sha256` | ✅ | Single line of lowercase hex (64 chars) |
| `<id>-<version>-<target>.tar.gz.sig` | ⬜ | cosign signature (Phase 31.3 enforces) |
| `<id>-<version>-<target>.tar.gz.cert` | ⬜ | cosign certificate (Phase 31.3 enforces) |

**Target triples**: standard rust target triples
(`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, etc.).

## Operator install flow

```bash
nexo plugin install community-author/nexo-plugin-slack
# tag defaults to "latest"

nexo plugin install community-author/nexo-plugin-slack@v0.2.0
# specific tag

# Override target detection (cross-target install for testing):
NEXO_INSTALL_TARGET=aarch64-apple-darwin nexo plugin install ...
```

Behind the scenes:

1. Parse `<owner>/<repo>@<tag>` coords
2. `GET https://api.github.com/repos/<owner>/<repo>/releases/tags/<tag>`
   (or `/latest`)
3. Find `nexo-plugin.toml` asset → download → parse → extract
   `plugin.id`
4. Find `<id>-<version>-<target>.tar.gz` asset → download
   matching `.sha256` asset
5. Stream-download the tarball, computing sha256
6. Compare computed sha256 vs the `.sha256` asset's body —
   reject + cleanup on mismatch
7. (Phase 31.3) verify cosign signature against
   `config/extensions/trusted_keys.toml`
8. (Phase 31.1.b) extract the tarball into the daemon's
   `plugins.discovery.search_paths`

## What's shipped vs deferred

- ✅ **31.0** — `nexo-ext-registry` types crate (`ExtEntry`,
  `ExtDownload`, `ExtSigning`, `ExtTier`).
- ✅ **31.1** — this crate. Resolver + downloader + sha256
  verifier hitting GitHub Releases API.
- ✅ **31.1.b** — `extract_verified_tarball` lays the verified
  tarball into `<dest_root>/<id>-<version>/` with staging +
  atomic rename, path-safety validation, entry-type whitelist
  (regular files + dirs only — symlinks/hardlinks/special-files
  rejected), size budgets, manifest re-validation, and
  idempotent re-install.
- ⬜ **31.1.c** — `Mode::PluginInstall` CLI integration in
  main.rs.
- ⬜ **31.2** — per-plugin CI publish workflow template
  (GitHub Actions workflow that builds tarballs + sha256s +
  cosign signs + creates the Release).
- ⬜ **31.3** — cosign verification + per-author trust policy
  in `config/extensions/trusted_keys.toml`.
- ⬜ **31.6** — `nexo plugin new --lang rust|python|ts`
  scaffolder.

See `proyecto/PHASES-curated.md` § Phase 31 for the full plan.

## Why decentralized

- **Zero infrastructure**: nexo-rs maintainers don't run a
  catalog server, GitHub Pages, or CDN. Plugin authors host
  their own binaries on GitHub Releases (free, up to 2GB/asset).
- **No gatekeeping**: plugin authors publish on their own
  cadence without waiting for a maintainer to merge a PR to a
  shared catalog.
- **Operator chooses trust**: `tier: community` for everything;
  operator allowlists per-author cosign keys in
  `config/extensions/trusted_keys.toml` (Phase 31.3) to upgrade
  specific plugins to a `verified` policy.
- **Discoverable**: GitHub topic search (`topic:nexo-plugin`)
  surfaces plugins; future `nexo plugin search` subcommand
  (deferred) wraps that.

## Tests

```bash
cargo test -p nexo-ext-installer
```

21 tests:

- **Resolver / downloader (8)** — coords parsing, GitHub API URL
  construction, release shape validation (missing manifest,
  missing target tarball), happy-path round-trip with sha256
  verification, sha256 mismatch cleanup.
- **Extraction (13)** — happy path with binary chmod check,
  idempotent re-install skip, manifest mismatch + staging
  cleanup, path traversal via `..`, absolute-path injection,
  symlink rejection, entry count limit, missing binary, plus
  helper-level coverage of `validate_entry_path` and
  `cleanup_stale_staging`.
