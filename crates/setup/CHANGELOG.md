# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-setup-v0.1.1...nexo-setup-v0.2.0) - 2026-05-03

### Added

- *(82.10.l)* admin/llm_providers/probe — daemon-side LLM provider reachability check
- *(82.10.k)* nexo/admin/secrets/write — atomic secret persist + std::env::set_var injection
- *(82.10.h.b.b.collect)* admin_capability_collect helpers
- *(82.11.log.compose)* boot-side Tee composition for durable firehose
- *(82.10.h.b.pairing)* wire pairing notifier into admin_bootstrap
- *(83.8.12.6.runtime+.b)* SkillLoader fallback chain + on-disk migration
- *(83.8.12.4.b)* tenant-aware firehose + escalations handler filter
- *(83.8.12.6)* skills per-tenant layout — <root>/{__global__,<tenant_id>}/<name>/SKILL.md
- *(83.8.12.5.c.b)* tenant-scoped LLM provider yaml writer + drop the not_implemented gates
- *(82.13.c.2)* boot shares ProcessingControlStore between admin RPC + runtime
- *(82.13.b.3.1)* ProcessingControlStore push_pending/drain_pending + cap policy + drop event
- *(82.13.b.1.3)* TranscriptWriterAppender adapter + SDK + boot wire + docs
- *(83.8.12.4)* tenant_id filter wire shapes + agents handler enforcement
- *(83.8.12.3)* TenantsYamlPatcher production adapter
- *(83.8.12.2)* tenants domain handler + capability + INVENTORY
- *(83.8.4.b.b)* TelegramTranslator + EmailTranslator
- *(83.8.4.b.3)* integration test + docs + close-out
- *(83.8.4.b.2)* boot wire — AdminBootstrapInputs.broker
- *(83.8.4.b.1)* BrokerOutboundDispatcher + WhatsAppTranslator
- *(83.8.3)* FsSkillsStore production adapter
- *(83.8.2)* admin/skills domain handler + capability + INVENTORY
- *(82.14.2)* EscalationStore + handlers + auto-resolve hook
- *(82.13.2)* processing domain + InMemory store + dispatcher routing
- *(82.12.4)* NEXO_MICROAPP_HTTP_SERVERS_ENABLED + token_hash helper
- *(82.12.3)* allow_external_bind opt-in + boot validate
- *(82.12.2)* HttpServerSupervisor (boot probe + monitor loop)
- *(82.11.5)* firehose subscribe wire + INVENTORY + capability gate
- *(82.11.4)* AgentEventEmitter + broadcast firehose hook
- *(82.11.3)* TranscriptReaderFs production adapter
- *(82.10.h.b.5)* main.rs admin RPC bootstrap wire-path
- *(82.10.h.b.2)* StdioPairingNotifier + json_rpc_notification helper
- *(82.10.h.b.1)* InMemoryPairingChallengeStore adapter
- *(82.10.h.3)* production admin RPC adapters in nexo-setup
- *(82.10.b)* capability gates + audit log + INVENTORY
- *(36.2)* agent memory snapshot subsystem
- *(config,driver-loop,main)* extract_memories boot wire (M4.a.b)
- *(llm)* LlmError::QuotaExceeded + 4-provider plumb + last-quota cache (C4.c)
- *(capabilities)* add 3 INVENTORY entries + provider-agnostic doc-comment (C3 step 4-5)
- *(capabilities)* add CargoFeature toggle kind (C3 step 1-3)
- Phase 77.2-77.6 + skills (autoCompact, sessionMemoryCompact, extractMemories, relevance scorer, bundled skills)
- *(setup)* per-agent wizard submenu + yaml_patch helpers
- *(setup)* linear channel link flow (canal → agente → reauth/vincular)
- *(setup)* channel dashboard inside `nexo setup` step 3
- *(setup)* web-search wizard entry [W-3]
- *(project-tracker)* Phase 67.A.5 — config YAML + capabilities entry

### Fixed

- *(82.14.b+83.8.2.b)* wire skills + escalations into admin_bootstrap
- *(83.8.12.2.b)* wire tenants domain into admin RPC dispatcher
- *(clippy)* clear all -D warnings across workspace
- *(setup)* single-shot link flow with optional reauth + telegram chat-link
- *(setup)* wizard enumerates agents.d/ drop-ins, not just agents.yaml

### Other

- *(capabilities)* add drift-prevention test for env-toggle inventory (C3 step 6-7)
- stabilize workspace: complete mcp/followup wiring and satisfy strict CI lints
- Phase 79.10.b: Config tool — full main.rs registration via setup bridge
- Phase 79.10 step 2: YamlPatch + apply_patch_with_denylist
- Phase 79.10 step 1: ConfigTool denylist + globset workspace dep
- Phase 48 email follow-up: use-case sweep across plugin + tools + setup
- Phase 48 setup wizard: plain TLS guard + managed-provider password warn
- Phase 48 setup wizard: SPF/DKIM probe + Google account picker
- Phase 48 setup wizard UX polish: menu loop + connectivity probe
- Phase 48 follow-up: interactive setup wizard
- Phase 48.10: close Phase 48 — main.rs wiring + capabilities + docs
- Phase 48.2: EmailCredentialStore in nexo-auth
- Phase 27.1: cargo-dist baseline + bundled WIP
- Phase 70: pairing/dispatch DX cleanup
- cargo fmt --all
- *(crates)* expand 6 more READMEs (setup, taskflow, config, mcp, memory, broker)
