# Plugin install pipeline audit — 2026-05-19

Read-only audit of the Phase 97.1 install / scan / uninstall machinery
ahead of Phase 98 (plugin discovery). Goal: surface real gaps vs already-
solid patterns so the discovery layer composes cleanly and any
gap-closing work targets actual defects.

## End-to-end flow

```
                       ┌──────────────────────────────┐
operator (CLI)         │ agent plugin install <coords>│
                       └──────────────┬───────────────┘
                                      │
                                      ▼
                       ┌──────────────────────────────┐
                       │ src/plugin_install.rs        │
                       │   run_plugin_install         │
                       │   └ install_plugin_silent    │◄────────────┐
                       └──────────────┬───────────────┘             │
                                      │                             │
                                      ▼                             │
operator (admin UI)    ┌──────────────────────────────┐             │
                       │ InstallPluginModal.tsx       │             │
                       └──────────────┬───────────────┘             │
                                      │                             │
                                      ▼                             │
                       ┌──────────────────────────────┐             │
                       │ admin RPC                    │             │
                       │   nexo/admin/plugins/install │             │
                       └──────────────┬───────────────┘             │
                                      │                             │
                                      ▼                             │
                       ┌──────────────────────────────┐             │
                       │ crates/core/.../             │             │
                       │   plugin_install::install_   │             │
                       │   plugin                     │             │
                       │   (validators + dispatch)    │             │
                       └──────────────┬───────────────┘             │
                                      │                             │
                                      ▼                             │
                       ┌──────────────────────────────┐             │
                       │ src/plugin_install_adapter   │             │
                       │   LivePluginInstallerCell    │             │
                       │   .install                   │             │
                       │   → install_release ─────────┼─────────────┘
                       │   → install_cargo (cargo CLI)│
                       └──────────────────────────────┘
                                      │
                                      ▼
                       ┌──────────────────────────────┐
                       │ crates/ext-installer         │
                       │   resolve_release            │
                       │   download_and_verify        │
                       │   verify_plugin_signature    │
                       │   extract_verified_tarball   │
                       └──────────────┬───────────────┘
                                      │
                                      ▼
                       ┌──────────────────────────────┐
                       │ broker event                 │
                       │   plugin.lifecycle.<id>.     │
                       │   installed                  │
                       └──────────────────────────────┘
                                      │
                                      ▼ (post-install auto-scan)
                       ┌──────────────────────────────┐
                       │ LivePluginInstaller.scan     │
                       │   discover + diff + spawn    │
                       │   + register in live runtime │
                       └──────────────────────────────┘
```

## Inventory

| Area | File | Responsibility |
|------|------|----------------|
| CLI entry | `src/main.rs` (cmd dispatch lines ~10120) | `agent plugin {install,list,upgrade,remove,new,run,help}` |
| CLI orchestration | `src/plugin_install.rs` | `run_plugin_install` (human/JSON) → `install_plugin_silent` (pure) |
| Admin wire | `crates/tool-meta/src/admin/plugin_install.rs` | `PluginsScanParams/Response`, `PluginsInstallParams/Response`, `PluginsUninstallParams/Response`, `InstallSource { Release, Cargo }` |
| Admin handler | `crates/core/src/agent/admin_rpc/domains/plugin_install.rs` | 3 verbs + `PluginInstaller` trait + tight char-class validators |
| Adapter | `src/plugin_install_adapter.rs` | `LivePluginInstallerCell` (OnceCell) → `LivePluginInstaller` (`install`/`scan`/`uninstall`) |
| Pure install lib | `crates/ext-installer/src/lib.rs` (+ 7 siblings) | `resolve_release`, `download_and_verify`, `verify_plugin_signature`, `extract_verified_tarball`, `AuthorPolicy`, `TrustMode`, `TrustedKeysConfig`, `RepoCoords` |
| Lifecycle event | `src/plugin_install.rs::emit_lifecycle_installed_event` | broker `plugin.lifecycle.<id>.installed` (NATS, soft-fail) |
| Frontend module | `nexo-rs-plugin-admin/frontend/src/modules/plugins/PluginsMain.tsx` | Loaded list + diagnostics + Install/Restart buttons |
| Frontend install UI | `…/plugins/InstallPluginModal.tsx` | Form: crate_name + version + repo + source + force |
| Frontend restart UI | `…/plugins/RestartPluginModal.tsx` | Restart single plugin |
| Frontend API | `…/api/plugin_doctor.ts`, `…/api/plugin_restart.ts` | RPC client wrappers |

## Strengths (already solid — no Phase 98 work needed)

- **Idempotency real**: `install_plugin_silent` surfaces
  `extracted.was_already_present`. `.nexo-install.json` write is guarded
  by `if !was_already_present` so `installed_at` doesn't bump on no-op
  reruns (`plugin_install.rs:538-556`). CLI prints `✓ already installed at
  &lt;dir&gt; — nothing to do`.
- **Char-class validators at protocol boundary**:
  `validate_crate_name` / `validate_version` / `validate_repo_slug` reject
  shell / URL injection before any subprocess spawn or HTTP request
  (`plugin_install.rs#mod plugin_install::validators`,
  `admin_rpc/domains/plugin_install.rs:148-200`).
- **Typed error catalogue**: `InstallError`, `ExtractError`, `VerifyError`
  each have a `*_kind() -> &'static str` mapping so admin RPC + CLI emit
  the same string keys (`plugin_install.rs:79-125`). Admin UI / CLI never
  scrape free-form text.
- **Deferred-init pattern** in adapter: `LivePluginInstallerCell` uses
  `tokio::sync::OnceCell`. RPC calls before main.rs threads the runtime
  fixtures get a clean `"plugin installer not yet populated (daemon still
  booting)"` bail. Handler maps this to `InvalidParams` (retryable) not
  `Internal` (`admin_rpc/domains/plugin_install.rs:102-106`).
- **Auto-scan after install** closes the lifecycle loop: install success
  → `scan` runs → fresh plugin handle registered in live runtime (no
  daemon restart required). Soft-failure path: if scan errors after
  install, install RPC still succeeds + operator gets a tracing warn
  (`plugin_install_adapter.rs:439-450`).
- **Cosign sidecar cleanup on every failure branch**: signature / cert /
  bundle files are removed in *every* error path of the verify
  flow — no orphan files in the tarball cache
  (`plugin_install.rs:418-509`).
- **Trust-mode resolution layers correctly**: `resolve_effective_mode`
  combines `[[authors]]` policy + CLI `--require-signature` /
  `--skip-signature-verify` flags. Mutually exclusive combo rejected
  at boundary (`plugin_install.rs:324-332`).
- **Stale detection in scan**: handles present in `plugin_handles` but
  absent from discovery walk surface as `stale: Vec<String>` without
  being auto-uninstalled — that's `uninstall`'s job
  (`plugin_install_adapter.rs:340-345`).
- **Cargo output clip**: 16 KiB cap on cargo stdout/stderr in admin RPC
  reply keeps JSON payload sane on `--verbose` builds
  (`plugin_install_adapter.rs:49, 255-256`).
- **UX hints on CLI errors**: rate-limit hint (`NEXO_GITHUB_TOKEN`),
  target-not-found hint (`--target=…`), each surfaced when the matching
  error kind appears (`plugin_install.rs:653-664`).

## Real gaps (Phase 98 closes)

### Gap 1 — No catalogue browse

`InstallPluginModal.tsx:46-48` regex-validates `crate_name`, `version`,
`repo` but offers no autocomplete or pre-fill. Operator must know the
exact crate name and version ahead of time. No way to discover
`nexo-plugin-telegram` exists without out-of-band Googling.

Phase 98 sub-phases 98.5–98.11 (discovery backend) + 98.13 (pre-fill
modal from card click) close this.

### Gap 2 — No pre-install compat preview

Manifest compat is validated inside `extract_verified_tarball` (extract
step rejects manifest mismatches). Operator only learns "incompatible
SDK version" after download + sha256 + cosign verify + extract — wasted
work for a doomed install.

Phase 98 sub-phase 98.8 (`compat.rs` semver check via fetched
`nexo-plugin.toml` BEFORE install) closes this.

### Gap 3 — Trust tier hidden from admin UI

`trusted_keys.toml` machinery is robust internally (`AuthorPolicy`,
`TrustMode { ignore, warn, require }`, identity regex, OIDC issuer) but
the admin UI surfaces zero signal about "is this plugin signed by the
official Nexo team?". `PluginsInstallResponse.signature_verified` /
`signature_identity` / `signature_issuer` are emitted by the silent
installer (`plugin_install.rs:577-583`) but `LivePluginInstaller::
install_release` only forwards `crate_name + version + spawned + cargo_*`
to admin (`plugin_install_adapter.rs:210-216`). Verified signature
metadata is dropped on the floor.

Phase 98 sub-phase 98.13 (`<TrustBadge>` on card) + audit follow-up:
extend `PluginsInstallResponse` to forward the signature fields. Logged
as **Gap 3.1** below.

### Gap 4 — Admin RPC can't request stricter trust

CLI accepts `--require-signature` and `--skip-signature-verify`. Admin
RPC `PluginsInstallParams` has neither field — admin always uses the
resolved `trusted_keys.toml` policy. Use-case where this matters: SaaS
operator wants "require signed installs even for plugins not yet in
`[[authors]]`" but can't say so via API.

Phase 98 sub-phase 98.4 (trust mode parity) closes this — add
`require_signature: bool` to `PluginsInstallParams`.

### Gap 5 — No per-source health surfacing in UI

Operator can't see "GitHub rate-limited, crates.io OK, index repo 404".
Discovery response will need a `partial_failures: Vec<SourceError>` field;
UI needs a banner.

Phase 98 sub-phase 98.14 (`PartialFailureBanner`) closes this.

### Gap 6 — No curated index repo

Operator typing `nexo-plugin-...` into the install modal has no list to
pick from. Phase 98 sub-phase 98.16 creates
`lordmacu/nexo-plugin-index` with the 6 shipped plugins (telegram,
whatsapp, email, google, web-search, browser).

## Soft improvements (defer / nice-to-have)

| Improvement | Why defer |
|-------------|-----------|
| Boot-window wait-with-timeout instead of immediate bail | Current bail message is clean + retryable. Wait pattern adds 5s of dead time on boot races that are rare in practice. **Reframe** Phase 98 sub-phase 98.2: hold scope to *adding* a metric `plugin_install_boot_race_total` so we can measure frequency before changing behavior. |
| Concurrent install lock by `crate_name` | Race-window between two parallel installs of same `crate@version`: both download tarball, both call `extract_verified_tarball`. Extract destination is shared (`plugins.discovery.search_paths[0]/<plugin-id>/`). Second extract fails or overwrites; no corruption observed since extract is atomic-rename internally. Add regression test (98.3) before adding mutex — verify the race actually misbehaves. |
| Trust enforcement asymmetry CLI ↔ admin | Phase 98 sub-phase 98.4 surfaces a `trust_enforcement: "policy_applied" \| "cargo_skipped" \| "user_skip"` field in install response. Pure additive — doesn't change existing behavior. |

## Out-of-scope clean patterns (DO NOT TOUCH)

- `LivePluginInstallerCell` deferred-init via OnceCell — correct pattern,
  the bail UX is fine.
- `resolve_effective_mode` trust resolver — comprehensive + tested.
- `parse_cargo_installed_version` regex — tested, handles real-world
  cargo output.
- `clip_utf8` tail-with-marker — already pretty-prints clipped output.
- Validators (`validate_crate_name` / `validate_version` /
  `validate_repo_slug`) — char classes are tight enough; adding
  permissive chars opens injection vectors.

## Open follow-ups derived from this audit

These four are logged in `FOLLOWUPS.md` § Phase 98 deferreds when the
phase row is added.

1. **Gap 3.1** — extend `PluginsInstallResponse` with
   `signature_verified`, `signature_identity`, `signature_issuer`,
   `trust_mode`, `trust_policy_matched` fields so the admin UI can
   render a "signed by `<identity>`" footer in the install-success modal.
   Pure additive wire change.
2. **Soft improvement** — `plugin_install_boot_race_total` metric before
   altering bail behavior.
3. **Soft improvement** — concurrent-install regression test (98.3
   covers); if test passes today → no mutex needed.
4. **Soft improvement** — emit `trust_enforcement` discriminator field
   in install response (98.4).

## Cross-references

- Phase 97.1 ship notes: `proyecto/FOLLOWUPS.md` § Phase 97 (in progress).
- Phase 98 sub-phase breakdown: `proyecto/PHASE-98-PLUGIN-DISCOVERY-SUBPHASES.md`.
- Phase 81.33.b.real plugin auto-discovery design (manifest sections):
  `docs/src/architecture/plugin-auto-discovery.md`.
- Phase 93 decoupling audit (sibling format): `docs/src/architecture/
  phase-93-decoupling-audit.md`.
