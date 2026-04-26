# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-core-v0.1.1...nexo-core-v0.2.0) - 2026-04-26

### Added

- audit-before-done as code (HookAction::DispatchAudit + AuditChainer)
- operator interrupt + audit-before-done workflow
- self-modify gate so Cody can finish the nexo-rs roadmap
- *(core,project-tracker,agents)* Cody flows — preflight + workspace ops
- *(main,core)* in-process driver subsystem behind NEXO_DRIVER_INTEGRATED
- *(core)* web_fetch built-in tool [W-2]
- *(core)* PT-3 — DispatchTelemetry threaded through ProgramPhaseHandler + PT-9 NON_CHAT_ORIGIN_PLUGINS
- *(core)* PT-2 — runtime intake migrates to get_or_build_with_dispatch
- *(core)* PT-1 — ToolHandler adapters for the dispatch surface
- *(link-understanding)* readability-shaped boilerplate dropper [L-2]
- *(core)* Phase 67.H.3 — dispatch capability hot-reload via fresh ToolRegistryCache
- *(core,dispatch-tools)* Phase 67.D.3 — registry filters by DispatchPolicy
- *(config,core)* Phase 67.D.1 — DispatchPolicy on agent + per-binding override
- *(pairing)* wire telemetry counters PR-2 (Phase 26.y)

### Fixed

- B22+B23+B24 + comprehensive READMEs for programmer agent crates
- B17–B21 + S1/S3/S5 — audit pass cleanup
- B10 + B11 + B12 + B13 + B16 hardening pass
- B1..B7 + B9 — wiring del programador agente end-to-end

### Other

- *(crates)* expand 4 more READMEs (core, llm, pairing, extensions)
- *(core)* PT-8 — multi-agent dispatch e2e for handler + telemetry wiring
