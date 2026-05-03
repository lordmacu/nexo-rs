# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-memory-v0.1.2...nexo-memory-v0.2.0) - 2026-05-03

### Added

- *(86.1)* memory metrics module + 9 tests
- *(36.2)* agent memory snapshot subsystem
- deferred-schema filtering, cron jitter, MCP completion/complete, plan-mode pairing parser
- Phase 77.2-77.6 + skills (autoCompact, sessionMemoryCompact, extractMemories, relevance scorer, bundled skills)
- *(memory)* add secret scanner + guard for Phase 77.7

### Other

- fix workspace clippy/build regressions and docker context
- sync all local changes
- stabilize workspace: complete mcp/followup wiring and satisfy strict CI lints
- *(crates)* expand 6 more READMEs (setup, taskflow, config, mcp, memory, broker)
