# Changelog

All notable changes to this project are documented here. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project adheres to [Semantic Versioning](https://semver.org)
**once `v1.0.0` is tagged**. Until then breaking changes may land on
`main` between any two commits; see the commit history for detail.

## [Unreleased]

### Added

- Native / no-Docker install path: `docs/src/getting-started/install-native.md` +
  idempotent `scripts/bootstrap.sh` (Linux, macOS, Termux).
- Termux (Android) support:
  - Dedicated install guide `docs/src/getting-started/install-termux.md`
    with root-vs-non-root breakdown.
  - `bootstrap.sh` detects `$TERMUX_VERSION` / `$PREFIX` and branches:
    `pkg install rust`, `$PREFIX/bin`, defaults NATS to `skip` with
    a hint toward `broker.type: local`.
- `BrowserConfig.args: Vec<String>` forwards extra CLI flags to the
  spawned Chrome/Chromium (enables `--no-sandbox` etc. for Termux).
- Repository chrome: `SECURITY.md`, `CODE_OF_CONDUCT.md`,
  `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.{md,yml}`,
  `.github/PULL_REQUEST_TEMPLATE.md`.
- Documentation site (mdBook) published at
  <https://lordmacu.github.io/nexo-rs/> with every subsystem
  documented, Mermaid diagrams, 9 ADRs under `docs/src/adr/`, and
  5 end-to-end recipes under `docs/src/recipes/`.
- Pre-commit docs-sync gate in `.githooks/pre-commit` rejects
  production-file changes without accompanying `docs/` edits unless
  the commit message includes `[no-docs]`.
- CI: `.github/workflows/docs.yml` builds mdBook + rustdoc and
  deploys to GitHub Pages; broken local-link scan.

### Changed

- Dual-licensed `MIT OR Apache-2.0` with an enforceable `NOTICE`
  attribution block (ADR 0009).
- `README.md` rewritten with badges and deep links into the
  published documentation.

### Deprecated

_Nothing yet._

### Removed

_Nothing yet._

### Fixed

- Setup wizard no longer hardcodes a shared `whatsapp.session_dir`
  — the writer derives a per-agent path when the YAML field is
  empty, avoiding cross-agent session collisions.
- Extension tools are gated on `Requires::missing()`: if declared
  `bins` / `env` aren't available, the extension is skipped with a
  warn log instead of registering tools that fail every call.

### Security

- `SECURITY.md` formalizes the private disclosure channel
  (<informacion@cristiangarcia.co>) and sets expected response SLAs.

---

## [0.1.0] — 2026-04-24 (initial public release)

First public cut of the codebase. All 16 internal development
phases complete (120/120 sub-phases in `PHASES.md`). No backward-
compatibility commitments yet — treat the public surface as unstable
until `v1.0.0`.

<!-- Link definitions:
[Unreleased]: https://github.com/lordmacu/nexo-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lordmacu/nexo-rs/releases/tag/v0.1.0
-->
