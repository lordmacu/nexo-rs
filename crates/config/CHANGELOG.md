# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-config-v0.1.1...nexo-config-v0.2.0) - 2026-05-03

### Added

- *(83.8.12.5)* LLM providers per-tenant — TenantLlmConfig + resolve_provider + build_for_tenant
- *(81.5)* NexoPluginRegistry filesystem discovery (library + tests)
- *(83.8.12.2)* tenants domain handler + capability + INVENTORY
- *(83.8.12.1)* empresa wire shapes + BindingContext + AgentConfig empresa_id
- *(83.1)* AgentConfig.extensions_config field + 2 YAML round-trip tests
- *(82.12.3)* allow_external_bind opt-in + boot validate
- *(82.10.b)* capability gates + audit log + INVENTORY
- *(36.2)* agent memory snapshot subsystem
- *(82.7.b+c)* per-binding tool_rate_limits config + resolver
- *(82.7.a)* unify ToolRateLimitSpec + add essential_deny_on_miss
- *(82.5.c)* event-subscriber yaml inbound_kind + propagation
- *(82.4.b.1)* EventSubscriber skeleton + id reserved-char validate
- *(82.4.4)* EventSubscriberBinding schema + AgentConfig field
- *(82.2.2)* AppConfig.webhook_receiver field + loader
- *(82.2.1)* WebhookServerConfig schema + validation
- *(channels)* rate limit + per-binding tool granularity (Phase 80.9 closed)
- *(config,main)* daemon-embed MCP HTTP server in Mode::Run (M1.b.c)
- *(config,driver-loop,main)* extract_memories boot wire (M4.a.b)
- *(config)* wire memory.secret_guard YAML key (C5 step 1)
- *(effective-policy)* per-binding override for lsp/team/config_tool/repl (C1)
- deferred-schema filtering, cron jitter, MCP completion/complete, plan-mode pairing parser
- Phase 77.2-77.6 + skills (autoCompact, sessionMemoryCompact, extractMemories, relevance scorer, bundled skills)
- *(setup)* per-agent wizard submenu + yaml_patch helpers
- *(config)* pairing.yaml schema + loader + boot wiring [PR-6 partial]
- *(config,core)* Phase 67.D.1 — DispatchPolicy on agent + per-binding override

### Fixed

- *(clippy)* pairing.rs doc list overindent
- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain

### Other

- *(83.8.12.1.fix)* rename empresa → tenant for English code identifiers
- *(wip)* checkpoint mid-refactor + split microapp PHASES into dedicated file
- *(config)* cover memory.secret_guard YAML round-trip (C5 step 3)
- fix workspace clippy/build regressions and docker context
- sync all local changes
- stabilize workspace: complete mcp/followup wiring and satisfy strict CI lints
- harden denied overrides and pass per-call session context
- Phase 79.6 step 4: TeamPolicy + AgentConfig.team + fixture sweep
- Phase 79.10 step 3: ConfigToolPolicy + SUPPORTED_SETTINGS + AgentConfig.config_tool
- Phase 79.5 step 12: main.rs boot wiring + AgentConfig.lsp + fixture sweep
- Phase 79.5 step 9: LspPolicy in nexo-config
- Phase 76 MCP server hardening: HTTP transport, auth, multi-tenancy, rate-limit, telemetry
- Phase 79.8: RemoteTrigger outbound publisher (webhook + NATS)
- Phase 79.1: EnterPlanMode + ExitPlanMode tools (MVP)
- Phase 48 audit #3 #3: bounce orphan + retention prune
- Phase 48 audit fixes: DLQ cap + attachment / parse error metrics
- Phase 48 follow-up #10: attachment ref-count + retention GC
- Phase 48.5.a: MIME envelope foundations (events, deps, config)
- Phase 48.1: email plugin scaffold + multi-account config
- *(config)* align path_resolution with workspace-relative extra_docs
- Phase 27.1: cargo-dist baseline + bundled WIP
- cargo fmt --all
- *(crates)* expand 6 more READMEs (setup, taskflow, config, mcp, memory, broker)
- *(release)* per-crate independent versioning
