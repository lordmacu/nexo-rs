# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-mcp-v0.1.1...nexo-mcp-v0.2.0) - 2026-05-03

### Added

- *(channels)* permission relay end-to-end + public docs
- *(channels)* rate limit + per-binding tool granularity (Phase 80.9 closed)
- *(mcp,main)* SIGHUP reload trigger for nexo mcp-server expose_tools (M1.b)
- *(mcp-audit)* wire args_hash + args_size in tools/call dispatch (M2 step 3)
- *(mcp-audit)* add args_hash + args_size helper module (M2 step 1-2)
- deferred-schema filtering, cron jitter, MCP completion/complete, plan-mode pairing parser

### Fixed

- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain

### Other

- *(wip)* checkpoint mid-refactor + split microapp PHASES into dedicated file
- *(mcp-audit)* integration tests for args_hash + args_size end-to-end (M2 step 4)
- fix workspace clippy/build regressions and docker context
- sync all local changes
- stabilize workspace: complete mcp/followup wiring and satisfy strict CI lints
- harden denied overrides and pass per-call session context
- CI fix: silence clippy on in-flux Phase 76 + 79 + 48 scaffolding
- CI fix: silence dead-code + unused-assignment warnings under -D warnings
- Phase 76 MCP server hardening: HTTP transport, auth, multi-tenancy, rate-limit, telemetry
- Phase 27.1: cargo-dist baseline + bundled WIP
- *(crates)* expand 6 more READMEs (setup, taskflow, config, mcp, memory, broker)
- *(release)* per-crate independent versioning
