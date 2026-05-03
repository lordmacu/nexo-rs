# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-auth-v0.1.1...nexo-auth-v0.2.0) - 2026-05-03

### Added

- *(83.8.12.1)* empresa wire shapes + BindingContext + AgentConfig empresa_id
- *(83.1)* AgentConfig.extensions_config field + 2 YAML round-trip tests
- *(82.4.4)* EventSubscriberBinding schema + AgentConfig field
- *(config,driver-loop,main)* extract_memories boot wire (M4.a.b)
- deferred-schema filtering, cron jitter, MCP completion/complete, plan-mode pairing parser
- *(agents)* Cody — programming pair (Anthropic Claude Sonnet 4.5, English)
- *(config,core)* Phase 67.D.1 — DispatchPolicy on agent + per-binding override

### Fixed

- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain

### Other

- *(83.8.12.1.fix)* rename empresa → tenant for English code identifiers
- *(wip)* checkpoint mid-refactor + split microapp PHASES into dedicated file
- stabilize workspace: complete mcp/followup wiring and satisfy strict CI lints
- Phase 79.6 step 4: TeamPolicy + AgentConfig.team + fixture sweep
- Phase 79.10 step 3: ConfigToolPolicy + SUPPORTED_SETTINGS + AgentConfig.config_tool
- Phase 79.5 step 12: main.rs boot wiring + AgentConfig.lsp + fixture sweep
- pairing handshake + MCP HTTP transport + project-tracker state + browser CDP polish
- Phase 48.2: EmailCredentialStore in nexo-auth
- cargo fmt --all
- *(release)* per-crate independent versioning
