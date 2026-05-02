# Changelog — `nexo-microapp-sdk`

All notable changes to the SDK crate are documented here per
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The
crate follows [Semantic Versioning](https://semver.org) **once
`v1.0.0` ships**. Until then breaking changes may land between
0.x releases — see CHANGELOG entries for the migration.

## [Unreleased]

### Added

- `MockBindingContext` fluent builder under feature
  `test-harness` (Phase 83.15) — chain
  `with_agent / with_channel / with_account / with_session /
  with_mcp_channel_source` for cleaner per-binding test setup.
  Renders `binding_id` automatically.
- `HookOutcome::{Block, Transform}` (Phase 83.3) — vote-to-
  block / vote-to-transform decisions for the daemon's hook
  interceptor. Anti-loop signal `do_not_reply_again`. Legacy
  `Abort { reason }` still works and serialises as a `block`
  decision on the wire for back-compat with pre-83.3 daemons.

## [0.1.0] — 2026-04-15

### Added

- Phase 83.4 initial release: `Microapp` builder, `ToolHandler`
  / `HookHandler` traits, `ToolCtx` / `HookCtx`,
  `OutboundDispatcher` (feature `outbound`), `AdminClient`
  (feature `admin`), `MicroappTestHarness` (feature
  `test-harness`), `init_logging_from_env`.
- Phase 82.5 inbound metadata accessors (`ToolCtx::inbound`,
  `HookCtx::inbound`).
- Phase 82.10 admin RPC client surface (capability-gated).
- Phase 82.11 transcript firehose subscription helper.
- Phase 82.12 HTTP server token-rotation handler.
