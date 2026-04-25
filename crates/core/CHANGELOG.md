# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-core-v0.1.1) - 2026-04-25

### Added

- *(pairing)* channel-adapter registry + per-channel reply delivery
- *(core)* wire pairing gate into runtime intake
- *(core+bin)* wire pairing — config field, policy, CLI
- *(core)* wire web_search tool + per-agent/per-binding policy
- *(core/link-understanding)* fetch + extract URLs into prompt context
- *(config)* per-agent context_optimization override
- *(core/compaction)* online history compactor + tool result truncator
- *(core/llm)* pre-flight token counting + circuit-broken cascade
- *(core/llm-behavior)* wire prompt cache + workspace cache into run_turn
- *(core/workspace)* in-memory bundle cache with notify invalidation
- *(llm/context-opt)* foundation types for prompt cache + compaction
- *(admin/channels)* Telegram edit (PATCH) + delete [no-docs]
- *(poller-tools)* LLM tools — pollers_{list,show,run,pause,resume,reset}
- *(config)* per-agent + per-binding output language directive
- *(core)* emit events.runtime.config.reloaded after each successful reload
- *(core)* intake reads from snapshot.load() — hot-reload now takes effect
- *(auth)* per-(channel,instance) circuit breakers
- *(core)* ConfigReloadCoordinator — hot-swap of existing agents
- *(core)* ReloadCommand channel + apply handler in AgentRuntime
- *(core)* debounced config file watcher for Phase 18
- *(core)* telemetry primitives for Phase 18 hot-reload
- *(core)* AgentRuntime owns an ArcSwap<RuntimeSnapshot>
- *(core)* RuntimeSnapshot — immutable per-agent reloadable state
- *(auth)* phase 17 — runtime integration
- *(auth)* phase 17 scaffold — per-agent credential framework
- *(core)* wire ToolRegistryCache into runtime intake
- *(boot)* validate model.provider against the LLM registry
- *(core)* aggregate binding validation + wildcard overlap warn
- *(core)* enforce per-binding allowed_tools at LLM turn + execution
- *(plugins,core)* outbound + delegation read effective policy
- *(core)* prompt, skills, and allowed_delegates read from effective policy
- *(core)* LLM model read from effective policy per binding
- *(core)* resolve EffectiveBindingPolicy at inbound intake
- *(core)* per-binding tool registry cache
- *(core)* AgentContext carries EffectiveBindingPolicy
- *(core)* binding_validate — boot-time checks for per-binding overrides
- *(core)* EffectiveBindingPolicy — resolve per-binding overrides
- *(config)* binding overrides — Option<> fields on InboundBinding
- Ana sales agent + per-agent outbound allowlist + setup polish
- *(setup)* guided wizard + google plugin extraction + inline pairing
- agent framework phases 1-14 — runtime, memory, LLMs, plugins, skills, taskflow
- *(1.1)* workspace scaffold — 9 crates, config YAMLs, cargo build clean

### Fixed

- *(ci)* cross arm64 jammy image + ignore 2 known concurrency-flake tests
- *(ci)* rustfmt one-liner + sort_by_key for clippy 1.95
- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain
- *(audit)* land 18 of 25 findings from AUDIT-2026-04-25-pass2
- *(audit)* land 16 of 36 findings from AUDIT-2026-04-25
- *(core)* sanitise output-language directive against newlines + bloat
- *(core)* hot-reload runs post-assembly tool-name validation
- *(cli)* bring BrokerHandle trait into scope + derive Deserialize on ReloadOutcome
- *(core)* make ToolRegistryCache::get_or_build atomic + review follow-ups

### Other

- *(release)* bump nexo-pairing + nexo-memory to 0.1.2; sync path-dep pins
- *(release)* per-crate independent versioning
- *(release)* bump workspace 0.1.0 → 0.1.1, add per-crate READMEs
- telemetry counters + histogram (W-1)
- link understanding telemetry (L-1)
- agent_* crates → nexo_*, agent bin → nexo
- hot-reload context_optimization flags via per-turn snapshot read
- clippy -D warnings pass on the whole workspace [no-docs]
- *(core)* unblock the two preexisting llm_behavior failures
- *(core)* add arc-swap + notify deps for Phase 18 hot-reload
- *(core)* binding_index is Option<usize>, not usize::MAX sentinel
- *(d5)* memory — short-term, long-term, vector
- *(d3)* LLM providers — minimax, anthropic, openai-compat, retry
- *(core)* lock down match_binding_index first-match semantics
- pre-resolve policies + skip boot prune for bound agents
- *(d1)* architecture section — overview, runtime, bus, fault tolerance
- pre-release prep — CI, dual license, ext gating, Ana→MiniMax
